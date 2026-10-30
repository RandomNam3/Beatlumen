#include "bridge.h"
#include "audio.h"
#include "testhooks.h"
#include "secrets.h"
#include "tls.h"
#include "web.h"

#include <QClipboard>
#include <QDesktopServices>
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
#include <QNetworkInformation>
#include <QOpenGLContext>
#include <QOpenGLFunctions>
#include <QPalette>
#include <QPointer>
#include <QProcess>
#include <QSettings>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QStyleHints>

#if defined(BEATLUMEN_PORTAL)
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusVariant>
#endif
#if defined(BEATLUMEN_MPRIS) || defined(BEATLUMEN_SMTC)
#include "media_desktop.h"
#endif
#if defined(BEATLUMEN_DESKTOP_CAPTURE)
#include "desktop_capture.h"
#endif

#ifdef Q_OS_ANDROID
#include "android_capture.h"
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

int32_t openUrlCallback(void *, const char *url)
{
#if defined(Q_OS_WASM)
    web::openUrl(QString::fromUtf8(url));
    return 1;
#else
    return QDesktopServices::openUrl(QUrl(QString::fromUtf8(url))) ? 1 : 0;
#endif
}

uint32_t audioStartCallback(void *user)
{
    Q_UNUSED(user);
    return AudioOutput::instance()->start();
}

#ifdef Q_OS_ANDROID
// AirPlay announcement through Android's NsdManager (ServiceAnnouncer.java).
constexpr const char *kAnnouncer = "io/github/randomnam3/beatlumen/ServiceAnnouncer";

int32_t serviceRegister(void *, const char *instance, const char *type, uint16_t port, const char *txt)
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    const jboolean ok = QJniObject::callStaticMethod<jboolean>(
        kAnnouncer, "register", "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;ILjava/lang/String;)Z",
        ctx.object(), QJniObject::fromString(QString::fromUtf8(instance)).object<jstring>(),
        QJniObject::fromString(QString::fromUtf8(type)).object<jstring>(), jint(port),
        QJniObject::fromString(QString::fromUtf8(txt)).object<jstring>());
    return ok ? 1 : 0;
}

void serviceUnregister(void *)
{
    QJniObject::callStaticMethod<void>(kAnnouncer, "unregister", "()V");
}

// UnifiedPush (UnifiedPush.java): registration with the distributor, and the
// endpoint and decrypted messages coming back from Java.
constexpr const char *kPush = "io/github/randomnam3/beatlumen/UnifiedPush";
Bridge *g_pushBridge = nullptr;

int32_t pushRegister(void *, const char *instance, const char *vapid)
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    return QJniObject::callStaticMethod<jint>(kPush, "register", "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)I",
        ctx.object(), QJniObject::fromString(QString::fromUtf8(instance)).object<jstring>(),
        QJniObject::fromString(QString::fromUtf8(vapid)).object<jstring>());
}

void pushUnregister(void *, const char *instance)
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject::callStaticMethod<void>(kPush, "unregister", "(Landroid/content/Context;Ljava/lang/String;)V",
        ctx.object(), QJniObject::fromString(QString::fromUtf8(instance)).object<jstring>());
}

QString jstr(JNIEnv *env, jstring s)
{
    if (!s)
        return {};
    const char *c = env->GetStringUTFChars(s, nullptr);
    QString out = QString::fromUtf8(c);
    env->ReleaseStringUTFChars(s, c);
    return out;
}

// Called on a Java thread: forwarded to the core on the main thread.
void pushCommand(const QVariantMap &cmd)
{
    if (g_pushBridge)
        QMetaObject::invokeMethod(g_pushBridge, [cmd]() { if (g_pushBridge) g_pushBridge->call(cmd.value(QStringLiteral("cmd")).toString(), cmd); }, Qt::QueuedConnection);
}

void JNICALL nativeEndpoint(JNIEnv *env, jclass, jstring instance, jstring endpoint, jstring p256dh, jstring auth)
{
    pushCommand({ { QStringLiteral("cmd"), QStringLiteral("push.endpoint") },
        { QStringLiteral("instance"), jstr(env, instance) }, { QStringLiteral("endpoint"), jstr(env, endpoint) },
        { QStringLiteral("p256dh"), jstr(env, p256dh) }, { QStringLiteral("auth"), jstr(env, auth) } });
}

