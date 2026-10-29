// "Visualize other apps" on Windows: WASAPI loopback records the default
// output; the system's media sessions (the same that the volume flyout
// shows) give title, cover, position and the transport controls.
#include "desktop_capture.h"
#include "bridge.h"

#include <QCoreApplication>
#include <QCryptographicHash>
#include <QDir>
#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QStandardPaths>

#include <windows.h>
#include <audioclient.h>
#include <mmreg.h>
#include <winstring.h>
#include <mmdeviceapi.h>
#include <roapi.h>
#include <shcore.h>
#include <windows.media.control.h>
#include <windows.storage.streams.h>
#include <wrl.h>
#include <wrl/wrappers/corewrappers.h>

#include <atomic>
#include <cmath>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using Microsoft::WRL::ComPtr;
using Microsoft::WRL::Wrappers::HStringReference;
namespace AF = ABI::Windows::Foundation;
namespace MC = ABI::Windows::Media::Control;
namespace SS = ABI::Windows::Storage::Streams;

namespace {

QPointer<Bridge> g_bridge;

void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

void postState(bool running, const QString &error = {})
{
    post(QStringLiteral("capture.state"), { { QStringLiteral("running"), running }, { QStringLiteral("error"), error } });
}

QString fromHString(HSTRING h)
{
    UINT32 n = 0;
    const wchar_t *s = WindowsGetStringRawBuffer(h, &n);
    return QString::fromWCharArray(s, int(n));
}

// Waits for a WinRT operation (on a worker thread).
template <typename Op, typename R>
HRESULT awaitResult(Op *op, R *result, int timeoutMs = 3000)
{
    if (!op)
        return E_POINTER;
    ComPtr<AF::IAsyncInfo> info;
    HRESULT hr = op->QueryInterface(IID_PPV_ARGS(&info));
    if (FAILED(hr))
        return hr;
    ::AsyncStatus st = ::AsyncStatus::Started;
    for (int waited = 0; waited < timeoutMs; waited += 5) {
        info->get_Status(&st);
        if (st != ::AsyncStatus::Started)
            break;
        Sleep(5);
    }
    if (st != ::AsyncStatus::Completed)
        return E_FAIL;
    return op->GetResults(result);
}

// ---- recording (WASAPI loopback) --------------------------------------------------

class Recorder
{
public:
    void start()
    {
        stop();
        m_running = true;
        m_thread = std::thread([this]() { run(); });
    }

    void stop()
    {
        m_running = false;
        if (m_thread.joinable())
            m_thread.join();
    }

private:
    void run()
    {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        QString error;
        if (!record(error)) {
            m_running = false;
            postState(false, error);
        }
        CoUninitialize();
    }

    bool record(QString &error)
    {
        ComPtr<IMMDeviceEnumerator> en;
        ComPtr<IMMDevice> dev;
        ComPtr<IAudioClient> client;
        ComPtr<IAudioCaptureClient> capture;
        if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&en)))
            || FAILED(en->GetDefaultAudioEndpoint(eRender, eConsole, &dev))
            || FAILED(dev->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(client.GetAddressOf())))) {
            error = QStringLiteral("no sound output to record");
            return false;
        }
        WAVEFORMATEX *wfx = nullptr;
        if (FAILED(client->GetMixFormat(&wfx))) {
            error = QStringLiteral("unknown output format");
            return false;
        }
        bool isFloat = wfx->wFormatTag == WAVE_FORMAT_IEEE_FLOAT;
        if (wfx->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
            const auto *ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE *>(wfx);
            // The subformat GUIDs carry the format tag in their first field.
            isFloat = ext->SubFormat.Data1 == WAVE_FORMAT_IEEE_FLOAT;
        }
        const int channels = wfx->nChannels;
        const int bits = wfx->wBitsPerSample;
        const uint32_t rate = wfx->nSamplesPerSec;
        // 20 ms buffer: the visuals follow closely.
        HRESULT hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200000, 0, wfx, nullptr);
        CoTaskMemFree(wfx);
        if (FAILED(hr) || FAILED(client->GetService(IID_PPV_ARGS(&capture))) || FAILED(client->Start())) {
            error = QStringLiteral("cannot record the sound output");
            return false;
        }
        if (!(isFloat && bits == 32) && bits != 16) {
            client->Stop();
            error = QStringLiteral("unsupported output format");
            return false;
        }
        postState(true);
        std::vector<int16_t> pcm;
        while (m_running) {
            Sleep(10);
            UINT32 packet = 0;
            while (m_running && SUCCEEDED(capture->GetNextPacketSize(&packet)) && packet > 0) {
                BYTE *data = nullptr;
                UINT32 frames = 0;
                DWORD flags = 0;
                if (FAILED(capture->GetBuffer(&data, &frames, &flags, nullptr, nullptr)))
                    break;
                pcm.assign(size_t(frames) * 2, 0);
                if (!(flags & AUDCLNT_BUFFERFLAGS_SILENT) && data) {
                    for (UINT32 f = 0; f < frames; ++f) {
                        for (int c = 0; c < 2; ++c) {
                            const int src = c < channels ? c : 0;
                            int16_t v;
                            if (isFloat) {
                                const float x = reinterpret_cast<const float *>(data)[f * channels + src];
                                v = int16_t(std::lround(std::fmax(-1.0f, std::fmin(1.0f, x)) * 32767.0f));
                            } else {
                                v = reinterpret_cast<const int16_t *>(data)[f * channels + src];
                            }
                            pcm[size_t(f) * 2 + c] = v;
                        }
                    }
                }
                capture->ReleaseBuffer(frames);
                if (frames > 0)
                    bl_capture_push(pcm.data(), frames, 2, rate);
            }
        }
        client->Stop();
        return true;
    }

    std::atomic<bool> m_running{false};
    std::thread m_thread;
};

