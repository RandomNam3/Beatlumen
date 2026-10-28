#include "bridge.h"
#include "audio.h"
#include "testhooks.h"
#include "tls.h"
#include "web.h"

#include <QClipboard>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QGuiApplication>
#include <QJSEngine>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocale>
#include <QOpenGLContext>
#include <QOpenGLFunctions>
#include <QPalette>
#include <QPointer>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QStyleHints>

#ifdef Q_OS_ANDROID
#include "android_media.h"
#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>
#include <QThreadPool>
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
#if defined(Q_OS_WASM)
    const QString dataDir = web::dataDir();
#else
    const QString dataDir = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
#endif
    QDir().mkpath(dataDir);
    QJsonArray jargs;
    for (const QString &a : args)
        jargs.append(a);
    const QByteArray argsJson = QJsonDocument(jargs).toJson(QJsonDocument::Compact);
    const QByteArray lang = QLocale::system().name().left(2).toUtf8();

    BlCallbacks cb{};
    cb.user = this;
    cb.log = logCallback;
#if defined(Q_OS_WASM)
    cb.http_request = web::httpRequest;
#endif
    AudioOutput::configure(cb);
    tls::configure(cb);
    if (!cb.audio_push)
        cb.audio_start = audioStartCallback;
    m_core = bl_core_new(dataDir.toUtf8().constData(), argsJson.constData(), lang.constData(), platform().toUtf8().constData(), cb);

    // Files picked through content URIs are copied into the cache; drop
    // copies older than a week (they are only needed while playing).
    const QDir picked(QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/picked"));
    const QDateTime weekAgo = QDateTime::currentDateTime().addDays(-7);
    for (const QFileInfo &fi : picked.entryInfoList(QDir::Files)) {
        if (fi.lastModified() < weekAgo)
            QFile::remove(fi.absoluteFilePath());
    }

    connect(&m_timer, &QTimer::timeout, this, &Bridge::tick);
    m_timer.setTimerType(Qt::PreciseTimer);
    m_timer.start(16);
    connect(QGuiApplication::styleHints(), &QStyleHints::colorSchemeChanged, this, &Bridge::systemThemeChanged);
    connect(qGuiApp, &QGuiApplication::paletteChanged, this, &Bridge::systemThemeChanged);
    connect(qGuiApp, &QGuiApplication::applicationStateChanged, this, [this](Qt::ApplicationState s) {
        if (s != Qt::ApplicationActive && m_core) {
            bl_core_save(m_core);
            web::persistStorage();
        }
    });
#ifdef Q_OS_ANDROID
    // Background playback: media session, notification, foreground service.
    new AndroidMedia(this);
    // With the screen off or the app in the background nothing is drawn
    // (no visuals, no live analysis); the core puts off track analyses and
    // only needs to deliver state a few times a second. Audio is decoded on
    // its own thread and moves on to the next track by itself.
    connect(qGuiApp, &QGuiApplication::applicationStateChanged, this, [this](Qt::ApplicationState s) {
        const bool visible = s == Qt::ApplicationActive;
        m_timer.setInterval(visible ? 16 : 250);
        call(QStringLiteral("app.visible"), {{QStringLiteral("visible"), visible}});
    });
#endif
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

QJSValue Bridge::call(const QString &cmd, const QVariantMap &args)
{
    if (!m_core)
        return QJSValue();
    QJsonObject o = QJsonObject::fromVariantMap(args);
    o.insert(QStringLiteral("cmd"), cmd);
    const QByteArray req = QJsonDocument(o).toJson(QJsonDocument::Compact);
    const QByteArray res = takeString(bl_core_command(m_core, req.constData()));
    // Commands often change state; deliver it without waiting for the timer.
    QMetaObject::invokeMethod(this, &Bridge::tick, Qt::QueuedConnection);
    // Parse in the JS engine so QML gets plain JS arrays and objects.
    QJSEngine *engine = qjsEngine(this);
    if (!engine)
        return QJSValue();
    QJSValue parse = engine->globalObject().property(QStringLiteral("JSON")).property(QStringLiteral("parse"));
    return parse.call({QJSValue(QString::fromUtf8(res))});
}

void Bridge::tick()
{
    if (!m_core)
        return;
    handleEvents(takeString(bl_core_tick(m_core)));
#if defined(Q_OS_WASM)
    // Settings are written by the core every few seconds; mirror the data
    // directory to IndexedDB at a similar pace.
    if (++m_persistCounter % 300 == 0)
        web::persistStorage();
#endif
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
            const int underrunsBefore = m_state.value(QStringLiteral("underruns")).toInt();
            m_state = e.value(QStringLiteral("state")).toObject().toVariantMap();
            // Certificate the user trusts for the server (Qt's own requests).
            const QVariantMap settings = m_state.value(QStringLiteral("settings")).toMap();
            const QUrl server(settings.value(QStringLiteral("serverUrl")).toString());
            if (server.scheme() == QLatin1String("https"))
                tls::setPin(server.host() + QLatin1Char(':') + QString::number(server.port(443)),
                            settings.value(QStringLiteral("serverCertPin")).toString().toLatin1());
            if (!qEnvironmentVariableIsEmpty("BEATLUMEN_TEST_DUMP")) {
                const int u = m_state.value(QStringLiteral("underruns")).toInt();
                if (u != underrunsBefore)
                    qInfo("audio underruns: %d", u);
            }
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
    TestHooks::attach(window);
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
    openPaths(localPaths(urls), play);
}

void Bridge::openPaths(const QStringList &paths, bool play)
{
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

#ifdef Q_OS_ANDROID
namespace {

// Receives the result of the system document picker (ACTION_OPEN_DOCUMENT)
// and imports the picked documents on a worker thread (FileImport.java):
// images become permanent backgrounds in the app's data folder, everything
// else (songs to play, presets) is copied into the cache.
class PickReceiver : public QAndroidActivityResultReceiver
{
public:
    explicit PickReceiver(Bridge *bridge) : m_bridge(bridge) {}
    QString accept;

    void handleActivityResult(int, int resultCode, const QJniObject &data) override
    {
        const int resultOk = -1;
        if (resultCode != resultOk || !data.isValid())
            return;
        QJniObject uris = QJniObject::callStaticObjectMethod("io/github/randomnam3/beatlumen/FileImport", "uris",
                                                             "(Landroid/content/Intent;)[Ljava/lang/String;", data.object());
        if (!uris.isValid())
            return;
        const bool images = accept.startsWith(QStringLiteral("image/"));
        const QString dest = images ? QStandardPaths::writableLocation(QStandardPaths::AppDataLocation) + QStringLiteral("/backgrounds")
                                    : QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/picked");
        const jlong maxBytes = images ? jlong(64) << 20 : jlong(2) << 30;
        QPointer<Bridge> bridge = m_bridge;
        QThreadPool::globalInstance()->start([bridge, uris, dest, maxBytes]() {
            QJniObject ctx = QNativeInterface::QAndroidApplication::context();
            QJniObject result = QJniObject::callStaticObjectMethod(
                "io/github/randomnam3/beatlumen/FileImport", "copy",
                "(Landroid/content/Context;[Ljava/lang/String;Ljava/lang/String;J)[Ljava/lang/String;",
                ctx.object(), uris.object(), QJniObject::fromString(dest).object<jstring>(), maxBytes);
            QStringList paths;
            if (result.isValid()) {
                QJniEnvironment env;
                auto arr = result.object<jobjectArray>();
                const jsize n = env->GetArrayLength(arr);
                for (jsize i = 0; i < n; ++i) {
                    QJniObject str = QJniObject::fromLocalRef(env->GetObjectArrayElement(arr, i));
                    paths << str.toString();
                }
            }
            QMetaObject::invokeMethod(bridge, [bridge, paths]() {
                if (bridge && !paths.isEmpty())
                    bridge->openPaths(paths, true);
            }, Qt::QueuedConnection);
        });
    }

private:
    Bridge *m_bridge;
};

} // namespace
#endif

void Bridge::pickFiles(const QString &accept, bool multiple)
{
#ifdef Q_OS_ANDROID
    // The system document picker; picked content URIs are copied into the
    // cache and opened like local files (audio plays, images become
    // backgrounds, .hfx files are imported).
    static PickReceiver *receiver = new PickReceiver(this);
    receiver->accept = accept;
    QJniObject intent("android/content/Intent", "(Ljava/lang/String;)V", QJniObject::fromString(QStringLiteral("android.intent.action.OPEN_DOCUMENT")).object<jstring>());
    intent.callObjectMethod("addCategory", "(Ljava/lang/String;)Landroid/content/Intent;", QJniObject::fromString(QStringLiteral("android.intent.category.OPENABLE")).object<jstring>());
    intent.callObjectMethod("setType", "(Ljava/lang/String;)Landroid/content/Intent;", QJniObject::fromString(accept.isEmpty() ? QStringLiteral("*/*") : accept).object<jstring>());
    intent.callObjectMethod("putExtra", "(Ljava/lang/String;Z)Landroid/content/Intent;", QJniObject::fromString(QStringLiteral("android.intent.extra.ALLOW_MULTIPLE")).object<jstring>(), jboolean(multiple));
    QtAndroidPrivate::startActivity(intent, 4711, receiver);
#else
    web::pickFiles(accept, multiple, [this](const QString &name, const QByteArray &data) {
        if (m_core)
            bl_core_open_data(m_core, name.toUtf8().constData(), reinterpret_cast<const uint8_t *>(data.constData()), size_t(data.size()));
    });
#endif
}

bool Bridge::moveTaskToBack()
{
#ifdef Q_OS_ANDROID
    // Back with nothing left to close while music plays: keep running in the
    // background (like the home button) instead of finishing the activity.
    QNativeInterface::QAndroidApplication::runOnAndroidMainThread([]() {
        QJniObject activity = QtAndroidPrivate::activity();
        if (activity.isValid())
            activity.callMethod<jboolean>("moveTaskToBack", "(Z)Z", jboolean(true));
    });
    return true;
#else
    return false;
#endif
}
