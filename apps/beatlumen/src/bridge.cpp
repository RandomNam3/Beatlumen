#include "bridge.h"
#include "audio.h"

#include <QClipboard>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocale>
#include <QOpenGLContext>
#include <QOpenGLFunctions>
#include <QPalette>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QStyleHints>

#ifdef Q_OS_ANDROID
#include <QCoreApplication>
#include <QJniObject>
#include <QtCore/private/qandroidextras_p.h>
#endif

#ifndef GL_FRAMEBUFFER_BINDING
#define GL_FRAMEBUFFER_BINDING 0x8CA6
#endif

namespace {

QByteArray takeString(char *s)
{
    if (!s)
        return {};
    QByteArray out(s);
    bl_string_free(s);
    return out;
}

const void *getProc(void *ctx, const char *name)
{
    return reinterpret_cast<const void *>(static_cast<QOpenGLContext *>(ctx)->getProcAddress(name));
}

void logCallback(void *user, const char *msg)
{
    static_cast<Bridge *>(user)->log(QString::fromUtf8(msg));
}

uint32_t audioStartCallback(void *user)
{
    Q_UNUSED(user);
    return AudioOutput::instance()->start();
}

} // namespace

Bridge::Bridge(const QStringList &args, QObject *parent)
    : QObject(parent)
{
    const QString dataDir = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    QDir().mkpath(dataDir);
    QJsonArray jargs;
    for (const QString &a : args)
        jargs.append(a);
    const QByteArray argsJson = QJsonDocument(jargs).toJson(QJsonDocument::Compact);
    const QByteArray lang = QLocale::system().name().left(2).toUtf8();

    BlCallbacks cb{};
    cb.user = this;
    cb.log = logCallback;
    AudioOutput::configure(cb);
    if (!cb.audio_push)
        cb.audio_start = audioStartCallback;
    m_core = bl_core_new(dataDir.toUtf8().constData(), argsJson.constData(), lang.constData(), platform().toUtf8().constData(), cb);

    connect(&m_timer, &QTimer::timeout, this, &Bridge::tick);
    m_timer.setTimerType(Qt::PreciseTimer);
    m_timer.start(16);
    connect(QGuiApplication::styleHints(), &QStyleHints::colorSchemeChanged, this, &Bridge::systemThemeChanged);
    connect(qGuiApp, &QGuiApplication::paletteChanged, this, &Bridge::systemThemeChanged);
    connect(qGuiApp, &QGuiApplication::applicationStateChanged, this, [this](Qt::ApplicationState s) {
        if (s != Qt::ApplicationActive && m_core)
            bl_core_save(m_core);
    });
    tick();
}

Bridge::~Bridge()
{
    if (m_core) {
        BlCore *c = m_core;
        m_core = nullptr;
        bl_core_free(c);
    }
    AudioOutput::instance()->stop();
}

void Bridge::log(const QString &msg)
{
    qInfo().noquote() << msg;
}

QString Bridge::platform() const
{
#if defined(Q_OS_ANDROID)
    return QStringLiteral("android");
#elif defined(Q_OS_WASM)
    return QStringLiteral("web");
#elif defined(Q_OS_WIN)
    return QStringLiteral("windows");
#elif defined(Q_OS_MACOS)
    return QStringLiteral("macos");
#else
    return QStringLiteral("linux");
#endif
}

bool Bridge::mobile() const
{
#if defined(Q_OS_ANDROID) || defined(Q_OS_IOS)
    return true;
#else
    return false;
#endif
}

QString Bridge::version() const
{
    return QCoreApplication::applicationVersion();
}

bool Bridge::systemDark() const
{
    const auto scheme = QGuiApplication::styleHints()->colorScheme();
    if (scheme == Qt::ColorScheme::Unknown)
        return true;
    return scheme == Qt::ColorScheme::Dark;
}