void JNICALL nativeMessage(JNIEnv *env, jclass, jstring json)
{
    pushCommand({ { QStringLiteral("cmd"), QStringLiteral("push.message") }, { QStringLiteral("json"), jstr(env, json) } });
}

void JNICALL nativeFailed(JNIEnv *env, jclass, jstring instance, jstring reason)
{
    pushCommand({ { QStringLiteral("cmd"), QStringLiteral("push.failed") },
        { QStringLiteral("instance"), jstr(env, instance) }, { QStringLiteral("reason"), jstr(env, reason) } });
}

// Passkeys and saved passwords through Android's Credential Manager
// (Credentials.java); results come back on the main thread as
// "vault.credentialResult".
constexpr const char *kCredentials = "io/github/randomnam3/beatlumen/Credentials";

int32_t credentialRequest(void *, const char *kind, const char *json)
{
    const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
    QJniObject activity = QtAndroidPrivate::activity();
    if (!activity.isValid())
        return 0;
    const QByteArray k(kind);
    auto str = [&](const char *key) { return QJniObject::fromString(o.value(QLatin1String(key)).toString()); };
    if (k == "get") {
        QJniObject::callStaticMethod<void>(kCredentials, "get", "(Landroid/app/Activity;Ljava/lang/String;Z)V",
            activity.object(), str("publicKey").object<jstring>(), jboolean(o.value(QStringLiteral("password")).toBool()));
    } else if (k == "create") {
        QJniObject::callStaticMethod<void>(kCredentials, "createPasskey", "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)V",
            activity.object(), str("publicKey").object<jstring>(), str("user").object<jstring>());
    } else if (k == "save") {
        QJniObject::callStaticMethod<void>(kCredentials, "savePassword", "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)V",
            activity.object(), str("user").object<jstring>(), str("password").object<jstring>());
    } else {
        return 0;
    }
    return 1;
}

void JNICALL nativeCredentialResult(JNIEnv *env, jclass, jstring kind, jboolean ok, jstring json)
{
    pushCommand({ { QStringLiteral("cmd"), QStringLiteral("vault.credentialResult") },
        { QStringLiteral("kind"), jstr(env, kind) }, { QStringLiteral("ok"), bool(ok) }, { QStringLiteral("json"), jstr(env, json) } });
}

void setupCredentials()
{
    const JNINativeMethod methods[] = {
        { "nativeResult", "(Ljava/lang/String;ZLjava/lang/String;)V", reinterpret_cast<void *>(nativeCredentialResult) },
    };
    QJniEnvironment env;
    env.registerNativeMethods(kCredentials, methods, 1);
}

// Opened from the system's media controls after the app was closed
// (ResumeService / MediaBridge.resumeInApp): play on. The flag is taken so
// that a recreated activity does not play again.
bool takeResume(QJniObject &intent)
{
    if (!intent.isValid())
        return false;
    const QJniObject key = QJniObject::fromString(QStringLiteral("io.github.randomnam3.beatlumen.RESUME"));
    if (!intent.callMethod<jboolean>("getBooleanExtra", "(Ljava/lang/String;Z)Z", key.object<jstring>(), jboolean(false)))
        return false;
    intent.callMethod<void>("removeExtra", "(Ljava/lang/String;)V", key.object<jstring>());
    return true;
}