// ---- the playing app's media session ---------------------------------------------------

class Sessions
{
public:
    void start()
    {
        stop();
        m_running = true;
        m_thread = std::thread([this]() { run(); });
    }

    void stop()
    {
        m_running = false;
        if (m_thread.joinable())
            m_thread.join();
        m_lastJson.clear();
    }

    void control(const QString &action, double position)
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        m_pending.push_back({ action, position });
    }

private:
    struct Command
    {
        QString action;
        double position;
    };

    void run()
    {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        ComPtr<MC::IGlobalSystemMediaTransportControlsSessionManagerStatics> statics;
        ComPtr<AF::IAsyncOperation<MC::GlobalSystemMediaTransportControlsSessionManager *>> op;
        ComPtr<MC::IGlobalSystemMediaTransportControlsSessionManager> mgr;
        if (FAILED(RoGetActivationFactory(
                HStringReference(RuntimeClass_Windows_Media_Control_GlobalSystemMediaTransportControlsSessionManager).Get(),
                IID_PPV_ARGS(&statics)))
            || FAILED(statics->RequestAsync(&op)) || FAILED(awaitResult(op.Get(), mgr.GetAddressOf()))) {
            CoUninitialize();
            return;
        }
        post(QStringLiteral("capture.access"), { { QStringLiteral("granted"), true } });
        int tick = 0;
        while (m_running) {
            std::vector<Command> cmds;
            {
                std::lock_guard<std::mutex> lock(m_mutex);
                cmds.swap(m_pending);
            }
            ComPtr<MC::IGlobalSystemMediaTransportControlsSession> session = pick(mgr.Get());
            for (const Command &c : cmds)
                perform(session.Get(), c);
            if (!cmds.empty() || tick % 10 == 0)
                report(session.Get());
            ++tick;
            Sleep(100);
        }
        post(QStringLiteral("capture.session"), { { QStringLiteral("session"), QVariant() } });
        CoUninitialize();
    }

    static bool isOwn(MC::IGlobalSystemMediaTransportControlsSession *s)
    {
        HSTRING id = nullptr;
        if (FAILED(s->get_SourceAppUserModelId(&id)))
            return false;
        const bool own = fromHString(id).contains(QLatin1String("beatlumen"), Qt::CaseInsensitive);
        WindowsDeleteString(id);
        return own;
    }

    static bool isPlaying(MC::IGlobalSystemMediaTransportControlsSession *s)
    {
        ComPtr<MC::IGlobalSystemMediaTransportControlsSessionPlaybackInfo> info;
        MC::GlobalSystemMediaTransportControlsSessionPlaybackStatus st;
        return SUCCEEDED(s->GetPlaybackInfo(&info)) && SUCCEEDED(info->get_PlaybackStatus(&st))
            && st == MC::GlobalSystemMediaTransportControlsSessionPlaybackStatus_Playing;
    }

    // The app that plays (else the system's current session), not this one.
    static ComPtr<MC::IGlobalSystemMediaTransportControlsSession> pick(MC::IGlobalSystemMediaTransportControlsSessionManager *mgr)
    {
        ComPtr<AF::Collections::IVectorView<MC::GlobalSystemMediaTransportControlsSession *>> list;
        ComPtr<MC::IGlobalSystemMediaTransportControlsSession> best;
        if (SUCCEEDED(mgr->GetSessions(&list))) {
            UINT32 n = 0;
            list->get_Size(&n);
            for (UINT32 i = 0; i < n; ++i) {
                ComPtr<MC::IGlobalSystemMediaTransportControlsSession> s;
                if (FAILED(list->GetAt(i, &s)) || isOwn(s.Get()))
                    continue;
                if (isPlaying(s.Get()))
                    return s;
                if (!best)
                    best = s;
            }
        }
        ComPtr<MC::IGlobalSystemMediaTransportControlsSession> cur;
        if (SUCCEEDED(mgr->GetCurrentSession(&cur)) && cur && !isOwn(cur.Get()))
            return cur;
        return best;
    }

    static void perform(MC::IGlobalSystemMediaTransportControlsSession *s, const Command &c)
    {
        if (!s)
            return;
        ComPtr<AF::IAsyncOperation<bool>> op;
        if (c.action == QLatin1String("play"))
            s->TryPlayAsync(&op);
        else if (c.action == QLatin1String("pause"))
            s->TryPauseAsync(&op);
        else if (c.action == QLatin1String("toggle"))
            s->TryTogglePlayPauseAsync(&op);
        else if (c.action == QLatin1String("next"))
            s->TrySkipNextAsync(&op);
        else if (c.action == QLatin1String("prev"))
            s->TrySkipPreviousAsync(&op);
        else if (c.action == QLatin1String("seek"))
            s->TryChangePlaybackPositionAsync(INT64(c.position * 1e7), &op);
        boolean ok = false;
        if (op)
            awaitResult(op.Get(), &ok, 1000);
    }

    void report(MC::IGlobalSystemMediaTransportControlsSession *s)
    {
        QJsonObject o;
        if (s) {
            HSTRING id = nullptr;
            QString app;
            if (SUCCEEDED(s->get_SourceAppUserModelId(&id))) {
                app = fromHString(id);
                WindowsDeleteString(id);
            }
            // "Spotify.exe", "SpotifyAB.SpotifyMusic_…!Spotify"
            app = app.section(QLatin1Char('!'), -1);
            if (app.endsWith(QLatin1String(".exe"), Qt::CaseInsensitive))
                app.chop(4);
            o.insert(QStringLiteral("app"), app);
            ComPtr<AF::IAsyncOperation<MC::GlobalSystemMediaTransportControlsSessionMediaProperties *>> op;
            ComPtr<MC::IGlobalSystemMediaTransportControlsSessionMediaProperties> props;
            if (SUCCEEDED(s->TryGetMediaPropertiesAsync(&op)) && SUCCEEDED(awaitResult(op.Get(), props.GetAddressOf())) && props) {
                HSTRING h = nullptr;
                if (SUCCEEDED(props->get_Title(&h))) { o.insert(QStringLiteral("title"), fromHString(h)); WindowsDeleteString(h); }
                if (SUCCEEDED(props->get_Artist(&h))) { o.insert(QStringLiteral("artist"), fromHString(h)); WindowsDeleteString(h); }
                if (SUCCEEDED(props->get_AlbumTitle(&h))) { o.insert(QStringLiteral("album"), fromHString(h)); WindowsDeleteString(h); }
                const QString key = o.value(QStringLiteral("title")).toString() + QLatin1Char('|') + o.value(QStringLiteral("artist")).toString();
                if (key != m_coverKey) {
                    m_coverKey = key;
                    m_coverFile = saveCover(props.Get(), key);
                }
                o.insert(QStringLiteral("cover"), m_coverFile);
            }
            ComPtr<MC::IGlobalSystemMediaTransportControlsSessionPlaybackInfo> info;
            MC::GlobalSystemMediaTransportControlsSessionPlaybackStatus st = MC::GlobalSystemMediaTransportControlsSessionPlaybackStatus_Stopped;
            bool canSeek = false;
            if (SUCCEEDED(s->GetPlaybackInfo(&info))) {
                info->get_PlaybackStatus(&st);
                ComPtr<MC::IGlobalSystemMediaTransportControlsSessionPlaybackControls> controls;
                boolean b = false;
                if (SUCCEEDED(info->get_Controls(&controls)) && SUCCEEDED(controls->get_IsPlaybackPositionEnabled(&b)))
                    canSeek = b;
            }
            const bool playing = st == MC::GlobalSystemMediaTransportControlsSessionPlaybackStatus_Playing;
            ComPtr<MC::IGlobalSystemMediaTransportControlsSessionTimelineProperties> tl;
            double pos = 0, end = 0;
            if (SUCCEEDED(s->GetTimelineProperties(&tl))) {
                AF::TimeSpan p{}, e{};
                AF::DateTime updated{};
                tl->get_Position(&p);
                tl->get_EndTime(&e);
                tl->get_LastUpdatedTime(&updated);
                pos = p.Duration / 1e7;
                end = e.Duration / 1e7;
                if (playing && updated.UniversalTime > 0) {
                    FILETIME ft;
                    GetSystemTimeAsFileTime(&ft);
                    const qint64 now = (qint64(ft.dwHighDateTime) << 32) | ft.dwLowDateTime;
                    pos += std::fmax(0.0, (now - updated.UniversalTime) / 1e7);
                }
            }
            o.insert(QStringLiteral("duration"), end);
            o.insert(QStringLiteral("position"), end > 0 ? std::fmin(pos, end) : pos);
            o.insert(QStringLiteral("playing"), playing);
            o.insert(QStringLiteral("canSeek"), canSeek);
        }
        QJsonObject key = o;
        key.remove(QStringLiteral("position"));
        const QByteArray json = QJsonDocument(key).toJson(QJsonDocument::Compact);
        // Unchanged: once in a while for the position.
        if (json == m_lastJson && ++m_same < 10)
            return;
        m_same = 0;
        m_lastJson = json;
        post(QStringLiteral("capture.session"),
             { { QStringLiteral("session"), o.isEmpty() ? QVariant() : QVariant(o.toVariantMap()) } });
    }

    // The session's thumbnail as a file for the core; "" without one.
    static QString saveCover(MC::IGlobalSystemMediaTransportControlsSessionMediaProperties *props, const QString &key)
    {
        ComPtr<SS::IRandomAccessStreamReference> ref;
        ComPtr<AF::IAsyncOperation<SS::IRandomAccessStreamWithContentType *>> op;
        ComPtr<SS::IRandomAccessStreamWithContentType> stream;
        if (FAILED(props->get_Thumbnail(&ref)) || !ref || FAILED(ref->OpenReadAsync(&op))
            || FAILED(awaitResult(op.Get(), stream.GetAddressOf())) || !stream)
            return {};
        ComPtr<IStream> is;
        if (FAILED(CreateStreamOverRandomAccessStream(stream.Get(), IID_PPV_ARGS(&is))))
            return {};
        QByteArray data;
        char buf[64 * 1024];
        ULONG got = 0;
        while (SUCCEEDED(is->Read(buf, sizeof buf, &got)) && got > 0 && data.size() < (16 << 20))
            data.append(buf, int(got));
        if (data.isEmpty())
            return {};
        const QString dir = QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/capture");
        QDir().mkpath(dir);
        const QString file = dir + QLatin1Char('/')
            + QString::fromLatin1(QCryptographicHash::hash(key.toUtf8(), QCryptographicHash::Sha256).toHex().left(24)) + QStringLiteral(".img");
        QFile f(file);
        if (!f.open(QIODevice::WriteOnly))
            return {};
        f.write(data);
        return file;
    }

    std::atomic<bool> m_running{false};
    std::thread m_thread;
    std::mutex m_mutex;
    std::vector<Command> m_pending;
    QByteArray m_lastJson;
    int m_same = 0;
    QString m_coverKey;
    QString m_coverFile;
};

Recorder *g_recorder = nullptr;
Sessions *g_sessions = nullptr;

int32_t captureRequest(void *, const char *kind, const char *json)
{
    const QByteArray k(kind);
    if (k == "start") {
        g_recorder->start();
        g_sessions->start();
    } else if (k == "stop") {
        g_recorder->stop();
        g_sessions->stop();
        postState(false);
        post(QStringLiteral("capture.session"), { { QStringLiteral("session"), QVariant() } });
    } else if (k == "access") {
        return 1; // nothing to allow
    } else if (k == "control") {
        const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
        g_sessions->control(o.value(QStringLiteral("action")).toString(), o.value(QStringLiteral("position")).toDouble());
    } else {
        return 0;
    }
    return 1;
}

} // namespace

namespace desktopcapture {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    if (!g_recorder) {
        g_recorder = new Recorder;
        g_sessions = new Sessions;
        // Threads end with the app.
        QObject::connect(qApp, &QCoreApplication::aboutToQuit, []() {
            g_recorder->stop();
            g_sessions->stop();
        });
    }
    cb.capture_request = captureRequest;
}

} // namespace desktopcapture
