// Microphones on Android: the device's inputs, recorded with AudioRecord
// (android/src/.../MicRecorder.java). The RECORD_AUDIO permission is asked
// for when an input is opened, not before.
#include "mic_input.h"
#include "bridge.h"

#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QSet>
#include <QtCore/private/qandroidextras_p.h>

namespace {

constexpr const char *kRecorder = "io/github/randomnam3/beatlumen/MicRecorder";

QPointer<Bridge> g_bridge;
// Streams the core wants open (Qt thread): one closed while the
// permission dialog was up is not started afterwards.
QSet<uint32_t> g_wanted;

// Runs a core command on the Qt thread (never inside the current one).
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

void postState(uint32_t stream, bool running, const QString &error = {})
{
    post(QStringLiteral("mic.state"), { { QStringLiteral("stream"), stream },
                                        { QStringLiteral("running"), running },
                                        { QStringLiteral("error"), error } });
}

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

// ---- from Java --------------------------------------------------------------------

void JNICALL nativeMicState(JNIEnv *env, jclass, jint stream, jboolean running, jstring error)
{
    postState(uint32_t(stream), running, jstr(env, error));
}

// Recording threads: straight to the core's buffers.
void JNICALL nativeMicPcm(JNIEnv *env, jclass, jint stream, jobject buf, jint bytes, jint channels, jint rate, jlong ageNs)
{
    const auto *p = static_cast<const int16_t *>(env->GetDirectBufferAddress(buf));
    const jlong capacity = env->GetDirectBufferCapacity(buf);
    if (!p || bytes <= 0 || channels <= 0 || rate <= 0 || bytes > capacity)
        return;
    const int64_t t = ageNs >= 0 ? bl_clock_us() - int64_t(ageNs / 1000) : 0;
    bl_mic_push(uint32_t(stream), p, size_t(bytes) / (2 * size_t(channels)), uint32_t(channels), uint32_t(rate), t);
}

// ---- requests -------------------------------------------------------------------------

void list()
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    const QJniObject json = QJniObject::callStaticObjectMethod(kRecorder, "list", "(Landroid/content/Context;)Ljava/lang/String;",
                                                               ctx.object());
    const QJsonArray devices = QJsonDocument::fromJson(json.toString().toUtf8()).array();
    post(QStringLiteral("mic.devices"), { { QStringLiteral("devices"), devices.toVariantList() } });
}

void openNow(uint32_t stream, const QString &device)
{
    if (!g_wanted.contains(stream))
        return;
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject::callStaticMethod<void>(kRecorder, "open", "(Landroid/content/Context;ILjava/lang/String;)V", ctx.object(),
                                       jint(stream), QJniObject::fromString(device).object<jstring>());
}

// Opening asks for the permission first (once; Android remembers it).
void open(uint32_t stream, const QString &device)
{
    const QString perm = QStringLiteral("android.permission.RECORD_AUDIO");
    if (QtAndroidPrivate::checkPermission(perm).result() == QtAndroidPrivate::Authorized) {
        openNow(stream, device);
        return;
    }
    QtAndroidPrivate::requestPermission(perm).then(qApp, [stream, device](QtAndroidPrivate::PermissionResult r) {
        if (r == QtAndroidPrivate::Authorized)
            openNow(stream, device);
        else
            postState(stream, false, QStringLiteral("permission denied"));
    });
}

int32_t micRequest(void *, const char *kind, const char *json)
{
    const QByteArray k(kind);
    const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
    const uint32_t stream = uint32_t(o.value(QStringLiteral("stream")).toDouble());
    if (k == "list") {
        list();
    } else if (k == "open") {
        // Not from within the core's command: the permission dialog comes
        // back later.
        const QString device = o.value(QStringLiteral("device")).toString();
        g_wanted.insert(stream);
        QMetaObject::invokeMethod(qApp, [stream, device]() { open(stream, device); }, Qt::QueuedConnection);
    } else if (k == "close") {
        g_wanted.remove(stream);
        QJniObject::callStaticMethod<void>(kRecorder, "close", "(I)V", jint(stream));
    } else {
        return 0;
    }
    return 1;
}

} // namespace

namespace micinput {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    QJniEnvironment env;
    const JNINativeMethod methods[] = {
        { "nativeMicState", "(IZLjava/lang/String;)V", reinterpret_cast<void *>(nativeMicState) },
        { "nativeMicPcm", "(ILjava/nio/ByteBuffer;IIIJ)V", reinterpret_cast<void *>(nativeMicPcm) },
    };
    if (!env.registerNativeMethods(kRecorder, methods, 2)) {
        qWarning("mic: could not register native methods");
        return;
    }
    QJniObject::callStaticMethod<void>(kRecorder, "setNativeReady", "(Z)V", jboolean(true));
    cb.mic_request = micRequest;
}

} // namespace micinput