// Songs, playlists and links shared with the app ("Share", "Open with"):
// documents are copied like picked files, then the core asks what to do.
bool takeShared(Bridge *bridge, const QJniObject &intent)
{
    if (!intent.isValid())
        return false;
    const char *cls = "io/github/randomnam3/beatlumen/FileImport";
    QJniObject uris = QJniObject::callStaticObjectMethod(cls, "shared", "(Landroid/content/Intent;)[Ljava/lang/String;", intent.object());
    QJniObject text = QJniObject::callStaticObjectMethod(cls, "sharedText", "(Landroid/content/Intent;)Ljava/lang/String;", intent.object());
    QJniEnvironment env;
    const jsize n = uris.isValid() ? env->GetArrayLength(uris.object<jobjectArray>()) : 0;
    const QString link = text.isValid() ? text.toString() : QString();
    if (n == 0 && link.isEmpty())
        return false;
    // Handled once (also when the activity is recreated).
    intent.callObjectMethod("setAction", "(Ljava/lang/String;)Landroid/content/Intent;", QJniObject::fromString(QStringLiteral("android.intent.action.MAIN")).object<jstring>());
    QPointer<Bridge> b = bridge;
    if (!link.isEmpty()) {
        QMetaObject::invokeMethod(bridge, [b, link]() { if (b) b->call(QStringLiteral("share.receive"), { { QStringLiteral("text"), link } }); }, Qt::QueuedConnection);
        return true;
    }
    const QString dest = QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/shared");
    QThreadPool::globalInstance()->start([b, uris, dest]() {
        QJniObject ctx = QNativeInterface::QAndroidApplication::context();
        QJniObject result = QJniObject::callStaticObjectMethod(
            "io/github/randomnam3/beatlumen/FileImport", "copy",
            "(Landroid/content/Context;[Ljava/lang/String;Ljava/lang/String;J)[Ljava/lang/String;",
            ctx.object(), uris.object(), QJniObject::fromString(dest).object<jstring>(), jlong(2) << 30);
        QVariantList paths;
        if (result.isValid()) {
            QJniEnvironment e;
            auto arr = result.object<jobjectArray>();
            for (jsize i = 0; i < e->GetArrayLength(arr); ++i)
                paths << QJniObject::fromLocalRef(e->GetObjectArrayElement(arr, i)).toString();
        }
        QMetaObject::invokeMethod(QCoreApplication::instance(), [b, paths]() {
            if (b && !paths.isEmpty())
                b->call(QStringLiteral("share.receive"), { { QStringLiteral("paths"), paths } });
        }, Qt::QueuedConnection);
    });
    return true;
}

// beatlumen://auth?state=…&result=… opened by the sign-in page in the
// browser (passkeys), while the app runs or when the link starts it.
struct AuthLinkListener : QtAndroidPrivate::NewIntentListener {
    Bridge *bridge = nullptr;
    bool handleNewIntent(JNIEnv *, jobject intent) override
    {
        QJniObject i(intent);
        if (capture::takeTileRequest(i)) {
            QPointer<Bridge> b = bridge;
            QMetaObject::invokeMethod(bridge, [b]() { if (b) b->call(QStringLiteral("capture.start")); }, Qt::QueuedConnection);
            return true;
        }
        if (takeResume(i)) {
            QPointer<Bridge> b = bridge;
            QMetaObject::invokeMethod(bridge, [b]() { if (b) b->call(QStringLiteral("play")); }, Qt::QueuedConnection);
            return true;
        }
        if (takeShared(bridge, i))
            return true;
        QJniObject uri = i.callObjectMethod("getData", "()Landroid/net/Uri;");
        if (!uri.isValid())
            return false;
        const QString url = uri.callObjectMethod("toString", "()Ljava/lang/String;").toString();
        if (!url.startsWith(QStringLiteral("beatlumen://auth")))
            return false;
        QPointer<Bridge> b = bridge;
        QMetaObject::invokeMethod(bridge, [b, url]() { if (b) b->call(QStringLiteral("vault.browserResult"), { { QStringLiteral("url"), url } }); }, Qt::QueuedConnection);
        return true;
    }
};

void setupAuthLinks(Bridge *bridge)
{
    static AuthLinkListener listener;
    listener.bridge = bridge;
    QtAndroidPrivate::registerNewIntentListener(&listener);
    QJniObject activity = QtAndroidPrivate::activity();
    QJniObject intent = activity.isValid() ? activity.callObjectMethod("getIntent", "()Landroid/content/Intent;") : QJniObject();
    if (capture::takeTileRequest(intent)) {
        QPointer<Bridge> b = bridge;
        QTimer::singleShot(500, bridge, [b]() { if (b) b->call(QStringLiteral("capture.start")); });
    } else if (takeResume(intent)) {
        QPointer<Bridge> b = bridge;
        // After the core restored the queue.
        QTimer::singleShot(500, bridge, [b]() { if (b) b->call(QStringLiteral("play")); });
    } else {
        QPointer<Bridge> b = bridge;
        QJniObject i = intent;
        QTimer::singleShot(500, bridge, [b, i]() { if (b) takeShared(b, i); });
    }
}

