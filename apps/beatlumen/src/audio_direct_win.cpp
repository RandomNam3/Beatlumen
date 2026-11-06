// A sound card in direct use on Windows: WASAPI in exclusive mode, event driven.
// No mixer and no conversion in between, and no fall back to shared mode: what
// the device refuses is reported, and the core decides. All of COM happens on the
// output's own thread, whatever thread the core asks from.
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include "audio_direct.h"
#include "bridge.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>

#include <windows.h>
#include <audioclient.h>
#include <avrt.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <propidl.h>
#include <wrl.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <future>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

QPointer<Bridge> g_bridge;

// Runs a core command on the Qt thread.
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

// PKEY_Device_FriendlyName (functiondiscoverykeys_devpkey.h).
const PROPERTYKEY kFriendlyName = { { 0xa45c254e, 0xdf1c, 0x4efd, { 0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0 } }, 14 };
// PKEY_Device_EnumeratorName: the bus of the device ("USB", "HDAUDIO", "BTHENUM").
const PROPERTYKEY kEnumeratorName = { { 0xa45c254e, 0xdf1c, 0x4efd, { 0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0 } }, 24 };
// KSDATAFORMAT_SUBTYPE_PCM (ksmedia.h).
const GUID kSubtypePcm = { 0x00000001, 0x0000, 0x0010, { 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71 } };

const char *const kNotAvailable = "The sound card is not available";
const char *const kInUse = "The sound card is in use by another program";
const char *const kNotTaken = "The sound card does not take this music";
const char *const kNotAllowed = "Windows does not allow exclusive use of this sound card (sound settings, device properties, Advanced)";

QString errorText(HRESULT hr)
{
    if (hr == AUDCLNT_E_DEVICE_IN_USE)
        return QString::fromLatin1(kInUse);
    if (hr == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED)
        return QString::fromLatin1(kNotAllowed);
    if (hr == AUDCLNT_E_UNSUPPORTED_FORMAT)
        return QString::fromLatin1(kNotTaken);
    return QString::fromLatin1(kNotAvailable);
}

// ---- the cards -------------------------------------------------------------

QString takeString(LPWSTR s)
{
    const QString r = s ? QString::fromWCharArray(s) : QString();
    CoTaskMemFree(s);
    return r;
}

QString propertyText(IPropertyStore *props, const PROPERTYKEY &key)
{
    QString text;
    PROPVARIANT v;
    PropVariantInit(&v);
    if (SUCCEEDED(props->GetValue(key, &v)) && v.vt == VT_LPWSTR && v.pwszVal)
        text = QString::fromWCharArray(v.pwszVal);
    PropVariantClear(&v);
    return text;
}

// Looks and opens nothing. The system's default output comes first.
QByteArray enumerate()
{
    QJsonArray list;
    ComPtr<IMMDeviceEnumerator> en;
    ComPtr<IMMDeviceCollection> all;
    if (SUCCEEDED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&en)))
        && SUCCEEDED(en->EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE, &all))) {
        QString defaultId;
        ComPtr<IMMDevice> def;
        if (SUCCEEDED(en->GetDefaultAudioEndpoint(eRender, eConsole, &def))) {
            LPWSTR id = nullptr;
            if (SUCCEEDED(def->GetId(&id)))
                defaultId = takeString(id);
        }
        UINT n = 0;
        all->GetCount(&n);
        for (UINT i = 0; i < n; ++i) {
            ComPtr<IMMDevice> dev;
            LPWSTR rawId = nullptr;
            if (FAILED(all->Item(i, &dev)) || FAILED(dev->GetId(&rawId)))
                continue;
            const QString id = takeString(rawId);
            QString name = id;
            ComPtr<IPropertyStore> props;
            if (SUCCEEDED(dev->OpenPropertyStore(STGM_READ, &props))) {
                // Whatever arrives at a Bluetooth device, its codec changes it.
                if (propertyText(props.Get(), kEnumeratorName).startsWith(QLatin1String("BTH")))
                    continue;
                const QString friendly = propertyText(props.Get(), kFriendlyName);
                if (!friendly.isEmpty())
                    name = friendly;
            }
            QJsonObject o;
            o[QStringLiteral("id")] = id;
            o[QStringLiteral("name")] = name;
            if (id == defaultId)
                list.prepend(o);
            else
                list.append(o);
        }
    }
    return QJsonDocument(list).toJson(QJsonDocument::Compact);
}