QColor Bridge::systemAccent() const
{
#ifdef Q_OS_ANDROID
    // Material You: system_accent1_500 (API 31+).
    if (QNativeInterface::QAndroidApplication::sdkVersion() >= 31) {
        QJniObject ctx = QNativeInterface::QAndroidApplication::context();
        QJniObject res = ctx.callObjectMethod("getResources", "()Landroid/content/res/Resources;");
        const jint id = res.callMethod<jint>("getIdentifier", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                                              QJniObject::fromString(systemDark() ? "system_accent1_200" : "system_accent1_600").object<jstring>(),
                                              QJniObject::fromString("color").object<jstring>(), QJniObject::fromString("android").object<jstring>());
        if (id != 0) {
            const jint argb = ctx.callMethod<jint>("getColor", "(I)I", id);
            return QColor::fromRgba(static_cast<QRgb>(argb));
        }
    }
#endif
    const QColor c = QGuiApplication::palette().color(QPalette::Accent);
    return c.isValid() ? c : QColor(0xe9, 0x1e, 0x63);
}

QVariant Bridge::call(const QString &cmd, const QVariantMap &args)
{
    if (!m_core)
        return {};
    QJsonObject o = QJsonObject::fromVariantMap(args);
    o.insert(QStringLiteral("cmd"), cmd);
    const QByteArray req = QJsonDocument(o).toJson(QJsonDocument::Compact);
    const QByteArray res = takeString(bl_core_command(m_core, req.constData()));
    // Commands often change state; deliver it without waiting for the timer.
    QMetaObject::invokeMethod(this, &Bridge::tick, Qt::QueuedConnection);
    const QJsonDocument doc = QJsonDocument::fromJson("[" + res + "]");
    return doc.array().at(0).toVariant();
}

void Bridge::tick()
{
    if (!m_core)
        return;
    handleEvents(takeString(bl_core_tick(m_core)));
    const double p = bl_core_position(m_core);
    if (p != m_position) {
        m_position = p;
        emit positionChanged();
    }
#ifdef Q_OS_ANDROID
    const bool playing = bl_core_is_playing(m_core);
    if (playing != m_keepScreenOn)
        setKeepScreenOn(playing);
#endif
}

void Bridge::handleEvents(const QByteArray &json)
{
    if (json.isEmpty() || json == "[]")
        return;
    const QJsonArray events = QJsonDocument::fromJson(json).array();
    for (const QJsonValue &v : events) {
        const QJsonObject e = v.toObject();
        const QString type = e.value(QStringLiteral("type")).toString();
        if (type == QLatin1String("state")) {
            m_state = e.value(QStringLiteral("state")).toObject().toVariantMap();
            emit stateChanged();
        } else if (type == QLatin1String("toast")) {
            emit toast(e.value(QStringLiteral("text")).toString(), e.value(QStringLiteral("error")).toBool());
        } else if (type == QLatin1String("cover")) {
            emit coverReady(e.value(QStringLiteral("id")).toString(), e.value(QStringLiteral("url")).toString());
        }
    }
}

void Bridge::attachWindow(QQuickWindow *window)
{
    if (!window || m_window == window)
        return;
    m_window = window;
    window->setColor(Qt::black);
    connect(window, &QQuickWindow::beforeRenderPassRecording, this, &Bridge::renderUnderlay, Qt::DirectConnection);
    connect(window, &QQuickWindow::sceneGraphInvalidated, this, [this]() {
        if (m_core)
            bl_core_gl_lost(m_core);
        m_glReady = false;
    }, Qt::DirectConnection);
    // Keep animating: the visualizer changes every frame.
    connect(window, &QQuickWindow::frameSwapped, window, &QQuickWindow::update, Qt::QueuedConnection);
    m_frameClock.start();
    window->update();
}