void setupPush(Bridge *bridge)
{
    g_pushBridge = bridge;
    const JNINativeMethod methods[] = {
        { "nativeEndpoint", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V", reinterpret_cast<void *>(nativeEndpoint) },
        { "nativeMessage", "(Ljava/lang/String;)V", reinterpret_cast<void *>(nativeMessage) },
        { "nativeFailed", "(Ljava/lang/String;Ljava/lang/String;)V", reinterpret_cast<void *>(nativeFailed) },
    };
    QJniEnvironment env;
    if (env.registerNativeMethods(kPush, methods, 3))
        QJniObject::callStaticMethod<void>(kPush, "setNativeReady", "(Z)V", jboolean(true));
}
#endif

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
    cb.open_url = openUrlCallback;
    probeNativeDialogs();
    secrets::configure(cb, this);
#if defined(BEATLUMEN_DESKTOP_CAPTURE)
    desktopcapture::configure(cb, this);
#endif
#ifdef Q_OS_ANDROID
    cb.service_register = serviceRegister;
    cb.service_unregister = serviceUnregister;
    cb.push_register = pushRegister;
    cb.push_unregister = pushUnregister;
    cb.credential_request = credentialRequest;
    capture::configure(cb, this);
    setupPush(this);
    setupCredentials();
    setupAuthLinks(this);
#endif
    m_core = bl_core_new(dataDir.toUtf8().constData(), argsJson.constData(), lang.constData(), platform().toUtf8().constData(), cb);
#if defined(Q_OS_WASM)
    web::listenAuth([this](const QString &query) { call(QStringLiteral("vault.browserResult"), { { QStringLiteral("url"), query } }); });
#endif

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
#if defined(BEATLUMEN_MPRIS) || defined(BEATLUMEN_SMTC)
    m_desktopMedia = new DesktopMedia(this);
#endif
#ifndef Q_OS_WASM
    // Mobile data or not: picks the stream quality setting for the network.
    if (QNetworkInformation::loadBackendByFeatures(QNetworkInformation::Feature::Metered)
        || QNetworkInformation::loadDefaultBackend()) {
        if (auto *net = QNetworkInformation::instance()) {
            auto report = [this, net]() {
                const bool metered = net->isMetered()
                    || net->transportMedium() == QNetworkInformation::TransportMedium::Cellular;
                call(QStringLiteral("net.metered"), {{QStringLiteral("metered"), metered}});
            };
            connect(net, &QNetworkInformation::isMeteredChanged, this, report);
            connect(net, &QNetworkInformation::transportMediumChanged, this, report);
            report();
        }
    }
#endif
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

// Linux: Qt's file dialogs are native only through the XDG desktop portal
// (main.cpp selects that platform theme); without a portal they fall back to
// a generic Qt Quick dialog, so the app's own picker is used instead.
// BEATLUMEN_NATIVE_DIALOGS=0/1 overrides the check.
void Bridge::probeNativeDialogs()
{
    const QByteArray force = qgetenv("BEATLUMEN_NATIVE_DIALOGS");
    if (!force.isEmpty()) {
        m_nativeDialogs = force != "0";
        return;
    }
#if defined(BEATLUMEN_PORTAL)
    if (qgetenv("QT_QPA_PLATFORMTHEME") != "xdgdesktopportal")
        return;
    QDBusMessage msg = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.portal.Desktop"),
        QStringLiteral("/org/freedesktop/portal/desktop"),
        QStringLiteral("org.freedesktop.DBus.Properties"), QStringLiteral("Get"));
    msg << QStringLiteral("org.freedesktop.portal.FileChooser") << QStringLiteral("version");
    auto *w = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(msg, 10000), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        QDBusPendingReply<QDBusVariant> r = *w;
        w->deleteLater();
        // Version 3 picks folders too.
        if (!r.isError() && r.value().variant().toUInt() >= 3) {
            m_nativeDialogs = true;
            emit nativeDialogsChanged();
        } else {
            log(QStringLiteral("no file chooser portal: using the app's file picker"));
        }
    });
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

