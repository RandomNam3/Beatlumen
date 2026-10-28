#include "media_desktop.h"
#include "bridge.h"
#include "tls.h"

#include <QCoreApplication>
#include <QCryptographicHash>
#include <QDir>
#include <QFile>
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QStandardPaths>
#include <QUrl>
#include <QWindow>

#include <string>

// ---- platform backends --------------------------------------------------

#if defined(BEATLUMEN_MPRIS)

#include <QDBusAbstractAdaptor>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusObjectPath>

namespace {

constexpr const char *kPath = "/org/mpris/MediaPlayer2";

// org.mpris.MediaPlayer2
class MprisRoot : public QDBusAbstractAdaptor
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2")
    Q_PROPERTY(bool CanQuit READ yes)
    Q_PROPERTY(bool CanRaise READ yes)
    Q_PROPERTY(bool HasTrackList READ no)
    Q_PROPERTY(QString Identity READ identity)
    Q_PROPERTY(QString DesktopEntry READ desktopEntry)
    Q_PROPERTY(QStringList SupportedUriSchemes READ none)
    Q_PROPERTY(QStringList SupportedMimeTypes READ none)
public:
    MprisRoot(QObject *parent, DesktopMedia *media)
        : QDBusAbstractAdaptor(parent)
        , m_media(media)
    {
    }
    bool yes() const { return true; }
    bool no() const { return false; }
    QString identity() const { return QStringLiteral("Beatlumen"); }
    QString desktopEntry() const { return QStringLiteral("io.github.randomnam3.beatlumen"); }
    QStringList none() const { return {}; }

public slots:
    void Raise() { m_media->raise(); }
    void Quit() { QCoreApplication::quit(); }

private:
    DesktopMedia *m_media;
};

// org.mpris.MediaPlayer2.Player
class MprisPlayer : public QDBusAbstractAdaptor
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2.Player")
    Q_PROPERTY(QString PlaybackStatus READ playbackStatus)
    Q_PROPERTY(QString LoopStatus READ loopStatus WRITE setLoopStatus)
    Q_PROPERTY(double Rate READ rate WRITE setRate)
    Q_PROPERTY(bool Shuffle READ shuffle WRITE setShuffle)
    Q_PROPERTY(QVariantMap Metadata READ metadata)
    Q_PROPERTY(double Volume READ volume WRITE setVolume)
    Q_PROPERTY(qlonglong Position READ position)
    Q_PROPERTY(double MinimumRate READ rate)
    Q_PROPERTY(double MaximumRate READ rate)
    Q_PROPERTY(bool CanGoNext READ canGoNext)
    Q_PROPERTY(bool CanGoPrevious READ canGoPrevious)
    Q_PROPERTY(bool CanPlay READ canPlay)
    Q_PROPERTY(bool CanPause READ canPlay)
    Q_PROPERTY(bool CanSeek READ canSeek)
    Q_PROPERTY(bool CanControl READ canControl)
public:
    MprisPlayer(QObject *parent, DesktopMedia *media)
        : QDBusAbstractAdaptor(parent)
        , m_media(media)
    {
        connect(media, &DesktopMedia::seeked, this, &MprisPlayer::Seeked);
    }
    const DesktopMedia::Snapshot &s() const { return m_media->snapshot(); }
    QString playbackStatus() const { return s().status; }
    QString loopStatus() const { return s().loop; }
    void setLoopStatus(const QString &v) { m_media->setLoop(v); }
    double rate() const { return 1.0; }
    void setRate(double) {}
    bool shuffle() const { return s().shuffle; }
    void setShuffle(bool on) { m_media->setShuffle(on); }
    double volume() const { return s().volume; }
    void setVolume(double v) { m_media->setVolume(v); }
    qlonglong position() const { return m_media->positionUs(); }
    bool canGoNext() const { return s().canNext; }
    bool canGoPrevious() const { return s().canPrevious; }
    bool canPlay() const { return s().canPlay; }
    bool canSeek() const { return s().canSeek; }
    bool canControl() const { return true; }

    static QDBusObjectPath trackId(const QString &id)
    {
        if (id.isEmpty())
            return QDBusObjectPath(QStringLiteral("/org/mpris/MediaPlayer2/TrackList/NoTrack"));
        // Object paths allow [A-Za-z0-9_] only: a hash of the core's id.
        const QByteArray h = QCryptographicHash::hash(id.toUtf8(), QCryptographicHash::Sha1).toHex().left(16);
        return QDBusObjectPath(QStringLiteral("/io/github/randomnam3/beatlumen/track/t") + QString::fromLatin1(h));
    }

    QVariantMap metadata() const
    {
        const DesktopMedia::Snapshot &m = s();
        QVariantMap md;
        md.insert(QStringLiteral("mpris:trackid"), QVariant::fromValue(trackId(m.id)));
        if (m.id.isEmpty())
            return md;
        if (m.lengthUs > 0)
            md.insert(QStringLiteral("mpris:length"), qlonglong(m.lengthUs));
        md.insert(QStringLiteral("xesam:title"), m.title);
        if (!m.artists.isEmpty())
            md.insert(QStringLiteral("xesam:artist"), m.artists);
        if (!m.album.isEmpty())
            md.insert(QStringLiteral("xesam:album"), m.album);
        if (!m.artUrl.isEmpty())
            md.insert(QStringLiteral("mpris:artUrl"), m.artUrl);
        return md;
    }

