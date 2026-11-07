// A USB sound card in direct use on Android 14 and newer: the work is done in
// android/src/.../DirectOutput.java, this connects it with the core. Below that
// version the callbacks stay empty, and the core reports it as not available.
#include "audio_direct.h"
#include "bridge.h"

#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>

#include <algorithm>
#include <cstring>
#include <vector>

namespace {

constexpr const char *kOutput = "io/github/randomnam3/beatlumen/DirectOutput";
// AudioManager.setPreferredMixerAttributes.
const int kFirstSdk = 34;
// Beatlumen's PcmFormat.
const size_t kFormatBytes[] = {2, 3, 4, 4};

QPointer<Bridge> g_bridge;
bool g_ready = false;

// Runs a core command on the Qt thread.
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

// ---- from Java --------------------------------------------------------------------

// The output's thread: straight from the core's buffers.
void JNICALL nativeRender(JNIEnv *env, jclass, jobject buf, jint frames, jint format, jint channels, jint latency)
{
    auto *p = static_cast<uint8_t *>(env->GetDirectBufferAddress(buf));
    if (!p || frames <= 0 || format < 0 || format > 3 || channels < 2)
        return;
    if (env->GetDirectBufferCapacity(buf) < jlong(frames) * channels * jlong(kFormatBytes[format]))
        return;
    bl_audio_render_pcm(p, size_t(frames), uint32_t(format), uint32_t(channels), uint32_t(std::max(latency, 0)));
}

void JNICALL nativeFailed(JNIEnv *env, jclass, jstring error)
{
    const char *c = error ? env->GetStringUTFChars(error, nullptr) : nullptr;
    const QString text = c ? QString::fromUtf8(c) : QStringLiteral("The sound card is not available");
    if (c)
        env->ReleaseStringUTFChars(error, c);
    post(QStringLiteral("audio.directFailed"), {{QStringLiteral("error"), text}});
}

// ---- from the core -------------------------------------------------------------------

size_t listCallback(void *, char *json, size_t len)
{
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    const QByteArray list = QJniObject::callStaticObjectMethod(kOutput, "list", "(Landroid/content/Context;)Ljava/lang/String;",
                                                               ctx.object()).toString().toUtf8();
    if (size_t(list.size()) > len)
        return 0;
    std::memcpy(json, list.constData(), size_t(list.size()));
    return size_t(list.size());
}

int32_t openCallback(void *, const char *device, uint32_t rate, const uint32_t *formats, size_t count, BlDirectOpened *opened, char *err, size_t errLen)
{
    QJniEnvironment env;
    const std::vector<jint> values(formats, formats + count);
    jintArray wanted = env->NewIntArray(jsize(count));
    if (!wanted)
        return 0;
    env->SetIntArrayRegion(wanted, 0, jsize(count), values.data());
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    const QJniObject answer = QJniObject::callStaticObjectMethod(
        kOutput, "open", "(Landroid/content/Context;Ljava/lang/String;I[I)Ljava/lang/String;", ctx.object(),
        QJniObject::fromString(QString::fromUtf8(device)).object<jstring>(), jint(rate), wanted);
    env->DeleteLocalRef(wanted);
    const QJsonObject o = QJsonDocument::fromJson(answer.toString().toUtf8()).object();
    if (o.contains(QStringLiteral("rate"))) {
        opened->rate = uint32_t(o.value(QStringLiteral("rate")).toInt());
        opened->format = uint32_t(o.value(QStringLiteral("format")).toInt());
        opened->channels = uint32_t(o.value(QStringLiteral("channels")).toInt());
        return 1;
    }
    const QByteArray text = o.value(QStringLiteral("error")).toString(QStringLiteral("The sound card is not available")).toUtf8();
    if (errLen > 0) {
        const size_t n = std::min(size_t(text.size()), errLen - 1);
        std::memcpy(err, text.constData(), n);
        err[n] = 0;
    }
    return 0;
}

void closeCallback(void *)
{
    QJniObject::callStaticMethod<void>(kOutput, "close", "()V");
}

void idleCallback(void *, int32_t idle)
{
    QJniObject::callStaticMethod<void>(kOutput, "setIdle", "(Z)V", jboolean(idle != 0));
}

} // namespace

namespace directaudio {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    if (QNativeInterface::QAndroidApplication::sdkVersion() < kFirstSdk)
        return;
    g_bridge = bridge;
    QJniEnvironment env;
    const JNINativeMethod methods[] = {
        {"nativeRender", "(Ljava/nio/ByteBuffer;IIII)V", reinterpret_cast<void *>(nativeRender)},
        {"nativeFailed", "(Ljava/lang/String;)V", reinterpret_cast<void *>(nativeFailed)},
    };
    if (!env.registerNativeMethods(kOutput, methods, 2)) {
        qWarning("direct output: could not register native methods");
        return;
    }
    g_ready = true;
    cb.direct_list = listCallback;
    cb.direct_open = openCallback;
    cb.direct_close = closeCallback;
    cb.direct_idle = idleCallback;
}

void shutdown()
{
    if (g_ready)
        closeCallback(nullptr);
}

} // namespace directaudio