#if defined(Q_OS_LINUX) && !defined(Q_OS_ANDROID)
namespace {

// "r,g,b" (QSettings reads it as a list).
QColor rgbSetting(const QVariant &v)
{
    const QStringList parts = v.toStringList().size() == 3 ? v.toStringList() : v.toString().split(QLatin1Char(','));
    if (parts.size() != 3)
        return {};
    return QColor(parts[0].trimmed().toInt(), parts[1].trimmed().toInt(), parts[2].trimmed().toInt());
}

// The accent of KDE Plasma (kdeglobals): the chosen accent colour, else the
// colour scheme's selection colour. Read directly, as a Qt that ships with
// the app (AppImage) does not load Plasma's own platform theme.
QColor kdeAccent()
{
    const QString path = QStandardPaths::locate(QStandardPaths::GenericConfigLocation, QStringLiteral("kdeglobals"));
    if (path.isEmpty())
        return {};
    QSettings s(path, QSettings::IniFormat);
    QColor c = rgbSetting(s.value(QStringLiteral("General/AccentColor")));
    if (!c.isValid())
        c = rgbSetting(s.value(QStringLiteral("Colors:Selection/BackgroundNormal")));
    return c;
}

// GNOME 47+ accent colour names.
QColor gnomeAccent()
{
    QProcess p;
    p.start(QStringLiteral("gsettings"), {QStringLiteral("get"), QStringLiteral("org.gnome.desktop.interface"), QStringLiteral("accent-color")});
    if (!p.waitForFinished(500) || p.exitCode() != 0)
        return {};
    const QString name = QString::fromUtf8(p.readAllStandardOutput()).trimmed().remove(QLatin1Char('\''));
    static const QHash<QString, QColor> colors = {
        {QStringLiteral("blue"), QColor(0x35, 0x84, 0xe4)},   {QStringLiteral("teal"), QColor(0x21, 0x90, 0xa4)},
        {QStringLiteral("green"), QColor(0x3a, 0x94, 0x4a)},  {QStringLiteral("yellow"), QColor(0xc8, 0x88, 0x00)},
        {QStringLiteral("orange"), QColor(0xed, 0x5b, 0x00)}, {QStringLiteral("red"), QColor(0xe6, 0x2d, 0x42)},
        {QStringLiteral("pink"), QColor(0xd5, 0x61, 0x99)},   {QStringLiteral("purple"), QColor(0x91, 0x41, 0xac)},
        {QStringLiteral("slate"), QColor(0x6f, 0x83, 0x96)},
    };
    return colors.value(name);
}

} // namespace
#endif

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
    // A usable accent: not (nearly) black, white or transparent, which some
    // desktops report when no accent is known.
    const auto usable = [](const QColor &c) {
        return c.isValid() && c.alpha() > 200 && c.lightness() > 45 && c.lightness() < 225;
    };
#if defined(Q_OS_LINUX) && !defined(Q_OS_ANDROID)
    const QString desktop = qEnvironmentVariable("XDG_CURRENT_DESKTOP").toUpper();
    if (desktop.contains(QStringLiteral("KDE"))) {
        const QColor c = kdeAccent();
        if (usable(c))
            return c;
    } else if (desktop.contains(QStringLiteral("GNOME"))) {
        static const QColor c = gnomeAccent();
        if (usable(c))
            return c;
    }
#endif
    const QColor c = QGuiApplication::palette().color(QPalette::Accent);
    return usable(c) ? c : QColor(0xe9, 0x1e, 0x63);
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
#if defined(BEATLUMEN_MPRIS) || defined(BEATLUMEN_SMTC)
    if (m_desktopMedia)
        m_desktopMedia->setWindow(window);
#endif
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

void Bridge::setSystemBarsHidden(bool hidden)
{
#ifdef Q_OS_ANDROID
    QNativeInterface::QAndroidApplication::runOnAndroidMainThread([hidden]() {
        QJniObject activity = QNativeInterface::QAndroidApplication::context();
        QJniObject window = activity.callObjectMethod("getWindow", "()Landroid/view/Window;");
        if (!window.isValid())
            return;
        if (QNativeInterface::QAndroidApplication::sdkVersion() >= 30) {
            QJniObject ctl = window.callObjectMethod("getInsetsController", "()Landroid/view/WindowInsetsController;");
            if (!ctl.isValid())
                return;
            const jint bars = QJniObject::callStaticMethod<jint>("android/view/WindowInsets$Type", "systemBars", "()I");
            if (hidden) {
                ctl.callMethod<void>("setSystemBarsBehavior", "(I)V", jint(2)); // BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                ctl.callMethod<void>("hide", "(I)V", bars);
            } else {
                ctl.callMethod<void>("show", "(I)V", bars);
            }
        } else {
            QJniObject decor = window.callObjectMethod("getDecorView", "()Landroid/view/View;");
            // SYSTEM_UI_FLAG_IMMERSIVE_STICKY | FULLSCREEN | HIDE_NAVIGATION
            decor.callMethod<void>("setSystemUiVisibility", "(I)V", jint(hidden ? 0x1000 | 0x4 | 0x2 : 0));
        }
    });
#else
    Q_UNUSED(hidden);
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