signals:
    void Seeked(qlonglong Position);

public slots:
    void Next() { m_media->command(QStringLiteral("next")); }
    void Previous() { m_media->command(QStringLiteral("prev")); }
    void Pause() { m_media->command(QStringLiteral("pause")); }
    void PlayPause() { m_media->command(QStringLiteral("toggle")); }
    void Stop() { m_media->command(QStringLiteral("stop")); }
    void Play() { m_media->command(QStringLiteral("play")); }
    void Seek(qlonglong offset) { m_media->seekBy(offset); }
    void SetPosition(const QDBusObjectPath &track, qlonglong pos)
    {
        // Ignored when the track changed in the meantime (MPRIS rule).
        if (track == trackId(s().id))
            m_media->seekTo(pos);
    }
    void OpenUri(const QString &) {}

private:
    DesktopMedia *m_media;
};

} // namespace

class DesktopMedia::Backend : public QObject
{
public:
    Backend(DesktopMedia *media)
        : m_media(media)
    {
        m_player = new MprisPlayer(this, media);
        new MprisRoot(this, media);
        QDBusConnection bus = QDBusConnection::sessionBus();
        if (!bus.isConnected())
            return;
        if (!bus.registerObject(QLatin1String(kPath), this, QDBusConnection::ExportAdaptors))
            return;
        m_service = QStringLiteral("org.mpris.MediaPlayer2.beatlumen");
        if (!bus.registerService(m_service)) {
            // Another instance runs already: MPRIS allows one name each.
            m_service += QStringLiteral(".instance") + QString::number(QCoreApplication::applicationPid());
            if (!bus.registerService(m_service))
                m_service.clear();
        }
        connect(media, &DesktopMedia::changed, this, &Backend::changed);
    }
    ~Backend() override
    {
        if (!m_service.isEmpty())
            QDBusConnection::sessionBus().unregisterService(m_service);
    }
    void setWindow(QWindow *) {}

private:
    void changed(const DesktopMedia::Snapshot &b)
    {
        const DesktopMedia::Snapshot &a = m_media->snapshot();
        QVariantMap props;
        if (a.status != b.status)
            props.insert(QStringLiteral("PlaybackStatus"), a.status);
        if (a.loop != b.loop)
            props.insert(QStringLiteral("LoopStatus"), a.loop);
        if (a.shuffle != b.shuffle)
            props.insert(QStringLiteral("Shuffle"), a.shuffle);
        if (a.volume != b.volume)
            props.insert(QStringLiteral("Volume"), a.volume);
        if (a.id != b.id || a.title != b.title || a.album != b.album || a.artists != b.artists || a.artUrl != b.artUrl
            || a.lengthUs != b.lengthUs)
            props.insert(QStringLiteral("Metadata"), m_player->metadata());
        if (a.canNext != b.canNext)
            props.insert(QStringLiteral("CanGoNext"), a.canNext);
        if (a.canPrevious != b.canPrevious)
            props.insert(QStringLiteral("CanGoPrevious"), a.canPrevious);
        if (a.canPlay != b.canPlay) {
            props.insert(QStringLiteral("CanPlay"), a.canPlay);
            props.insert(QStringLiteral("CanPause"), a.canPlay);
        }
        if (a.canSeek != b.canSeek)
            props.insert(QStringLiteral("CanSeek"), a.canSeek);
        if (props.isEmpty())
            return;
        QDBusMessage sig = QDBusMessage::createSignal(QLatin1String(kPath), QStringLiteral("org.freedesktop.DBus.Properties"),
                                                      QStringLiteral("PropertiesChanged"));
        sig << QStringLiteral("org.mpris.MediaPlayer2.Player") << props << QStringList();
        QDBusConnection::sessionBus().send(sig);
    }