QByteArray listJson()
{
    QByteArray json;
    std::thread t([&json]() {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        json = enumerate();
        CoUninitialize();
    });
    t.join();
    return json;
}

// ---- what a device takes -----------------------------------------------------

// Beatlumen's PcmFormat as WASAPI takes it. 24 bits in a word of four sit at its
// top here, which is what S32 writes: S24 (its low bytes) has no counterpart.
struct Layout {
    uint32_t format;
    WORD word;
    WORD valid;
};
const Layout kLayouts[] = { { 0, 16, 16 }, { 1, 24, 24 }, { 3, 32, 32 }, { 3, 32, 24 } };

struct Candidate {
    WAVEFORMATEXTENSIBLE wfx;
    uint32_t format;
};

WAVEFORMATEXTENSIBLE waveFormat(uint32_t rate, const Layout &layout, WORD channels, DWORD mask)
{
    WAVEFORMATEXTENSIBLE w{};
    w.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    w.Format.nChannels = channels;
    w.Format.nSamplesPerSec = rate;
    w.Format.wBitsPerSample = layout.word;
    w.Format.nBlockAlign = WORD(channels * layout.word / 8);
    w.Format.nAvgBytesPerSec = rate * w.Format.nBlockAlign;
    w.Format.cbSize = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);
    w.Samples.wValidBitsPerSample = layout.valid;
    w.dwChannelMask = mask;
    w.SubFormat = kSubtypePcm;
    return w;
}

// The asked rate, then the others by how close they are to it.
std::vector<uint32_t> ratesFrom(uint32_t rate)
{
    std::vector<uint32_t> rates = { 8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000 };
    rates.erase(std::remove(rates.begin(), rates.end(), rate), rates.end());
    std::stable_sort(rates.begin(), rates.end(), [rate](uint32_t a, uint32_t b) {
        return std::abs(int64_t(a) - int64_t(rate)) < std::abs(int64_t(b) - int64_t(rate));
    });
    rates.insert(rates.begin(), rate);
    return rates;
}

// What the device says it takes at the rate nearest to the asked one, best first:
// the format counts first, stereo comes before all of its channels (many
// interfaces only take those). S_OK with at least one, or why there is none.
HRESULT findCandidates(IAudioClient *client, uint32_t rate, const std::vector<uint32_t> &formats, std::vector<Candidate> &out)
{
    WORD deviceChannels = 2;
    DWORD deviceMask = 0;
    WAVEFORMATEX *mix = nullptr;
    if (SUCCEEDED(client->GetMixFormat(&mix))) {
        deviceChannels = mix->nChannels;
        if (mix->wFormatTag == WAVE_FORMAT_EXTENSIBLE)
            deviceMask = reinterpret_cast<const WAVEFORMATEXTENSIBLE *>(mix)->dwChannelMask;
        CoTaskMemFree(mix);
    }
    for (uint32_t r : ratesFrom(rate)) {
        for (uint32_t format : formats) {
            for (const Layout &layout : kLayouts) {
                if (layout.format != format)
                    continue;
                for (int wide = 0; wide < (deviceChannels > 2 ? 2 : 1); ++wide) {
                    const WAVEFORMATEXTENSIBLE w = wide
                        ? waveFormat(r, layout, deviceChannels, deviceMask)
                        : waveFormat(r, layout, 2, SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT);
                    const HRESULT hr = client->IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &w.Format, nullptr);
                    if (hr == S_OK)
                        out.push_back({ w, format });
                    else if (hr == AUDCLNT_E_DEVICE_IN_USE || hr == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED || hr == AUDCLNT_E_DEVICE_INVALIDATED)
                        return hr;
                }
            }
        }
        if (!out.empty())
            return S_OK;
    }
    return AUDCLNT_E_UNSUPPORTED_FORMAT;
}

