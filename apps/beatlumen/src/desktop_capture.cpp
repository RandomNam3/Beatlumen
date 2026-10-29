#include "desktop_capture.h"
#include "bridge.h"

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QSet>
#include <QCryptographicHash>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDBusReply>
#include <QDBusVariant>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QStandardPaths>
#include <QThreadPool>
#include <QTimer>
#include <QUrl>

#include <atomic>
#include <cmath>
#include <dlfcn.h>
#include <thread>
#include <vector>

namespace {

// Records the monitor of the default output through libpulse-simple
// (PulseAudio, or PipeWire's PulseAudio service). Qt Multimedia hides
// monitor sources, so it cannot record them. Loaded at run time: the app
// starts without it.
class PulseRecorder
{
public:
    ~PulseRecorder() { stop(); }

    // Empty on success, else what went wrong.
    QString start()
    {
        stop();
        if (!load())
            return QStringLiteral("PulseAudio or PipeWire is needed");
        SampleSpec spec { 3 /* PA_SAMPLE_S16LE */, kRate, 2 };
        // Short fragments: the visuals follow what is heard closely.
        const uint32_t frag = kRate / 25 * 4; // 40 ms
        BufferAttr attr { uint32_t(-1), uint32_t(-1), uint32_t(-1), uint32_t(-1), frag };
        int err = 0;
        m_pa = m_new(nullptr, "Beatlumen", 2 /* PA_STREAM_RECORD */, "@DEFAULT_MONITOR@",
                     "Visualizer", &spec, nullptr, &attr, &err);
        if (!m_pa)
            return QStringLiteral("cannot record the sound output: ") + QString::fromUtf8(m_strerror ? m_strerror(err) : "");
        m_run = true;
        m_thread = std::thread([this, frag]() {
            std::vector<int16_t> buf(frag / 2);
            int e = 0;
            while (m_run.load()) {
                if (m_read(m_pa, buf.data(), frag, &e) < 0)
                    break;
                bl_capture_push(buf.data(), buf.size() / 2, 2, kRate);
            }
        });
        return {};
    }

    void stop()
    {
        m_run = false;
        if (m_thread.joinable())
            m_thread.join();
        if (m_pa) {
            m_free(m_pa);
            m_pa = nullptr;
        }
    }

private:
    struct SampleSpec { int format; uint32_t rate; uint8_t channels; };
    struct BufferAttr { uint32_t maxlength, tlength, prebuf, minreq, fragsize; };
    using NewFn = void *(*)(const char *, const char *, int, const char *, const char *, const SampleSpec *,
                            const void *, const BufferAttr *, int *);
    using ReadFn = int (*)(void *, void *, size_t, int *);
    using FreeFn = void (*)(void *);
    using StrerrorFn = const char *(*)(int);
    static constexpr uint32_t kRate = 48000;

    bool load()
    {
        if (m_new)
            return true;
        void *lib = dlopen("libpulse-simple.so.0", RTLD_NOW | RTLD_LOCAL);
        if (!lib)
            return false;
        m_new = reinterpret_cast<NewFn>(dlsym(lib, "pa_simple_new"));
        m_read = reinterpret_cast<ReadFn>(dlsym(lib, "pa_simple_read"));
        m_free = reinterpret_cast<FreeFn>(dlsym(lib, "pa_simple_free"));
        m_strerror = reinterpret_cast<StrerrorFn>(dlsym(lib, "pa_strerror"));
        if (!m_new || !m_read || !m_free) {
            m_new = nullptr;
            return false;
        }
        return true;
    }

    NewFn m_new = nullptr;
    ReadFn m_read = nullptr;
    FreeFn m_free = nullptr;
    StrerrorFn m_strerror = nullptr;
    void *m_pa = nullptr;
    std::atomic<bool> m_run { false };
    std::thread m_thread;
};

constexpr const char *kPath = "/org/mpris/MediaPlayer2";
constexpr const char *kPlayer = "org.mpris.MediaPlayer2.Player";
constexpr const char *kRoot = "org.mpris.MediaPlayer2";
constexpr const char *kProps = "org.freedesktop.DBus.Properties";

class Capture : public QObject
{
public:
    explicit Capture(Bridge *bridge) : QObject(bridge), m_bridge(bridge)
    {
        m_poll.setInterval(1000);
        connect(&m_poll, &QTimer::timeout, this, &Capture::poll);
    }

    void start()
    {
        stop(false);
        const QString error = m_recorder.start();
        if (!error.isEmpty()) {
            qWarning("capture: %s", qPrintable(error));
            postState(false, error);
            return;
        }
        qInfo("capture: recording the sound output");
        post(QStringLiteral("capture.access"), { { QStringLiteral("granted"), true } });
        postState(true);
        m_lastJson.clear();
        poll();
        m_poll.start();
    }