    DesktopMedia *m_media;
    MprisPlayer *m_player = nullptr;
    QString m_service;
};

#elif defined(BEATLUMEN_SMTC)

#include <windows.h>
#include <roapi.h>
#include <shcore.h>
#include <systemmediatransportcontrolsinterop.h>
#include <windows.foundation.h>
#include <windows.media.h>
#include <windows.storage.streams.h>
#include <wrl/client.h>
#include <wrl/event.h>
#include <wrl/wrappers/corewrappers.h>

using Microsoft::WRL::Callback;
using Microsoft::WRL::ComPtr;
using Microsoft::WRL::Wrappers::HString;
using Microsoft::WRL::Wrappers::HStringReference;
namespace wm = ABI::Windows::Media;
namespace wf = ABI::Windows::Foundation;
namespace ws = ABI::Windows::Storage::Streams;

class DesktopMedia::Backend : public QObject
{
public:
    Backend(DesktopMedia *media)
        : m_media(media)
    {
        connect(media, &DesktopMedia::changed, this, [this](const DesktopMedia::Snapshot &b) { changed(b, false); });
        connect(media, &DesktopMedia::seeked, this, [this](qint64) { timeline(); });
    }
    ~Backend() override
    {
        if (m_smtc && m_token.value)
            m_smtc->remove_ButtonPressed(m_token);
    }

    void setWindow(QWindow *window)
    {
        if (m_smtc || !window)
            return;
        ComPtr<ISystemMediaTransportControlsInterop> interop;
        HRESULT hr = RoGetActivationFactory(HStringReference(RuntimeClass_Windows_Media_SystemMediaTransportControls).Get(),
                                            IID_PPV_ARGS(&interop));
        if (FAILED(hr) || !interop)
            return;
        hr = interop->GetForWindow(reinterpret_cast<HWND>(window->winId()), IID_PPV_ARGS(&m_smtc));
        if (FAILED(hr) || !m_smtc)
            return;
        m_smtc->put_IsPlayEnabled(true);
        m_smtc->put_IsPauseEnabled(true);
        m_smtc->put_IsStopEnabled(true);
        QPointer<DesktopMedia> media = m_media;
        auto handler = Callback<wf::ITypedEventHandler<wm::SystemMediaTransportControls *,
                                                       wm::SystemMediaTransportControlsButtonPressedEventArgs *>>(
            [media](wm::ISystemMediaTransportControls *, wm::ISystemMediaTransportControlsButtonPressedEventArgs *args) -> HRESULT {
                wm::SystemMediaTransportControlsButton b;
                if (FAILED(args->get_Button(&b)))
                    return S_OK;
                QString cmd;
                switch (b) {
                case wm::SystemMediaTransportControlsButton_Play: cmd = QStringLiteral("play"); break;
                case wm::SystemMediaTransportControlsButton_Pause: cmd = QStringLiteral("pause"); break;
                case wm::SystemMediaTransportControlsButton_Stop: cmd = QStringLiteral("stop"); break;
                case wm::SystemMediaTransportControlsButton_Next: cmd = QStringLiteral("next"); break;
                case wm::SystemMediaTransportControlsButton_Previous: cmd = QStringLiteral("prev"); break;
                default: return S_OK;
                }
                // A thread of the system: DesktopMedia::command queues it.
                if (media)
                    media->command(cmd);
                return S_OK;
            });
        m_smtc->add_ButtonPressed(handler.Get(), &m_token);
        m_smtc.As(&m_smtc2);
        if (m_smtc2) {
            auto seek = Callback<wf::ITypedEventHandler<wm::SystemMediaTransportControls *, wm::PlaybackPositionChangeRequestedEventArgs *>>(
                [media](wm::ISystemMediaTransportControls *, wm::IPlaybackPositionChangeRequestedEventArgs *args) -> HRESULT {
                    wf::TimeSpan t;
                    if (media && SUCCEEDED(args->get_RequestedPlaybackPosition(&t)))
                        media->seekTo(t.Duration / 10);
                    return S_OK;
                });
            m_smtc2->add_PlaybackPositionChangeRequested(seek.Get(), &m_seekToken);
        }
        changed(m_media->snapshot(), true);
    }

private:
    static HString str(const QString &s)
    {
        HString h;
        h.Set(reinterpret_cast<const wchar_t *>(s.utf16()), UINT32(s.size()));
        return h;
    }

