#include "android_capture.h"
#include "bridge.h"

#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QtCore/private/qandroidextras_p.h>

#include <atomic>

namespace {

constexpr const char *kService = "io/github/randomnam3/beatlumen/CaptureService";
constexpr const char *kSessions = "io/github/randomnam3/beatlumen/MediaSessions";
constexpr int kConsentRequest = 4712;

QPointer<Bridge> g_bridge;
std::atomic<bool> g_running{false};

QString jstr(JNIEnv *env, jstring s)
{
    if (!s)
        return {};
    const char *c = env->GetStringUTFChars(s, nullptr);
    if (!c)
        return {};
    const QString r = QString::fromUtf8(c);
    env->ReleaseStringUTFChars(s, c);
    return r;
}

// Runs a core command on the Qt thread.
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

void watchSessions(bool on)
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject::callStaticMethod<void>(kSessions, "watch", "(Landroid/content/Context;Z)V", ctx.object(), jboolean(on));
}

// Recording started or ended (also: declined, not allowed, failed).
void postState(bool running, const QString &error = {})
{
    if (!running)
        watchSessions(false);
    post(QStringLiteral("capture.state"), { { QStringLiteral("running"), running }, { QStringLiteral("error"), error } });
}

// ---- from Java --------------------------------------------------------------------

void JNICALL nativeState(JNIEnv *env, jclass, jboolean running, jstring error)
{
    g_running = running;
    postState(running, jstr(env, error));
}

// Recording thread: straight to the core's buffer.
void JNICALL nativePcm(JNIEnv *env, jclass, jobject buf, jint bytes, jint channels, jint rate)
{
    const auto *p = static_cast<const int16_t *>(env->GetDirectBufferAddress(buf));
    const jlong capacity = env->GetDirectBufferCapacity(buf);
    if (!p || bytes <= 0 || channels <= 0 || rate <= 0 || bytes > capacity)
        return;
    bl_capture_push(p, size_t(bytes) / (2 * size_t(channels)), uint32_t(channels), uint32_t(rate));
}

void JNICALL nativeSession(JNIEnv *env, jclass, jstring json)
{
    const QString s = jstr(env, json);
    QVariant session;
    if (!s.isEmpty())
        session = QJsonDocument::fromJson(s.toUtf8()).object().toVariantMap();
    post(QStringLiteral("capture.session"), { { QStringLiteral("session"), session } });
}

void JNICALL nativeAccess(JNIEnv *, jclass, jboolean granted)
{
    post(QStringLiteral("capture.access"), { { QStringLiteral("granted"), bool(granted) } });
}

// ---- starting -----------------------------------------------------------------------

// The user's answer on the system's consent screen.
class ConsentReceiver : public QAndroidActivityResultReceiver
{
public:
    void handleActivityResult(int, int resultCode, const QJniObject &data) override
    {
        const int resultOk = -1;
        if (resultCode != resultOk || !data.isValid()) {
            postState(false); // declined: nothing to report
            return;
        }
        QJniObject ctx = QNativeInterface::QAndroidApplication::context();
        QJniObject::callStaticMethod<void>(kService, "start", "(Landroid/content/Context;ILandroid/content/Intent;)V",
                                           ctx.object(), jint(resultCode), data.object());
    }
};

void askConsent()
{
    static ConsentReceiver receiver;
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject intent = QJniObject::callStaticObjectMethod(kService, "consentIntent",
                                                           "(Landroid/content/Context;)Landroid/content/Intent;", ctx.object());
    if (!intent.isValid()) {
        postState(false, QStringLiteral("not available"));
        return;
    }
    QtAndroidPrivate::startActivity(intent, kConsentRequest, &receiver);
}

// Recording other apps needs the audio recording permission (no
// microphone is used), then the user's consent for this session.
void start()
{
    const QString perm = QStringLiteral("android.permission.RECORD_AUDIO");
    if (QtAndroidPrivate::checkPermission(perm).result() == QtAndroidPrivate::Authorized) {
        askConsent();
        return;
    }
    QtAndroidPrivate::requestPermission(perm).then(qApp, [](QtAndroidPrivate::PermissionResult r) {
        if (r == QtAndroidPrivate::Authorized)
            askConsent();
        else
            postState(false, QStringLiteral("permission denied"));
    });
}

int32_t captureRequest(void *, const char *kind, const char *json)
{
    const QByteArray k(kind);
    if (k == "start") {
        // Not from within the core's command: the permission and consent
        // screens come back later.
        QMetaObject::invokeMethod(qApp, &start, Qt::QueuedConnection);
        watchSessions(true);
    } else if (k == "stop") {
        QJniObject::callStaticMethod<void>(kService, "stop", "()V");
        watchSessions(false);
    } else if (k == "access") {
        QJniObject activity = QtAndroidPrivate::activity();
        if (!activity.isValid())
            return 0;
        QJniObject::callStaticMethod<void>(kSessions, "requestAccess", "(Landroid/app/Activity;)V", activity.object());
    } else if (k == "control") {
        const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
        QJniObject::callStaticMethod<void>(kSessions, "control", "(Ljava/lang/String;D)V",
                                           QJniObject::fromString(o.value(QStringLiteral("action")).toString()).object<jstring>(),
                                           jdouble(o.value(QStringLiteral("position")).toDouble()));
    } else {
        return 0;
    }
    return 1;
}

} // namespace

namespace capture {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    if (QNativeInterface::QAndroidApplication::sdkVersion() < 29)
        return; // playback capture needs Android 10
    g_bridge = bridge;
    QJniEnvironment env;
    const JNINativeMethod service[] = {
        { "nativeState", "(ZLjava/lang/String;)V", reinterpret_cast<void *>(nativeState) },
        { "nativePcm", "(Ljava/nio/ByteBuffer;III)V", reinterpret_cast<void *>(nativePcm) },
    };
    const JNINativeMethod sessions[] = {
        { "nativeSession", "(Ljava/lang/String;)V", reinterpret_cast<void *>(nativeSession) },
        { "nativeAccess", "(Z)V", reinterpret_cast<void *>(nativeAccess) },
    };
    if (!env.registerNativeMethods(kService, service, 2) || !env.registerNativeMethods(kSessions, sessions, 2)) {
        qWarning("capture: could not register native methods");
        return;
    }
    QJniObject::callStaticMethod<void>(kService, "setNativeReady", "(Z)V", jboolean(true));
    QJniObject::callStaticMethod<void>(kSessions, "setNativeReady", "(Z)V", jboolean(true));
    cb.capture_request = captureRequest;
}

bool takeTileRequest(QJniObject &intent)
{
    if (!intent.isValid())
        return false;
    const QJniObject key = QJniObject::fromString(QStringLiteral("io.github.randomnam3.beatlumen.CAPTURE"));
    if (!intent.callMethod<jboolean>("getBooleanExtra", "(Ljava/lang/String;Z)Z", key.object<jstring>(), jboolean(false)))
        return false;
    intent.callMethod<void>("removeExtra", "(Ljava/lang/String;)V", key.object<jstring>());
    return true;
}

} // namespace capture