void Bridge::renderUnderlay()
{
    if (!m_core || !m_window || m_glFailed)
        return;
    m_window->beginExternalCommands();
    QOpenGLContext *ctx = QOpenGLContext::currentContext();
    if (ctx) {
        if (!m_glReady) {
            const int flavor = ctx->isOpenGLES() ? 1 : 0;
            const int r = bl_core_gl_init(m_core, getProc, ctx, flavor);
            m_glReady = r == 0;
            m_glFailed = r != 0;
            if (m_glFailed)
                log(QStringLiteral("visualizer unavailable (code %1)").arg(r));
        }
        if (m_glReady) {
            GLint fbo = 0;
            ctx->functions()->glGetIntegerv(GL_FRAMEBUFFER_BINDING, &fbo);
            const qreal dpr = m_window->effectiveDevicePixelRatio();
            const QSize size = m_window->size() * dpr;
            const double dt = m_frameClock.isValid() ? m_frameClock.restart() / 1000.0 : 1.0 / 60.0;
            bl_core_render(m_core, static_cast<uint32_t>(fbo), size.width(), size.height(), float(dpr), dt);
        }
    }
    m_window->endExternalCommands();
}

QStringList Bridge::localPaths(const QList<QUrl> &urls)
{
    QStringList out;
    const QString cache = QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/picked");
    for (const QUrl &u : urls) {
        if (u.isLocalFile()) {
            out << u.toLocalFile();
            continue;
        }
        if (u.scheme().isEmpty()) {
            out << u.toString();
            continue;
        }
        // Android content:// URIs: copy into the cache under the display name.
        QFile in(u.toString());
        if (!in.open(QIODevice::ReadOnly))
            continue;
        QDir().mkpath(cache);
        QString name = QFileInfo(u.toString()).fileName();
        if (name.isEmpty())
            name = QStringLiteral("file");
        const QString target = cache + QLatin1Char('/') + name;
        QFile outFile(target);
        if (outFile.open(QIODevice::WriteOnly)) {
            while (!in.atEnd())
                outFile.write(in.read(1 << 20));
            out << target;
        }
    }
    return out;
}

void Bridge::openUrls(const QList<QUrl> &urls, bool play)
{
    const QStringList paths = localPaths(urls);
    QVariantList list;
    for (const QString &p : paths)
        list << p;
    call(QStringLiteral("open"), {{QStringLiteral("paths"), list}, {QStringLiteral("play"), play}});
}

void Bridge::setKeepScreenOn(bool on)
{
    m_keepScreenOn = on;
#ifdef Q_OS_ANDROID
    QNativeInterface::QAndroidApplication::runOnAndroidMainThread([on]() {
        QJniObject activity = QNativeInterface::QAndroidApplication::context();
        QJniObject window = activity.callObjectMethod("getWindow", "()Landroid/view/Window;");
        const jint flag = 128; // FLAG_KEEP_SCREEN_ON
        if (on)
            window.callMethod<void>("addFlags", "(I)V", flag);
        else
            window.callMethod<void>("clearFlags", "(I)V", flag);
    });
#endif
}

void Bridge::requestMediaPermission()
{
#ifdef Q_OS_ANDROID
    const bool modern = QNativeInterface::QAndroidApplication::sdkVersion() >= 33;
    QStringList perms = modern ? QStringList{QStringLiteral("android.permission.READ_MEDIA_AUDIO"), QStringLiteral("android.permission.READ_MEDIA_IMAGES")}
                               : QStringList{QStringLiteral("android.permission.READ_EXTERNAL_STORAGE")};
    bool all = true;
    for (const QString &p : perms) {
        auto r = QtAndroidPrivate::checkPermission(p).result();
        if (r != QtAndroidPrivate::Authorized)
            r = QtAndroidPrivate::requestPermission(p).result();
        all = all && r == QtAndroidPrivate::Authorized;
    }
    if (all) {
        call(QStringLiteral("library.rescan"));
        emit mediaPermissionGranted();
    }
#else
    emit mediaPermissionGranted();
#endif
}

void Bridge::copyText(const QString &text)
{
    QGuiApplication::clipboard()->setText(text);
}