    void changed(const DesktopMedia::Snapshot &b, bool force)
    {
        if (!m_smtc)
            return;
        const DesktopMedia::Snapshot &a = m_media->snapshot();
        m_smtc->put_IsEnabled(!a.id.isEmpty());
        m_smtc->put_IsNextEnabled(a.canNext);
        m_smtc->put_IsPreviousEnabled(a.canPrevious);
        wm::MediaPlaybackStatus st = a.status == QLatin1String("Playing") ? wm::MediaPlaybackStatus_Playing
                                   : a.status == QLatin1String("Paused") ? wm::MediaPlaybackStatus_Paused
                                   : a.id.isEmpty() ? wm::MediaPlaybackStatus_Closed
                                   : wm::MediaPlaybackStatus_Stopped;
        m_smtc->put_PlaybackStatus(st);
        if (force || a.id != b.id || a.title != b.title || a.album != b.album || a.artists != b.artists || a.artUrl != b.artUrl)
            display(a);
        if (force || a.id != b.id || a.lengthUs != b.lengthUs || a.status != b.status)
            timeline();
    }

    void display(const DesktopMedia::Snapshot &a)
    {
        ComPtr<wm::ISystemMediaTransportControlsDisplayUpdater> up;
        if (FAILED(m_smtc->get_DisplayUpdater(&up)) || !up)
            return;
        up->ClearAll();
        if (!a.id.isEmpty()) {
            up->put_Type(wm::MediaPlaybackType_Music);
            ComPtr<wm::IMusicDisplayProperties> music;
            if (SUCCEEDED(up->get_MusicProperties(&music)) && music) {
                const QString artist = a.artists.join(QStringLiteral(", "));
                music->put_Title(str(a.title).Get());
                music->put_Artist(str(artist).Get());
                ComPtr<wm::IMusicDisplayProperties2> music2;
                if (SUCCEEDED(music.As(&music2)) && music2)
                    music2->put_AlbumTitle(str(a.album).Get());
            }
            const QString file = QUrl(a.artUrl).toLocalFile();
            if (!file.isEmpty()) {
                ComPtr<ws::IRandomAccessStream> stream;
                ComPtr<ws::IRandomAccessStreamReferenceStatics> refs;
                ComPtr<ws::IRandomAccessStreamReference> ref;
                const std::wstring path = QDir::toNativeSeparators(file).toStdWString();
                if (SUCCEEDED(CreateRandomAccessStreamOnFile(path.c_str(), 0 /* FileAccessMode::Read */, IID_PPV_ARGS(&stream)))
                    && SUCCEEDED(RoGetActivationFactory(HStringReference(RuntimeClass_Windows_Storage_Streams_RandomAccessStreamReference).Get(),
                                                        IID_PPV_ARGS(&refs)))
                    && SUCCEEDED(refs->CreateFromStream(stream.Get(), &ref)))
                    up->put_Thumbnail(ref.Get());
            }
        }
        up->Update();
    }

    void timeline()
    {
        if (!m_smtc2)
            return;
        const DesktopMedia::Snapshot &a = m_media->snapshot();
        ComPtr<IInspectable> inst;
        if (FAILED(RoActivateInstance(HStringReference(RuntimeClass_Windows_Media_SystemMediaTransportControlsTimelineProperties).Get(), &inst)))
            return;
        ComPtr<wm::ISystemMediaTransportControlsTimelineProperties> tl;
        if (FAILED(inst.As(&tl)))
            return;
        // TimeSpan counts 100 ns.
        tl->put_StartTime(wf::TimeSpan{0});
        tl->put_MinSeekTime(wf::TimeSpan{0});
        tl->put_EndTime(wf::TimeSpan{a.lengthUs * 10});
        tl->put_MaxSeekTime(wf::TimeSpan{a.lengthUs * 10});
        tl->put_Position(wf::TimeSpan{m_media->positionUs() * 10});
        m_smtc2->UpdateTimelineProperties(tl.Get());
    }

    DesktopMedia *m_media;
    ComPtr<wm::ISystemMediaTransportControls> m_smtc;
    ComPtr<wm::ISystemMediaTransportControls2> m_smtc2;
    EventRegistrationToken m_token{};
    EventRegistrationToken m_seekToken{};
};

#else

class DesktopMedia::Backend : public QObject
{
public:
    Backend(DesktopMedia *) {}
    void setWindow(QWindow *) {}
};

#endif

// ---- shared ---------------------------------------------------------------