// ---- the output --------------------------------------------------------------

// How long the device's turns are, if it agrees. Long enough for a busy system.
const REFERENCE_TIME kPeriod = 500000; // 50 ms
const DWORD kWaitMs = 200;
// A device that stops asking without saying why has gone.
const int kMaxQuietWaits = 10;

struct Stream {
    ComPtr<IMMDevice> device;
    ComPtr<IAudioClient> client;
    ComPtr<IAudioRenderClient> render;
    ComPtr<IAudioClock> clock;
    HANDLE ready = nullptr;
    UINT64 clockRate = 0;
    UINT32 frames = 0;
    uint64_t written = 0;
    BlDirectOpened opened{};

    ~Stream()
    {
        clock.Reset();
        render.Reset();
        client.Reset();
        device.Reset();
        if (ready)
            CloseHandle(ready);
    }
};

class Direct
{
public:
    bool open(const std::string &device, uint32_t rate, std::vector<uint32_t> formats, BlDirectOpened *opened, QString *err)
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        if (m_thread.joinable() && !m_failed && !m_idle && device == m_device && rate == m_rate && formats == m_formats) {
            *opened = m_opened;
            return true;
        }
        stopWorker(true);
        m_device = device;
        m_rate = rate;
        m_formats = std::move(formats);
        m_idle = false;
        std::promise<QString> result;
        std::future<QString> answer = result.get_future();
        startWorker(&result);
        *err = answer.get();
        if (!err->isEmpty()) {
            stopWorker(false);
            m_device.clear();
            return false;
        }
        *opened = m_opened;
        return true;
    }

    void close()
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        stopWorker(false);
        m_device.clear();
        m_idle = false;
    }

    void setIdle(bool idle)
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        if (idle == m_idle || m_device.empty())
            return;
        m_idle = idle;
        stopWorker(false);
        if (!idle)
            startWorker(nullptr); // Asking for the card can take seconds.
    }

