// Microphones on Windows: the active WASAPI capture endpoints, each opened
// input recorded in shared mode on its own thread (event driven, 10 ms
// periods) with the device's timestamps.
#include "mic_input.h"
#include "bridge.h"

#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QThreadPool>
#include <QVariantList>

#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <propidl.h>
#include <wrl.h>

#include <atomic>
#include <cmath>
#include <cstring>
#include <map>
#include <memory>
#include <thread>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

QPointer<Bridge> g_bridge;

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

// PKEY_Device_FriendlyName (functiondiscoverykeys_devpkey.h).
const PROPERTYKEY kFriendlyName = { { 0xa45c254e, 0xdf1c, 0x4efd, { 0x80, 0x20, 0x67, 0xd1, 0x46, 0xa8, 0x50, 0xe0 } }, 14 };

QString takeString(LPWSTR s)
{
    const QString r = s ? QString::fromWCharArray(s) : QString();
    CoTaskMemFree(s);
    return r;
}

// The sample format of a mix format.
struct Format {
    bool isFloat = false;
    int bits = 0;
    int channels = 0;
    uint32_t rate = 0;
};

Format formatOf(const WAVEFORMATEX *wfx)
{
    Format f;
    f.isFloat = wfx->wFormatTag == WAVE_FORMAT_IEEE_FLOAT;
    if (wfx->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
        // The subformat GUIDs carry the format tag in their first field.
        f.isFloat = reinterpret_cast<const WAVEFORMATEXTENSIBLE *>(wfx)->SubFormat.Data1 == WAVE_FORMAT_IEEE_FLOAT;
    }
    f.bits = wfx->wBitsPerSample;
    f.channels = wfx->nChannels;
    f.rate = wfx->nSamplesPerSec;
    return f;
}

// ---- listing (on a worker thread) ---------------------------------------------------------

void list()
{
    QThreadPool::globalInstance()->start([]() {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        QVariantList devices;
        {
            ComPtr<IMMDeviceEnumerator> en;
            ComPtr<IMMDeviceCollection> all;
            if (SUCCEEDED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&en)))
                && SUCCEEDED(en->EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE, &all))) {
                QString defaultId;
                ComPtr<IMMDevice> def;
                if (SUCCEEDED(en->GetDefaultAudioEndpoint(eCapture, eConsole, &def))) {
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
                        PROPVARIANT v;
                        PropVariantInit(&v);
                        if (SUCCEEDED(props->GetValue(kFriendlyName, &v)) && v.vt == VT_LPWSTR && v.pwszVal)
                            name = QString::fromWCharArray(v.pwszVal);
                        PropVariantClear(&v);
                    }
                    Format f;
                    ComPtr<IAudioClient> client;
                    WAVEFORMATEX *wfx = nullptr;
                    if (SUCCEEDED(dev->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(client.GetAddressOf())))
                        && SUCCEEDED(client->GetMixFormat(&wfx))) {
                        f = formatOf(wfx);
                        CoTaskMemFree(wfx);
                    }
                    devices.append(QVariantMap {
                        { QStringLiteral("id"), id },
                        { QStringLiteral("name"), name },
                        { QStringLiteral("channels"), f.channels > 0 ? f.channels : 1 },
                        { QStringLiteral("rate"), f.rate },
                        { QStringLiteral("default"), id == defaultId },
                    });
                }
            }
        }
        CoUninitialize();
        post(QStringLiteral("mic.devices"), { { QStringLiteral("devices"), devices } });
    });
}

// ---- recording --------------------------------------------------------------------------------

// The performance counter in 100 ns units (as WASAPI's QPC positions).
uint64_t qpc100ns()
{
    LARGE_INTEGER c, f;
    QueryPerformanceCounter(&c);
    QueryPerformanceFrequency(&f);
    return uint64_t(double(c.QuadPart) * 1e7 / double(f.QuadPart));
}

// One opened input on its own thread.
class Recorder
{
public:
    Recorder(uint32_t stream, const QString &device) : m_stream(stream), m_device(device.toStdWString())
    {
        m_thread = std::thread([this]() { run(); });
    }

    ~Recorder()
    {
        m_run = false;
        if (m_thread.joinable())
            m_thread.join();
    }

private:
    void run()
    {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        QString error;
        if (!record(error) && m_run.load())
            postState(m_stream, false, error);
        CoUninitialize();
    }

