#include "desktop_capture.h"
#include "bridge.h"

#include <QAudioDevice>
#include <QCoreApplication>
#include <QElapsedTimer>
#include <QSet>
#include <QAudioFormat>
#include <QAudioSource>
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
#include <QMediaDevices>
#include <QPointer>
#include <QStandardPaths>
#include <QThreadPool>
#include <QTimer>
#include <QUrl>

#include <cmath>

namespace {

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
        const QAudioDevice dev = monitorDevice();
        if (dev.isNull()) {
            postState(false, QStringLiteral("no monitor of the sound output (PulseAudio or PipeWire is needed)"));
            return;
        }
        QAudioFormat fmt;
        fmt.setSampleRate(48000);
        fmt.setChannelCount(2);
        fmt.setSampleFormat(QAudioFormat::Int16);
        if (!dev.isFormatSupported(fmt)) {
            const QAudioFormat pref = dev.preferredFormat();
            fmt.setSampleRate(pref.sampleRate() > 0 ? pref.sampleRate() : 48000);
        }
        m_format = fmt;
        m_source = new QAudioSource(dev, fmt, this);
        // Short buffers: the visuals follow what is heard closely.
        m_source->setBufferSize(fmt.bytesForDuration(40000));
        m_io = m_source->start();
        if (!m_io) {
            delete m_source;
            m_source = nullptr;
            postState(false, QStringLiteral("cannot record the sound output"));
            return;
        }
        connect(m_io, &QIODevice::readyRead, this, &Capture::read);
        qInfo("capture: recording %s", qPrintable(dev.description()));
        post(QStringLiteral("capture.access"), { { QStringLiteral("granted"), true } });
        postState(true);
        m_lastJson.clear();
        poll();
        m_poll.start();
    }

    void stop(bool report = true)
    {
        m_poll.stop();
        if (m_source) {
            m_source->stop();
            delete m_source;
            m_source = nullptr;
            m_io = nullptr;
        }
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
    // The monitor of the default output; else any monitor source.
    static QAudioDevice monitorDevice()
    {
        const QByteArray out = QMediaDevices::defaultAudioOutput().id();
        QAudioDevice any;
        for (const QAudioDevice &d : QMediaDevices::audioInputs()) {
            if (!out.isEmpty() && d.id() == out + ".monitor")
                return d;
            if (any.isNull() && (d.id().endsWith(".monitor") || d.description().startsWith(QLatin1String("Monitor of"))))
                any = d;
        }
        return any;
    }

    void read()
    {
        if (!m_io)
            return;
        const QByteArray d = m_io->readAll();
        const int frameBytes = 2 * m_format.channelCount();
        if (d.size() >= frameBytes)
            bl_capture_push(reinterpret_cast<const int16_t *>(d.constData()), size_t(d.size() / frameBytes),
                            uint32_t(m_format.channelCount()), uint32_t(m_format.sampleRate()));
    }

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
    QAudioSource *m_source = nullptr;
    QIODevice *m_io = nullptr;
    QAudioFormat m_format;
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
