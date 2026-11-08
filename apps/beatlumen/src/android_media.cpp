#include "android_media.h"
#include "bridge.h"
#include "tls.h"

#include <QCoreApplication>
#include <QGuiApplication>
#include <QJniEnvironment>
#include <QJniObject>
#include <QImage>
#include <QMutex>
#include <QSettings>
#include <QThreadPool>
#include <QUrl>
#include <QtCore/private/qandroidextras_p.h>

namespace {

constexpr const char *kJavaClass = "io/github/randomnam3/beatlumen/MediaBridge";

// Must match MediaBridge.STATE_*.
enum JavaState { Stopped = 0, Paused = 1, Playing = 2, Buffering = 3 };

QMutex s_lock;
AndroidMedia *s_instance = nullptr; // guarded by s_lock

// static native void nativeCommand(String cmd);
void JNICALL jniCommand(JNIEnv *env, jclass, jstring cmd)
{
    if (!cmd)
        return;
    const char *s = env->GetStringUTFChars(cmd, nullptr);
    if (!s)
        return;
    const QString c = QString::fromUtf8(s);
    env->ReleaseStringUTFChars(cmd, s);
    AndroidMedia::postCommand(c);
}

// static native void nativeSeek(long positionMs);
void JNICALL jniSeek(JNIEnv *, jclass, jlong positionMs)
{
    AndroidMedia::postSeek(double(positionMs) / 1000.0);
}

int javaState(const QString &play)
{
    if (play == QLatin1String("playing"))
        return Playing;
    if (play == QLatin1String("paused"))
        return Paused;
    if (play == QLatin1String("loading"))
        return Buffering;
    return Stopped;
}

} // namespace

AndroidMedia::AndroidMedia(Bridge *bridge)
    : QObject(bridge)
    , m_bridge(bridge)
{
    static bool registered = false;
    if (!registered) {
        const JNINativeMethod methods[] = {
            {"nativeCommand", "(Ljava/lang/String;)V", reinterpret_cast<void *>(jniCommand)},
            {"nativeSeek", "(J)V", reinterpret_cast<void *>(jniSeek)},
        };
        QJniEnvironment env;
        registered = env.registerNativeMethods(kJavaClass, methods, 2);
        if (!registered)
            qWarning("android media: could not register native methods");
    }
    {
        QMutexLocker lock(&s_lock);
        s_instance = this;
    }
    m_askedNotifications = QSettings().value(QStringLiteral("android/askedNotifications")).toBool();
    connect(bridge, &Bridge::stateChanged, this, [this]() { sync(false); });
    connect(bridge, &Bridge::positionChanged, this, &AndroidMedia::checkPosition);
    connect(bridge, &Bridge::coverReady, this, [this](const QString &id, const QString &url) {
        if (id == m_coverId) {
            m_coverUrl = url;
            sync(false);
        }
    });
    m_posClock.start();
}

AndroidMedia::~AndroidMedia()
{
    {
        QMutexLocker lock(&s_lock);
        s_instance = nullptr;
    }
    QJniObject::callStaticMethod<void>(kJavaClass, "release", "()V");
}

void AndroidMedia::postCommand(const QString &cmd)
{
    if (cmd != QLatin1String("play") && cmd != QLatin1String("pause") && cmd != QLatin1String("toggle")
        && cmd != QLatin1String("next") && cmd != QLatin1String("prev") && cmd != QLatin1String("stop"))
        return;
    QMutexLocker lock(&s_lock);
    if (AndroidMedia *self = s_instance) {
        // Queued on the Qt thread; dropped if `self` is gone by then.
        QMetaObject::invokeMethod(self, [self, cmd]() { self->m_bridge->call(cmd); }, Qt::QueuedConnection);
    }
}

void AndroidMedia::postSeek(double seconds)
{
    QMutexLocker lock(&s_lock);
    if (AndroidMedia *self = s_instance) {
        QMetaObject::invokeMethod(self, [self, seconds]() {
            self->m_bridge->call(QStringLiteral("seek"), {{QStringLiteral("position"), qMax(0.0, seconds)}});
        }, Qt::QueuedConnection);
    }
}

