// Microphones on Linux: the PulseAudio / PipeWire sources (monitors of
// outputs left out), listed with libpulse and recorded with libpulse-simple
// in short fragments. Both are loaded at run time: the app starts without
// them.
#include "mic_input.h"
#include "bridge.h"

#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QThreadPool>
#include <QVariantList>

#include <atomic>
#include <chrono>
#include <dlfcn.h>
#include <map>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

namespace {

QPointer<Bridge> g_bridge;

// Runs a core command on the Qt thread (never inside the current one).
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

// ---- libpulse (the declarations it needs, as in its headers) -------------------

struct SampleSpec { int format; uint32_t rate; uint8_t channels; };
struct BufferAttr { uint32_t maxlength, tlength, prebuf, minreq, fragsize; };
struct ChannelMap { uint8_t channels; int map[32]; };
struct CVolume { uint8_t channels; uint32_t values[32]; };
// pa_source_info up to the fields read here.
struct SourceInfo {
    const char *name;
    uint32_t index;
    const char *description;
    SampleSpec sample_spec;
    ChannelMap channel_map;
    uint32_t owner_module;
    CVolume volume;
    int mute;
    uint32_t monitor_of_sink;
};
// pa_server_info up to the fields read here.
struct ServerInfo {
    const char *user_name;
    const char *host_name;
    const char *server_version;
    const char *server_name;
    SampleSpec sample_spec;
    const char *default_sink_name;
    const char *default_source_name;
};

constexpr int kSampleS16LE = 3;
constexpr int kStreamRecord = 2;
constexpr int kContextNoAutospawn = 1;
constexpr int kContextReady = 4, kContextFailed = 5, kContextTerminated = 6;
constexpr int kOperationRunning = 0;
constexpr uint32_t kInvalidIndex = uint32_t(-1);

using SourceCb = void (*)(void *, const SourceInfo *, int, void *);
using ServerCb = void (*)(void *, const ServerInfo *, void *);

struct Pulse {
    // libpulse
    void *(*mainloop_new)();
    void *(*mainloop_get_api)(void *);
    int (*mainloop_iterate)(void *, int, int *);
    void (*mainloop_free)(void *);
    void *(*context_new)(void *, const char *);
    int (*context_connect)(void *, const char *, int, const void *);
    int (*context_get_state)(const void *);
    void (*context_disconnect)(void *);
    void (*context_unref)(void *);
    void *(*context_get_server_info)(void *, ServerCb, void *);
    void *(*context_get_source_info_list)(void *, SourceCb, void *);
    int (*operation_get_state)(const void *);
    void (*operation_unref)(void *);
    const char *(*strerror)(int);
    // libpulse-simple
    void *(*simple_new)(const char *, const char *, int, const char *, const char *, const SampleSpec *,
                        const void *, const BufferAttr *, int *);
    int (*simple_read)(void *, void *, size_t, int *);
    uint64_t (*simple_get_latency)(void *, int *);
    void (*simple_free)(void *);
};

const Pulse *pulse()
{
    static const Pulse *p = []() -> const Pulse * {
        void *lib = dlopen("libpulse.so.0", RTLD_NOW | RTLD_LOCAL);
        void *simple = dlopen("libpulse-simple.so.0", RTLD_NOW | RTLD_LOCAL);
        if (!lib || !simple)
            return nullptr;
        static Pulse f;
        bool ok = true;
        auto load = [&](auto &fn, void *from, const char *name) {
            fn = reinterpret_cast<std::remove_reference_t<decltype(fn)>>(dlsym(from, name));
            ok = ok && fn;
        };
        load(f.mainloop_new, lib, "pa_mainloop_new");
        load(f.mainloop_get_api, lib, "pa_mainloop_get_api");
        load(f.mainloop_iterate, lib, "pa_mainloop_iterate");
        load(f.mainloop_free, lib, "pa_mainloop_free");
        load(f.context_new, lib, "pa_context_new");
        load(f.context_connect, lib, "pa_context_connect");
        load(f.context_get_state, lib, "pa_context_get_state");
        load(f.context_disconnect, lib, "pa_context_disconnect");
        load(f.context_unref, lib, "pa_context_unref");
        load(f.context_get_server_info, lib, "pa_context_get_server_info");
        load(f.context_get_source_info_list, lib, "pa_context_get_source_info_list");
        load(f.operation_get_state, lib, "pa_operation_get_state");
        load(f.operation_unref, lib, "pa_operation_unref");
        load(f.strerror, lib, "pa_strerror");
        load(f.simple_new, simple, "pa_simple_new");
        load(f.simple_read, simple, "pa_simple_read");
        load(f.simple_get_latency, simple, "pa_simple_get_latency");
        load(f.simple_free, simple, "pa_simple_free");
        return ok ? &f : nullptr;
    }();
    return p;
}

// ---- listing ------------------------------------------------------------------------

struct Source {
    QString id, name;
    uint32_t channels = 1, rate = 48000;
};

// The inputs' formats (from the last listing), for recording them as they are.
std::mutex g_formatsLock;
std::map<QString, Source> g_formats;

struct Listing {
    QString defaultSource;
    QList<Source> sources;
};

// Lists the sources (on a worker thread; waits at most two seconds).
bool listSources(Listing &out, QString &error)
{
    const Pulse *pa = pulse();
    if (!pa) {
        error = QStringLiteral("PulseAudio or PipeWire is needed");
        return false;
    }
    void *loop = pa->mainloop_new();
    if (!loop)
        return false;
    void *ctx = pa->context_new(pa->mainloop_get_api(loop), "Beatlumen");
    bool ok = false;
    if (ctx && pa->context_connect(ctx, nullptr, kContextNoAutospawn, nullptr) >= 0) {
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
        auto iterate = [&]() {
            pa->mainloop_iterate(loop, 0, nullptr);
            std::this_thread::sleep_for(std::chrono::milliseconds(2));
            return std::chrono::steady_clock::now() < deadline;
        };
        int state = 0;
        while ((state = pa->context_get_state(ctx)) != kContextReady && state != kContextFailed
               && state != kContextTerminated && iterate()) {
        }
        if (state == kContextReady) {
            const ServerCb onServer = [](void *, const ServerInfo *i, void *user) {
                if (i && i->default_source_name)
                    static_cast<Listing *>(user)->defaultSource = QString::fromUtf8(i->default_source_name);
            };
            const SourceCb onSource = [](void *, const SourceInfo *i, int eol, void *user) {
                if (eol || !i || !i->name || i->monitor_of_sink != kInvalidIndex)
                    return;
                Source s;
                s.id = QString::fromUtf8(i->name);
                if (s.id.endsWith(QLatin1String(".monitor")))
                    return;
                s.name = i->description ? QString::fromUtf8(i->description) : s.id;
                s.channels = i->sample_spec.channels ? i->sample_spec.channels : 1;
                s.rate = i->sample_spec.rate ? i->sample_spec.rate : 48000;
                static_cast<Listing *>(user)->sources.append(s);
            };
            for (void *op : { pa->context_get_server_info(ctx, onServer, &out),
                              pa->context_get_source_info_list(ctx, onSource, &out) }) {
                if (!op)
                    continue;
                while (pa->operation_get_state(op) == kOperationRunning && iterate()) {
                }
                pa->operation_unref(op);
            }
            ok = true;
        } else {
            error = QStringLiteral("cannot connect to the sound server");
        }
        pa->context_disconnect(ctx);
    }
    if (ctx)
        pa->context_unref(ctx);
    pa->mainloop_free(loop);
    return ok;
}

void list()
{
    QThreadPool::globalInstance()->start([]() {
        Listing l;
        QString error;
        if (!listSources(l, error)) {
            qWarning("mic: %s", qPrintable(error));
            return;
        }
        QVariantList devices;
        std::lock_guard<std::mutex> g(g_formatsLock);
        for (const Source &s : l.sources) {
            g_formats[s.id] = s;
            devices.append(QVariantMap {
                { QStringLiteral("id"), s.id },
                { QStringLiteral("name"), s.name },
                { QStringLiteral("channels"), s.channels },
                { QStringLiteral("rate"), s.rate },
                { QStringLiteral("default"), s.id == l.defaultSource },
            });
        }
        post(QStringLiteral("mic.devices"), { { QStringLiteral("devices"), devices } });
    });
}

// ---- recording -------------------------------------------------------------------------

void postState(uint32_t stream, bool running, const QString &error = {})
{
    post(QStringLiteral("mic.state"), { { QStringLiteral("stream"), stream },
                                        { QStringLiteral("running"), running },
                                        { QStringLiteral("error"), error } });
}

// One opened input on its own thread.
class Recorder
{
public:
    Recorder(uint32_t stream, const QString &device) : m_stream(stream), m_device(device.toUtf8())
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
        const Pulse *pa = pulse();
        if (!pa) {
            postState(m_stream, false, QStringLiteral("PulseAudio or PipeWire is needed"));
            return;
        }
        Source fmt;
        {
            std::lock_guard<std::mutex> g(g_formatsLock);
            auto it = g_formats.find(QString::fromUtf8(m_device));
            if (it != g_formats.end())
                fmt = it->second;
        }
        // The input's own format (all channels: the core picks one or mixes).
        const uint32_t rate = fmt.rate >= 8000 && fmt.rate <= 192000 ? fmt.rate : 48000;
        const uint8_t channels = uint8_t(fmt.channels >= 1 && fmt.channels <= 32 ? fmt.channels : 1);
        const SampleSpec spec { kSampleS16LE, rate, channels };
        // 10 ms fragments: singing is scored against the song closely.
        const size_t frames = rate / 100;
        const uint32_t bytes = uint32_t(frames * channels * 2);
        const BufferAttr attr { uint32_t(-1), uint32_t(-1), uint32_t(-1), uint32_t(-1), bytes };
        int err = 0;
        void *s = pa->simple_new(nullptr, "Beatlumen", kStreamRecord, m_device.constData(), "Microphone", &spec, nullptr,
                                 &attr, &err);
        if (!s) {
            postState(m_stream, false, QStringLiteral("cannot record: ") + QString::fromUtf8(pa->strerror(err)));
            return;
        }
        postState(m_stream, true);
        std::vector<int16_t> buf(frames * channels);
        QString error;
        while (m_run.load()) {
            if (pa->simple_read(s, buf.data(), bytes, &err) < 0) {
                error = QStringLiteral("recording stopped: ") + QString::fromUtf8(pa->strerror(err));
                break;
            }
            // What is still buffered was recorded after these frames.
            int lerr = 0;
            uint64_t latency = pa->simple_get_latency(s, &lerr);
            if (latency == uint64_t(-1) || latency > 2000000)
                latency = 0;
            const int64_t t = bl_clock_us() - int64_t(latency) - int64_t(frames * 1000000 / rate);
            bl_mic_push(m_stream, buf.data(), frames, channels, rate, t);
        }
        pa->simple_free(s);
        if (m_run.load())
            postState(m_stream, false, error);
    }

    const uint32_t m_stream;
    const QByteArray m_device;
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
        if (!pulse())
            return 0;
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
    if (!pulse())
        return;
    g_bridge = bridge;
    // Recording ends with the app.
    QObject::connect(bridge, &QObject::destroyed, []() { g_recorders.clear(); });
    cb.mic_request = micRequest;
}

} // namespace micinput