    bool record(QString &error)
    {
        ComPtr<IMMDeviceEnumerator> en;
        ComPtr<IMMDevice> dev;
        ComPtr<IAudioClient> client;
        ComPtr<IAudioCaptureClient> capture;
        if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&en)))
            || FAILED(en->GetDevice(m_device.c_str(), &dev))
            || FAILED(dev->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void **>(client.GetAddressOf())))) {
            error = QStringLiteral("the microphone is not available");
            return false;
        }
        WAVEFORMATEX *wfx = nullptr;
        if (FAILED(client->GetMixFormat(&wfx))) {
            error = QStringLiteral("unknown microphone format");
            return false;
        }
        const Format f = formatOf(wfx);
        // 10 ms periods in a 20 ms buffer: singing is scored against the
        // song closely.
        HRESULT hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 200000, 0, wfx, nullptr);
        CoTaskMemFree(wfx);
        if (!(f.isFloat && f.bits == 32) && f.bits != 16 && f.bits != 24 && f.bits != 32) {
            error = QStringLiteral("unsupported microphone format");
            return false;
        }
        if (f.channels < 1 || f.channels > 64) {
            error = QStringLiteral("unsupported microphone format");
            return false;
        }
        HANDLE ready = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (FAILED(hr) || !ready || FAILED(client->SetEventHandle(ready)) || FAILED(client->GetService(IID_PPV_ARGS(&capture)))
            || FAILED(client->Start())) {
            if (ready)
                CloseHandle(ready);
            error = hr == E_ACCESSDENIED ? QStringLiteral("microphone access is turned off in the Windows privacy settings")
                                         : QStringLiteral("cannot record the microphone");
            return false;
        }
        postState(m_stream, true);
        const int ch = f.channels;
        const int bytesPerSample = f.bits / 8;
        std::vector<int16_t> pcm;
        bool ok = true;
        while (m_run.load()) {
            WaitForSingleObject(ready, 100);
            UINT32 packet = 0;
            while (m_run.load() && (hr = capture->GetNextPacketSize(&packet)) == S_OK && packet > 0) {
                BYTE *data = nullptr;
                UINT32 frames = 0;
                DWORD flags = 0;
                UINT64 qpcPosition = 0;
                hr = capture->GetBuffer(&data, &frames, &flags, nullptr, &qpcPosition);
                if (FAILED(hr))
                    break;
                pcm.assign(size_t(frames) * ch, 0);
                if (!(flags & AUDCLNT_BUFFERFLAGS_SILENT) && data) {
                    for (size_t i = 0; i < pcm.size(); ++i) {
                        const BYTE *p = data + i * bytesPerSample;
                        int32_t v;
                        if (f.isFloat) {
                            float x;
                            memcpy(&x, p, 4);
                            v = int32_t(std::lround(std::fmax(-1.0f, std::fmin(1.0f, x)) * 32767.0f));
                        } else if (f.bits == 16) {
                            v = int16_t(p[0] | (p[1] << 8));
                        } else {
                            // 24 or 32 bit: the top 16 bits.
                            v = int16_t(p[bytesPerSample - 2] | (p[bytesPerSample - 1] << 8));
                        }
                        pcm[i] = int16_t(v);
                    }
                }
                capture->ReleaseBuffer(frames);
                if (frames == 0)
                    continue;
                // When the first frame was recorded (the device's clock is
                // the performance counter); unknown after timing errors.
                int64_t t = 0;
                if (!(flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR) && qpcPosition > 0) {
                    const uint64_t now = qpc100ns();
                    if (now >= qpcPosition && now - qpcPosition < 20000000)
                        t = bl_clock_us() - int64_t((now - qpcPosition) / 10);
                }
                bl_mic_push(m_stream, pcm.data(), frames, uint32_t(ch), f.rate, t);
            }
            if (FAILED(hr)) {
                ok = false;
                error = hr == AUDCLNT_E_DEVICE_INVALIDATED ? QStringLiteral("the microphone was disconnected")
                                                          : QStringLiteral("recording stopped");
                break;
            }
        }
        client->Stop();
        CloseHandle(ready);
        return ok;
    }

    const uint32_t m_stream;
    const std::wstring m_device;
    std::atomic<bool> m_run { true };
    std::thread m_thread;
};

// The opened inputs (Qt thread).
std::map<uint32_t, std::unique_ptr<Recorder>> g_recorders;

int32_t micRequest(void *, const char *kind, const char *json)
{
    const QByteArray k(kind);
    const QJsonObject o = QJsonDocument::fromJson(QByteArray(json)).object();
    const uint32_t stream = uint32_t(o.value(QStringLiteral("stream")).toDouble());
    if (k == "list") {
        list();
    } else if (k == "open") {
        g_recorders.erase(stream);
        g_recorders[stream] = std::make_unique<Recorder>(stream, o.value(QStringLiteral("device")).toString());
    } else if (k == "close") {
        g_recorders.erase(stream);
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
    // Recording ends with the app.
    QObject::connect(bridge, &QObject::destroyed, []() { g_recorders.clear(); });
    cb.mic_request = micRequest;
}

} // namespace micinput