private:
    void startWorker(std::promise<QString> *result)
    {
        m_run = true;
        m_failed = false;
        m_thread = std::thread([this, result]() { worker(result); });
    }

    void stopWorker(bool playOut)
    {
        m_playOut = playOut;
        m_run = false;
        if (m_thread.joinable())
            m_thread.join();
    }

    void fail(const QString &error)
    {
        m_failed = true;
        post(QStringLiteral("audio.directFailed"), {{QStringLiteral("error"), error}});
    }

    // Without `result` the card comes back after a long pause: the core goes on
    // from what it knows, so anything else than the same is a failure.
    void worker(std::promise<QString> *result)
    {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        // Above the rest where the system allows it; fine without.
        DWORD task = 0;
        HANDLE mmcss = AvSetMmThreadCharacteristicsW(L"Pro Audio", &task);
        {
            Stream s;
            QString err;
            bool ok = openStream(s, &err);
            if (result) {
                if (ok)
                    m_opened = s.opened;
                else
                    m_failed = true;
                result->set_value(err);
            } else if (!ok) {
                if (m_run)
                    fail(err);
            } else if (s.opened.rate != m_opened.rate || s.opened.format != m_opened.format || s.opened.channels != m_opened.channels) {
                ok = false;
                s.client->Stop();
                fail(QString::fromLatin1(kNotTaken));
            }
            if (ok)
                play(s);
        }
        if (mmcss)
            AvRevertMmThreadCharacteristics(mmcss);
        CoUninitialize();
    }

    bool openStream(Stream &s, QString *err)
    {
        ComPtr<IMMDeviceEnumerator> en;
        const QString id = QString::fromStdString(m_device);
        DWORD state = 0;
        if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&en)))
            || FAILED(en->GetDevice(reinterpret_cast<LPCWSTR>(id.utf16()), &s.device))
            || FAILED(s.device->GetState(&state)) || state != DEVICE_STATE_ACTIVE) {
            *err = QString::fromLatin1(kNotAvailable);
            return false;
        }
        // The system's mixer lets go of the device a moment after the last
        // program in it, which may have been this one.
        HRESULT hr = AUDCLNT_E_DEVICE_IN_USE;
        for (int i = 0; i < 20 && hr == AUDCLNT_E_DEVICE_IN_USE && m_run; ++i) {
            if (i > 0)
                std::this_thread::sleep_for(std::chrono::milliseconds(100));
            hr = tryOpen(s);
        }
        if (FAILED(hr)) {
            qWarning("direct output: %s: 0x%08lx", m_device.c_str(), static_cast<unsigned long>(hr));
            *err = errorText(hr);
            return false;
        }
        qInfo("direct output: %s, %u Hz (asked for %u), format %u, %u channels, %u frames at a time", m_device.c_str(), s.opened.rate, m_rate,
              s.opened.format, s.opened.channels, s.frames);
        return true;
    }

    HRESULT tryOpen(Stream &s)
    {
        ComPtr<IAudioClient> probe;
        HRESULT hr = s.device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(probe.GetAddressOf()));
        if (FAILED(hr))
            return hr;
        std::vector<Candidate> candidates;
        hr = findCandidates(probe.Get(), m_rate, m_formats, candidates);
        if (FAILED(hr))
            return hr;
        probe.Reset();
        // Not everything a driver says it takes works.
        for (const Candidate &c : candidates) {
            hr = initialize(s, c.wfx);
            if (SUCCEEDED(hr)) {
                s.opened.rate = c.wfx.Format.nSamplesPerSec;
                s.opened.format = c.format;
                s.opened.channels = c.wfx.Format.nChannels;
                return start(s);
            }
            if (hr != AUDCLNT_E_UNSUPPORTED_FORMAT)
                return hr;
        }
        return hr;
    }

    // A client whose Initialize failed is used up, so each attempt has its own.
    HRESULT initialize(Stream &s, const WAVEFORMATEXTENSIBLE &wfx)
    {
        REFERENCE_TIME period = kPeriod;
        HRESULT hr = E_FAIL;
        for (int attempt = 0; attempt < 4; ++attempt) {
            ComPtr<IAudioClient> client;
            hr = s.device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(client.GetAddressOf()));
            if (FAILED(hr))
                return hr;
            REFERENCE_TIME usual = 0, shortest = 0;
            client->GetDevicePeriod(&usual, &shortest);
            period = std::max(period, shortest);
            hr = client->Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, period, period, &wfx.Format, nullptr);
            if (SUCCEEDED(hr)) {
                s.client = client;
                return hr;
            }
            if (hr == AUDCLNT_E_DEVICE_IN_USE || hr == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED || hr == AUDCLNT_E_UNSUPPORTED_FORMAT
                || hr == AUDCLNT_E_DEVICE_INVALIDATED)
                return hr;
            if (hr == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED) {
                // It tells the nearest length it can do.
                UINT32 frames = 0;
                if (FAILED(client->GetBufferSize(&frames)) || frames == 0)
                    return hr;
                period = REFERENCE_TIME(std::llround(10000000.0 * frames / wfx.Format.nSamplesPerSec));
            } else if (usual > 0 && period != usual) {
                // Some drivers only do their own length.
                period = usual;
            } else {
                return hr;
            }
        }
        return hr;
    }

    HRESULT start(Stream &s)
    {
        s.written = 0;
        if (!s.ready)
            s.ready = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (!s.ready)
            return E_FAIL;
        HRESULT hr = s.client->SetEventHandle(s.ready);
        if (SUCCEEDED(hr))
            hr = s.client->GetBufferSize(&s.frames);
        if (SUCCEEDED(hr))
            hr = s.client->GetService(IID_PPV_ARGS(&s.render));
        if (FAILED(hr))
            return hr;
        // Only for the latency; the position in the buffer stands in without.
        if (FAILED(s.client->GetService(IID_PPV_ARGS(&s.clock))) || FAILED(s.clock->GetFrequency(&s.clockRate)))
            s.clock.Reset();
        // The first turn is filled before the device starts.
        hr = fill(s, false);
        if (SUCCEEDED(hr))
            hr = s.client->Start();
        return hr;
    }

    // What was handed over and has not been heard yet.
    uint32_t queued(const Stream &s) const
    {
        UINT64 position = 0;
        if (s.clock && s.clockRate > 0 && SUCCEEDED(s.clock->GetPosition(&position, nullptr))) {
            const double heard = double(position) * s.opened.rate / double(s.clockRate);
            const double left = double(s.written) - heard;
            if (left >= 0 && left < double(s.opened.rate) * 2)
                return uint32_t(left);
        }
        return s.frames * 2;
    }

    HRESULT fill(Stream &s, bool silent)
    {
        BYTE *data = nullptr;
        HRESULT hr = s.render->GetBuffer(s.frames, &data);
        if (hr == AUDCLNT_E_BUFFER_TOO_LARGE)
            return S_OK; // Not its turn yet.
        if (FAILED(hr))
            return hr;
        if (!silent) {
            bl_audio_render_pcm(data, s.frames, s.opened.format, s.opened.channels, queued(s));
            s.written += s.frames;
        }
        return s.render->ReleaseBuffer(s.frames, silent ? AUDCLNT_BUFFERFLAGS_SILENT : 0);
    }

    void play(Stream &s)
    {
        int quiet = 0;
        while (m_run) {
            if (WaitForSingleObject(s.ready, kWaitMs) != WAIT_OBJECT_0) {
                if (++quiet >= kMaxQuietWaits) {
                    qWarning("direct output: the device stopped asking for samples");
                    fail(QString::fromLatin1(kNotAvailable));
                    break;
                }
                continue;
            }
            quiet = 0;
            const HRESULT hr = fill(s, false);
            if (FAILED(hr)) {
                qWarning("direct output: 0x%08lx", static_cast<unsigned long>(hr));
                fail(hr == AUDCLNT_E_DEVICE_IN_USE ? QString::fromLatin1(kInUse) : QString::fromLatin1(kNotAvailable));
                break;
            }
        }
        // What was queued plays out: two more turns of the device, on silence.
        if (m_playOut && !m_failed) {
            for (int i = 0; i < 2 && WaitForSingleObject(s.ready, kWaitMs) == WAIT_OBJECT_0; ++i) {
                if (FAILED(fill(s, true)))
                    break;
            }
        }
        s.client->Stop();
    }

    std::mutex m_mutex;
    std::thread m_thread;
    std::atomic<bool> m_run{false};
    std::atomic<bool> m_failed{false};
    std::atomic<bool> m_playOut{false};
    bool m_idle = false;
    std::string m_device;
    uint32_t m_rate = 0;
    std::vector<uint32_t> m_formats;
    BlDirectOpened m_opened{};
};

Direct &direct()
{
    static Direct d;
    return d;
}

size_t listCallback(void *, char *json, size_t len)
{
    const QByteArray list = listJson();
    if (size_t(list.size()) > len)
        return 0;
    std::memcpy(json, list.constData(), size_t(list.size()));
    return size_t(list.size());
}

int32_t openCallback(void *, const char *device, uint32_t rate, const uint32_t *formats, size_t count, BlDirectOpened *opened, char *err, size_t errLen)
{
    QString message;
    if (direct().open(device, rate, std::vector<uint32_t>(formats, formats + count), opened, &message))
        return 1;
    const QByteArray text = message.toUtf8();
    if (errLen > 0) {
        const size_t n = std::min(size_t(text.size()), errLen - 1);
        std::memcpy(err, text.constData(), n);
        err[n] = 0;
    }
    return 0;
}

void closeCallback(void *) { direct().close(); }
void idleCallback(void *, int32_t idle) { direct().setIdle(idle != 0); }

} // namespace

namespace directaudio {

void configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    cb.direct_list = listCallback;
    cb.direct_open = openCallback;
    cb.direct_close = closeCallback;
    cb.direct_idle = idleCallback;
}

void shutdown()
{
    direct().close();
}

} // namespace directaudio