void AndroidMedia::sync(bool force)
{
    const QVariantMap st = m_bridge->state();
    QVariantMap cur = st.value(QStringLiteral("current")).toMap();
    // Other apps' playback has their own media session and notification.
    if (cur.value(QStringLiteral("id")).toString().startsWith(QLatin1String("capture:")))
        cur.clear();
    const QString id = cur.value(QStringLiteral("id")).toString();

    Snapshot s;
    if (!cur.isEmpty()) {
        s.title = cur.value(QStringLiteral("title")).toString();
        if (s.title.isEmpty())
            s.title = QStringLiteral("Beatlumen");
        s.artist = cur.value(QStringLiteral("artist")).toString();
        s.album = cur.value(QStringLiteral("album")).toString();
        s.state = javaState(st.value(QStringLiteral("play")).toString());

        // Cover: server URL or extracted local file; local covers are
        // extracted on request and reported through coverReady.
        QString cover = cur.value(QStringLiteral("cover")).toString();
        if (id != m_coverId) {
            m_coverId = id;
            m_coverUrl.clear();
            if (cover.isEmpty() && cur.value(QStringLiteral("local")).toBool())
                m_bridge->call(QStringLiteral("covers"), {{QStringLiteral("ids"), QVariantList{id}}});
        }
        if (cover.isEmpty())
            cover = m_coverUrl;
        s.cover = cover;
    } else {
        s.state = Stopped;
        m_coverId.clear();
        m_coverUrl.clear();
    }
    s.durationMs = qint64(st.value(QStringLiteral("duration")).toDouble() * 1000.0);

    if (!force && s == m_last)
        return;
    if (s.state == Playing && m_last.state != Playing)
        askNotificationPermission();
    m_lastPos = m_bridge->position();
    m_posClock.restart();

    const bool coverChanged = s.cover != m_last.cover;
    m_last = s;
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject::callStaticMethod<void>(
        kJavaClass, "update",
        "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;JJI)V",
        ctx.object(),
        QJniObject::fromString(s.title).object<jstring>(),
        QJniObject::fromString(s.artist).object<jstring>(),
        QJniObject::fromString(s.album).object<jstring>(),
        QJniObject::fromString(s.cover).object<jstring>(),
        jlong(s.durationMs),
        jlong(m_lastPos * 1000.0),
        jint(s.state));
    if (coverChanged && !s.cover.isEmpty())
        sendCover(s.cover);
}

// The cover for the media session, decoded by the core (the URL is one of
// the core's image://server or image://thumb pictures) on a worker thread
// and handed to Java as pixels: Android's own decoders never see it.
void AndroidMedia::sendCover(const QString &source)
{
    QThreadPool::globalInstance()->start([source]() {
        const QImage img = tls::loadPicture(source, QSize(512, 512)).convertToFormat(QImage::Format_ARGB32);
        if (img.isNull())
            return;
        const int w = img.width(), h = img.height();
        QJniEnvironment env;
        jintArray pixels = env->NewIntArray(jsize(w) * h);
        if (!pixels)
            return;
        // Format_ARGB32 rows are 0xAARRGGBB words, as Android's ARGB_8888 ints.
        for (int y = 0; y < h; ++y)
            env->SetIntArrayRegion(pixels, jsize(y) * w, w, reinterpret_cast<const jint *>(img.constScanLine(y)));
        QJniObject::callStaticMethod<void>(kJavaClass, "setCover", "(Ljava/lang/String;II[I)V",
                                           QJniObject::fromString(source).object<jstring>(), jint(w), jint(h), pixels);
        env->DeleteLocalRef(pixels);
    });
}

// The media session extrapolates the position from the last update; only
// jumps (seeks, track restarts, stalls) need a new one.
void AndroidMedia::checkPosition()
{
    if (m_last.state < 0 || m_last.state == Stopped)
        return;
    const double pos = m_bridge->position();
    const double expected = m_lastPos + (m_last.state == Playing ? m_posClock.elapsed() / 1000.0 : 0.0);
    if (qAbs(pos - expected) > 1.0)
        sync(true);
}

// Android 13+: media-session notifications are exempt from the runtime
// notification permission on stock Android, but not on every vendor build;
// ask once, the first time playback starts while the app is visible.
void AndroidMedia::askNotificationPermission()
{
    if (m_askedNotifications || QNativeInterface::QAndroidApplication::sdkVersion() < 33
        || QGuiApplication::applicationState() != Qt::ApplicationActive)
        return;
    m_askedNotifications = true;
    QSettings().setValue(QStringLiteral("android/askedNotifications"), true);
    const QString perm = QStringLiteral("android.permission.POST_NOTIFICATIONS");
    if (QtAndroidPrivate::checkPermission(perm).result() != QtAndroidPrivate::Authorized)
        (void)QtAndroidPrivate::requestPermission(perm); // asynchronous, result not needed
}