    void stop(bool report = true)
    {
        m_poll.stop();
        m_recorder.stop();
        m_player.clear();
        if (report) {
            postState(false);
            post(QStringLiteral("capture.session"), { { QStringLiteral("session"), QVariant() } });
        }
    }

    void control(const QString &action, double position)
    {
        poll(); // the app that plays now
        if (m_player.isEmpty())
            return;
        static const QHash<QString, QString> methods = {
            { QStringLiteral("play"), QStringLiteral("Play") },
            { QStringLiteral("pause"), QStringLiteral("Pause") },
            { QStringLiteral("toggle"), QStringLiteral("PlayPause") },
            { QStringLiteral("next"), QStringLiteral("Next") },
            { QStringLiteral("prev"), QStringLiteral("Previous") },
        };
        QDBusMessage m;
        if (action == QLatin1String("seek")) {
            if (m_trackId.isEmpty())
                return;
            m = QDBusMessage::createMethodCall(m_player, QLatin1String(kPath), QLatin1String(kPlayer), QStringLiteral("SetPosition"));
            m << QVariant::fromValue(QDBusObjectPath(m_trackId)) << qlonglong(position * 1e6);
        } else if (methods.contains(action)) {
            m = QDBusMessage::createMethodCall(m_player, QLatin1String(kPath), QLatin1String(kPlayer), methods.value(action));
        } else {
            return;
        }
        QDBusConnection::sessionBus().call(m, QDBus::NoBlock);
        QTimer::singleShot(300, this, &Capture::poll);
    }

private:
    // Runs a core command on the Qt thread (not inside the current one).
    void post(const QString &cmd, const QVariantMap &args)
    {
        QPointer<Bridge> b = m_bridge;
        QMetaObject::invokeMethod(m_bridge, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
    }

    void postState(bool running, const QString &error = {})
    {
        post(QStringLiteral("capture.state"), { { QStringLiteral("running"), running }, { QStringLiteral("error"), error } });
    }

    static QVariant property(const QString &service, const char *iface, const char *name)
    {
        QDBusMessage m = QDBusMessage::createMethodCall(service, QLatin1String(kPath), QLatin1String(kProps), QStringLiteral("Get"));
        m << QLatin1String(iface) << QLatin1String(name);
        const QDBusMessage r = QDBusConnection::sessionBus().call(m, QDBus::Block, 500);
        if (r.type() != QDBusMessage::ReplyMessage || r.arguments().isEmpty())
            return {};
        return r.arguments().first().value<QDBusVariant>().variant();
    }

    // Follows the playing MPRIS player (else the one followed so far).
    void poll()
    {
        QDBusConnectionInterface *bus = QDBusConnection::sessionBus().interface();
        if (!bus)
            return;
        const QStringList names = bus->registeredServiceNames().value();
        QString best;
        for (const QString &n : names) {
            if (!n.startsWith(QLatin1String("org.mpris.MediaPlayer2.")) || n.startsWith(QLatin1String("org.mpris.MediaPlayer2.beatlumen")))
                continue;
            const QString status = property(n, kPlayer, "PlaybackStatus").toString();
            if (status == QLatin1String("Playing")) {
                best = n;
                break;
            }
            if (best.isEmpty() || n == m_player)
                best = n;
        }
        if (best != m_player)
            qInfo("capture: following %s", best.isEmpty() ? "no player" : qPrintable(best));
        m_player = best;
        report();
    }

    void report()
    {
        if (m_player.isEmpty()) {
            m_trackId.clear();
            send(QJsonObject());
            return;
        }
        const QVariantMap meta = qdbus_cast<QVariantMap>(property(m_player, kPlayer, "Metadata"));
        const QString status = property(m_player, kPlayer, "PlaybackStatus").toString();
        const qlonglong pos = property(m_player, kPlayer, "Position").toLongLong();
        const bool canSeek = property(m_player, kPlayer, "CanSeek").toBool();
        QString app = property(m_player, kRoot, "Identity").toString();
        if (app.isEmpty())
            app = m_player.mid(int(qstrlen("org.mpris.MediaPlayer2."))).section(QLatin1Char('.'), 0, 0);
        m_trackId = meta.value(QStringLiteral("mpris:trackid")).value<QDBusObjectPath>().path();
        if (m_trackId.isEmpty())
            m_trackId = meta.value(QStringLiteral("mpris:trackid")).toString();
        QJsonObject o;
        o.insert(QStringLiteral("app"), app);
        o.insert(QStringLiteral("title"), meta.value(QStringLiteral("xesam:title")).toString());
        o.insert(QStringLiteral("artist"), meta.value(QStringLiteral("xesam:artist")).toStringList().join(QStringLiteral(", ")));
        o.insert(QStringLiteral("album"), meta.value(QStringLiteral("xesam:album")).toString());
        o.insert(QStringLiteral("duration"), meta.value(QStringLiteral("mpris:length")).toLongLong() / 1e6);
        o.insert(QStringLiteral("position"), pos / 1e6);
        o.insert(QStringLiteral("playing"), status == QLatin1String("Playing"));
        o.insert(QStringLiteral("canSeek"), canSeek && !m_trackId.isEmpty());
        o.insert(QStringLiteral("cover"), coverFile(meta.value(QStringLiteral("mpris:artUrl")).toString()));
        send(o);
    }

    // The player's cover as a local file: file:// as it is, http(s) once
    // downloaded into the cache.
    QString coverFile(const QString &art)
    {
        const QUrl u(art);
        if (u.isLocalFile())
            return QFileInfo::exists(u.toLocalFile()) ? u.toLocalFile() : QString();
        if (u.scheme() != QLatin1String("http") && u.scheme() != QLatin1String("https"))
            return {};
        const QString dir = QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/capture");
        const QString file = dir + QLatin1Char('/')
            + QString::fromLatin1(QCryptographicHash::hash(art.toUtf8(), QCryptographicHash::Sha256).toHex().left(24)) + QStringLiteral(".img");
        if (QFileInfo::exists(file))
            return file;
        if (!m_fetching.contains(art)) {
            m_fetching.insert(art);
            QPointer<Capture> self = this;
            QThreadPool::globalInstance()->start([self, art, dir, file]() {
                size_t len = 0;
                uint8_t *data = bl_http_fetch(art.toUtf8().constData(), &len);
                if (data && len > 0 && len < (16u << 20)) {
                    QDir().mkpath(dir);
                    QFile f(file);
                    if (f.open(QIODevice::WriteOnly))
                        f.write(reinterpret_cast<const char *>(data), qint64(len));
                }
                if (data)
                    bl_bytes_free(data, len);
                QMetaObject::invokeMethod(qApp, [self]() { if (self) self->report(); }, Qt::QueuedConnection);
            });
        }
        return {};
    }

    void send(const QJsonObject &o)
    {
        // The position moves on its own: unchanged otherwise, only jumps count.
        QJsonObject key = o;
        const double pos = key.take(QStringLiteral("position")).toDouble();
        const QByteArray json = QJsonDocument(key).toJson(QJsonDocument::Compact);
        const double expected = m_lastPos + (m_lastPlaying ? m_posClock.elapsed() / 1000.0 : 0.0);
        if (json == m_lastJson && std::abs(pos - expected) < 1.5)
            return;
        m_lastJson = json;
        m_lastPos = pos;
        m_lastPlaying = o.value(QStringLiteral("playing")).toBool();
        m_posClock.restart();
        post(QStringLiteral("capture.session"),
             { { QStringLiteral("session"), o.isEmpty() ? QVariant() : QVariant(o.toVariantMap()) } });
    }

    Bridge *m_bridge;
    PulseRecorder m_recorder;
    QTimer m_poll;
    QString m_player;
    QString m_trackId;
    QByteArray m_lastJson;
    double m_lastPos = 0;
    bool m_lastPlaying = false;
    QElapsedTimer m_posClock;
    QSet<QString> m_fetching;
};

QPointer<Capture> g_capture;

int32_t captureRequest(void *, const char *kind, const char *json)
{
    if (!g_capture)
        return 0;
    const QByteArray k(kind);
    const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
    QPointer<Capture> c = g_capture;
    // Not from within the core's command: the capture reports back to it.
    if (k == "start")
        QMetaObject::invokeMethod(c, [c]() { if (c) c->start(); }, Qt::QueuedConnection);
    else if (k == "stop")
        QMetaObject::invokeMethod(c, [c]() { if (c) c->stop(); }, Qt::QueuedConnection);
    else if (k == "access")
        return 1; // nothing to allow on the desktop
    else if (k == "control") {
        const QString action = o.value(QStringLiteral("action")).toString();
        const double pos = o.value(QStringLiteral("position")).toDouble();
        QMetaObject::invokeMethod(c, [c, action, pos]() { if (c) c->control(action, pos); }, Qt::QueuedConnection);
    } else
        return 0;
    return 1;
}

} // namespace

namespace desktopcapture {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    if (!QDBusConnection::sessionBus().isConnected())
        return;
    g_capture = new Capture(bridge);
    cb.capture_request = captureRequest;
}

} // namespace desktopcapture