DesktopMedia::DesktopMedia(Bridge *bridge)
    : QObject(bridge)
    , m_bridge(bridge)
{
    m_backend = new Backend(this);
    m_backend->setParent(this);
    connect(bridge, &Bridge::stateChanged, this, &DesktopMedia::sync);
    connect(bridge, &Bridge::positionChanged, this, &DesktopMedia::checkPosition);
    connect(bridge, &Bridge::coverReady, this, [this](const QString &id, const QString &url) {
        if (id == m_coverId) {
            m_coverUrl = url;
            sync();
        }
    });
    m_posClock.start();
}

DesktopMedia::~DesktopMedia() = default;

void DesktopMedia::setWindow(QWindow *window)
{
    m_window = window;
    m_backend->setWindow(window);
}

qint64 DesktopMedia::positionUs() const
{
    return qint64(m_bridge->position() * 1e6);
}

void DesktopMedia::command(const QString &cmd)
{
    QMetaObject::invokeMethod(this, [this, cmd]() { m_bridge->call(cmd); }, Qt::QueuedConnection);
}

void DesktopMedia::seekTo(qint64 us)
{
    QMetaObject::invokeMethod(this, [this, us]() {
        const double len = m_last.lengthUs / 1e6;
        double s = qMax(0.0, us / 1e6);
        if (len > 0 && s > len)
            return; // MPRIS: past the end does nothing
        m_bridge->call(QStringLiteral("seek"), {{QStringLiteral("position"), s}});
    }, Qt::QueuedConnection);
}

void DesktopMedia::seekBy(qint64 us)
{
    QMetaObject::invokeMethod(this, [this, us]() {
        const double target = m_bridge->position() + us / 1e6;
        if (m_last.lengthUs > 0 && target * 1e6 > m_last.lengthUs)
            m_bridge->call(QStringLiteral("next"));
        else
            m_bridge->call(QStringLiteral("seek"), {{QStringLiteral("position"), qMax(0.0, target)}});
    }, Qt::QueuedConnection);
}

void DesktopMedia::setVolume(double v)
{
    QMetaObject::invokeMethod(this, [this, v]() {
        m_bridge->call(QStringLiteral("volume"), {{QStringLiteral("value"), qBound(0.0, v, 1.0)}});
    }, Qt::QueuedConnection);
}

void DesktopMedia::setShuffle(bool on)
{
    QMetaObject::invokeMethod(this, [this, on]() {
        m_bridge->call(QStringLiteral("shuffle"), {{QStringLiteral("value"), on}});
    }, Qt::QueuedConnection);
}

void DesktopMedia::setLoop(const QString &mpris)
{
    const QString v = mpris == QLatin1String("Track") ? QStringLiteral("Item")
                    : mpris == QLatin1String("Playlist") ? QStringLiteral("Queue")
                    : QStringLiteral("None");
    QMetaObject::invokeMethod(this, [this, v]() {
        m_bridge->call(QStringLiteral("repeat"), {{QStringLiteral("value"), v}});
    }, Qt::QueuedConnection);
}

void DesktopMedia::raise()
{
    QMetaObject::invokeMethod(this, [this]() {
        if (!m_window)
            return;
        if (m_window->visibility() == QWindow::Minimized || m_window->visibility() == QWindow::Hidden)
            m_window->showNormal();
        m_window->raise();
        m_window->requestActivate();
    }, Qt::QueuedConnection);
}

void DesktopMedia::sync()
{
    const QVariantMap st = m_bridge->state();
    const QVariantMap cur = st.value(QStringLiteral("current")).toMap();
    const QString play = st.value(QStringLiteral("play")).toString();
    const int index = st.value(QStringLiteral("queueIndex")).toInt();
    const int length = st.value(QStringLiteral("queueLength")).toInt();
    const QString repeat = st.value(QStringLiteral("repeat")).toString();

    Snapshot s;
    s.id = cur.value(QStringLiteral("id")).toString();
    if (!s.id.isEmpty()) {
        s.title = cur.value(QStringLiteral("title")).toString();
        const QString artist = cur.value(QStringLiteral("artist")).toString();
        if (!artist.isEmpty())
            s.artists = artist.split(QStringLiteral(", "));
        s.album = cur.value(QStringLiteral("album")).toString();
        s.status = play == QLatin1String("playing") || play == QLatin1String("loading") ? QStringLiteral("Playing")
                 : play == QLatin1String("paused") ? QStringLiteral("Paused")
                 : QStringLiteral("Stopped");

        // Cover: an extracted local file, or a server image. Server URLs
        // carry the login token: they are downloaded here, never shown.
        QString cover = cur.value(QStringLiteral("cover")).toString();
        if (s.id != m_coverId) {
            m_coverId = s.id;
            m_coverUrl.clear();
            if (cover.isEmpty() && cur.value(QStringLiteral("local")).toBool())
                m_bridge->call(QStringLiteral("covers"), {{QStringLiteral("ids"), QVariantList{s.id}}});
        }
        if (cover.isEmpty())
            cover = m_coverUrl;
        if (cover.startsWith(QLatin1String("file:")))
            s.artUrl = cover;
        else if (cover.startsWith(QLatin1String("http://")) || cover.startsWith(QLatin1String("https://"))) {
            fetchCover(cover);
            if (cover == m_coverSource && !m_coverFile.isEmpty())
                s.artUrl = QUrl::fromLocalFile(m_coverFile).toString();
        }
    } else {
        m_coverId.clear();
        m_coverUrl.clear();
    }
    s.lengthUs = qint64(st.value(QStringLiteral("duration")).toDouble() * 1e6);
    s.loop = repeat == QLatin1String("Item") ? QStringLiteral("Track")
           : repeat == QLatin1String("Queue") ? QStringLiteral("Playlist")
           : QStringLiteral("None");
    s.shuffle = st.value(QStringLiteral("shuffle")).toBool();
    s.volume = st.value(QStringLiteral("volume")).toDouble();
    s.canPlay = !s.id.isEmpty() || length > 0;
    s.canNext = length > 0 && (index + 1 < length || repeat == QLatin1String("Queue") || s.shuffle);
    s.canPrevious = !s.id.isEmpty();
    s.canSeek = !s.id.isEmpty() && s.lengthUs > 0;

    const double pos = m_bridge->position();
    if (s == m_last)
        return;
    const Snapshot before = m_last;
    m_last = s;
    m_lastPos = pos;
    m_posClock.restart();
    emit changed(before);
}

// Players extrapolate the position while playing; only jumps (seeks,
// restarts) are announced.
void DesktopMedia::checkPosition()
{
    const double pos = m_bridge->position();
    const double expected = m_lastPos + (m_last.status == QLatin1String("Playing") ? m_posClock.elapsed() / 1000.0 : 0.0);
    if (qAbs(pos - expected) > 1.0)
        emit seeked(qint64(pos * 1e6));
    if (qAbs(pos - expected) > 1.0 || m_last.status != QLatin1String("Playing")) {
        m_lastPos = pos;
        m_posClock.restart();
    }
}

void DesktopMedia::fetchCover(const QString &url)
{
    if (url == m_coverSource)
        return;
    m_coverSource = url;
    m_coverFile.clear();
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/media-covers");
    QDir().mkpath(dir);
    // Named after the URL without its query (the token changes).
    const QString key = url.section(QLatin1Char('?'), 0, 0);
    const QString file = dir + QLatin1Char('/') + QString::fromLatin1(QCryptographicHash::hash(key.toUtf8(), QCryptographicHash::Sha256).toHex().left(32));
    if (QFile::exists(file)) {
        m_coverFile = file;
        return;
    }
#if !defined(Q_OS_WASM) && !defined(Q_OS_ANDROID)
    if (!m_net)
        m_net = tls::pinnedNetworkAccess(this);
    QNetworkReply *r = m_net->get(QNetworkRequest(QUrl(url)));
    connect(r, &QNetworkReply::finished, this, [this, r, url, file, dir]() {
        r->deleteLater();
        const QByteArray data = r->readAll();
        if (r->error() != QNetworkReply::NoError || data.isEmpty() || data.size() > 16 * 1024 * 1024)
            return;
        // Keep the cache small: covers of the last few tracks.
        QDir d(dir);
        const QFileInfoList old = d.entryInfoList(QDir::Files, QDir::Time);
        for (int i = 20; i < old.size(); ++i)
            QFile::remove(old[i].absoluteFilePath());
        QFile f(file + QStringLiteral(".part"));
        if (!f.open(QIODevice::WriteOnly) || f.write(data) != data.size())
            return;
        f.close();
        QFile::remove(file);
        if (!f.rename(file))
            return;
        if (url == m_coverSource) {
            m_coverFile = file;
            m_last.artUrl.clear(); // force an update
            sync();
        }
    });
#endif
}

#if defined(BEATLUMEN_MPRIS)
#include "media_desktop.moc"
#endif
