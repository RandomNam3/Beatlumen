#include "audio.h"

#if defined(__ANDROID__)

// Android: AAudio directly. The stream is opened for the default device
// (no fixed device id), marked as music playback, so Android routes it to
// whatever output is active (speaker, wired or Bluetooth headphones) and
// moves it when that changes. A watchdog reopens it when Android drops it
// (e.g. while Bluetooth renegotiates) and measures the real output latency
// (Bluetooth adds 100-300 ms) for in-sync visuals.

#include <aaudio/AAudio.h>
#include <dlfcn.h>
#include <time.h>

#include <QtGlobal>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <thread>

namespace {

// AAudioStreamBuilder_setUsage/setContentType exist from API 28.
using SetI32 = void (*)(AAudioStreamBuilder *, int32_t);

int64_t monotonicNs()
{
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return int64_t(ts.tv_sec) * 1000000000 + ts.tv_nsec;
}

} // namespace

struct AudioOutput::Impl
{
    std::mutex mutex;
    std::condition_variable cv;
    std::thread watchdog;
    AAudioStream *stream = nullptr;
    std::atomic<bool> broken{false};
    std::atomic<uint32_t> latency{0};
    bool quit = false;
    uint32_t rate = 48000;

    static aaudio_data_callback_result_t onData(AAudioStream *, void *user, void *audio, int32_t frames)
    {
        auto *self = static_cast<Impl *>(user);
        bl_audio_render(static_cast<float *>(audio), size_t(frames), self->latency.load(std::memory_order_relaxed));
        return AAUDIO_CALLBACK_RESULT_CONTINUE;
    }

    // Called on an AAudio thread: the stream must not be closed here.
    static void onError(AAudioStream *, void *user, aaudio_result_t)
    {
        auto *self = static_cast<Impl *>(user);
        self->broken = true;
        self->cv.notify_all();
    }

    void close()
    {
        if (stream) {
            AAudioStream_requestStop(stream);
            AAudioStream_close(stream);
            stream = nullptr;
        }
    }

    bool open()
    {
        close();
        AAudioStreamBuilder *b = nullptr;
        if (AAudio_createStreamBuilder(&b) != AAUDIO_OK)
            return false;
        AAudioStreamBuilder_setDirection(b, AAUDIO_DIRECTION_OUTPUT);
        AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_FLOAT);
        AAudioStreamBuilder_setChannelCount(b, 2);
        AAudioStreamBuilder_setSampleRate(b, int32_t(rate));
        AAudioStreamBuilder_setSharingMode(b, AAUDIO_SHARING_MODE_SHARED);
        // Power saving over low latency: the visuals compensate the latency.
        AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_NONE);
        static const auto setUsage = reinterpret_cast<SetI32>(dlsym(RTLD_DEFAULT, "AAudioStreamBuilder_setUsage"));
        static const auto setContentType = reinterpret_cast<SetI32>(dlsym(RTLD_DEFAULT, "AAudioStreamBuilder_setContentType"));
        if (setUsage)
            setUsage(b, 1); // AAUDIO_USAGE_MEDIA
        if (setContentType)
            setContentType(b, 2); // AAUDIO_CONTENT_TYPE_MUSIC
        AAudioStreamBuilder_setDataCallback(b, &Impl::onData, this);
        AAudioStreamBuilder_setErrorCallback(b, &Impl::onError, this);
        const aaudio_result_t r = AAudioStreamBuilder_openStream(b, &stream);
        AAudioStreamBuilder_delete(b);
        if (r != AAUDIO_OK) {
            qWarning("audio output: AAudio open failed: %s", AAudio_convertResultToText(r));
            stream = nullptr;
            return false;
        }
        broken = false;
        if (AAudioStream_requestStart(stream) != AAUDIO_OK) {
            close();
            return false;
        }
        qInfo("audio output: AAudio device %d, %d Hz, burst %d", AAudioStream_getDeviceId(stream),
              AAudioStream_getSampleRate(stream), AAudioStream_getFramesPerBurst(stream));
        return true;
    }

    // Frames written but not yet heard (buffer + device + e.g. Bluetooth).
    void measureLatency()
    {
        if (!stream)
            return;
        int64_t pos = 0, timeNs = 0;
        if (AAudioStream_getTimestamp(stream, CLOCK_MONOTONIC, &pos, &timeNs) != AAUDIO_OK)
            return;
        const int64_t written = AAudioStream_getFramesWritten(stream);
        const int64_t heardNow = pos + (monotonicNs() - timeNs) * int64_t(rate) / 1000000000;
        const int64_t lat = written - heardNow;
        if (lat >= 0 && lat < int64_t(rate) * 2)
            latency = uint32_t(lat);
    }

    void run()
    {
        int failures = 0;
        std::unique_lock<std::mutex> lock(mutex);
        while (!quit) {
            cv.wait_for(lock, std::chrono::milliseconds(failures ? 250 * failures : 500));
            if (quit)
                break;
            const bool dead = !stream || broken
                || AAudioStream_getState(stream) == AAUDIO_STREAM_STATE_DISCONNECTED;
            if (dead) {
                // Give the routing a moment to settle, then open a new stream.
                lock.unlock();
                std::this_thread::sleep_for(std::chrono::milliseconds(200));
                lock.lock();
                if (quit)
                    break;
                failures = open() ? 0 : std::min(failures + 1, 8);
            } else {
                measureLatency();
            }
        }
        close();
    }
};

AudioOutput *AudioOutput::instance()
{
    static AudioOutput out;
    return &out;
}

void AudioOutput::configure(BlCallbacks &) {}

uint32_t AudioOutput::start()
{
    if (!d) {
        d = new Impl;
        {
            std::lock_guard<std::mutex> lock(d->mutex);
            d->open();
        }
        d->watchdog = std::thread([this]() { d->run(); });
    }
    return d->rate;
}

void AudioOutput::stop()
{
    if (!d)
        return;
    {
        std::lock_guard<std::mutex> lock(d->mutex);
        d->quit = true;
    }
    d->cv.notify_all();
    if (d->watchdog.joinable())
        d->watchdog.join();
}

#elif defined(BL_HAVE_MULTIMEDIA)

#include <QAudioDevice>
#include <QAudioFormat>
#include <QAudioSink>
#include <QIODevice>
#include <QMediaDevices>
#include <QMutex>
#include <QSemaphore>
#include <QThread>
#include <QTimer>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <thread>
#include <cstdio>
#include <vector>

namespace {

// Test support: BEATLUMEN_TEST_AUDIO=<file> appends every rendered block
// (interleaved stereo float) so the output can be checked for glitches.
void dumpAudio(const float *data, size_t frames)
{
    static FILE *f = []() -> FILE * {
        const QByteArray p = qgetenv("BEATLUMEN_TEST_AUDIO");
        return p.isEmpty() ? nullptr : fopen(p.constData(), "wb");
    }();
    if (f) {
        fwrite(data, sizeof(float), frames * 2, f);
        fflush(f);
    }
}

// Without an audio device (headless systems) playback still advances: a
// thread consumes audio in real time so the clock and visuals keep going.
class NullClock
{
public:
    ~NullClock() { stop(); }
    void start(uint32_t rate)
    {
        stop();
        m_run = true;
        m_thread = std::thread([this, rate]() {
            const size_t frames = rate / 100;
            std::vector<float> buf(frames * 2);
            auto next = std::chrono::steady_clock::now();
            while (m_run) {
                bl_audio_render(buf.data(), frames, uint32_t(frames));
                dumpAudio(buf.data(), frames);
                next += std::chrono::milliseconds(10);
                std::this_thread::sleep_until(next);
            }
        });
    }
    void stop()
    {
        m_run = false;
        if (m_thread.joinable())
            m_thread.join();
    }

private:
    std::atomic<bool> m_run{false};
    std::thread m_thread;
};

// Pull device: every read renders audio from the core.
class PullDevice : public QIODevice
{
public:
    QAudioSink *sink = nullptr;
    // Time of the last read by the audio system (steady clock, ms).
    std::atomic<qint64> lastRead{0};

    bool isSequential() const override { return true; }
    qint64 bytesAvailable() const override { return 1 << 20; }

protected:
    qint64 readData(char *data, qint64 maxlen) override
    {
        const qint64 frames = maxlen / 8;
        if (frames <= 0)
            return 0;
        lastRead = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now().time_since_epoch()).count();
        uint32_t latency = 0;
        if (sink) {
            const qint64 queued = sink->bufferSize() - sink->bytesFree();
            latency = queued > 0 ? uint32_t(queued / 8) : 0;
        }
        bl_audio_render(reinterpret_cast<float *>(data), size_t(frames), latency);
        dumpAudio(reinterpret_cast<const float *>(data), size_t(frames));
        return frames * 8;
    }
    qint64 writeData(const char *, qint64) override { return -1; }
};

} // namespace

struct AudioOutput::Impl
{
    QThread thread;
    QObject *worker = nullptr;
    QAudioSink *sink = nullptr;
    PullDevice *device = nullptr;
    QMediaDevices *devices = nullptr;
    QTimer *reopenTimer = nullptr;
    QTimer *watchdog = nullptr;
    NullClock nullClock;
    // Chosen once: the core resamples to this rate, so it must not change
    // when the output device does (the system mixer converts if needed).
    uint32_t rate = 0;
    int failures = 0;

    // Runs on the audio thread.
    void open()
    {
        nullClock.stop();
        if (sink) {
            sink->stop();
            delete sink;
            sink = nullptr;
        }
        if (device)
            device->sink = nullptr;
        const QAudioDevice dev = QMediaDevices::defaultAudioOutput();
        if (dev.isNull()) {
            rate = rate ? rate : 48000;
            nullClock.start(rate);
            return;
        }
        QAudioFormat fmt;
        fmt.setChannelCount(2);
        fmt.setSampleFormat(QAudioFormat::Float);
        if (!rate)
            rate = 48000;
        fmt.setSampleRate(int(rate));
        if (!dev.isFormatSupported(fmt) && rate == 48000 && dev.preferredFormat().sampleRate() > 0) {
            // First open only: take the device's rate if 48 kHz is refused.
            fmt.setSampleRate(dev.preferredFormat().sampleRate());
            rate = uint32_t(fmt.sampleRate());
        }
        sink = new QAudioSink(dev, fmt);
        // ~80 ms buffer: low enough for tight visual sync.
        sink->setBufferSize(qsizetype(rate * 8 * 0.08));
        if (!device) {
            device = new PullDevice;
            device->open(QIODevice::ReadOnly);
        }
        device->sink = sink;
        device->lastRead = nowMs();
        QObject::connect(sink, &QAudioSink::stateChanged, worker, [this](QAudio::State st) {
            // The stream died (device unplugged or rerouted, e.g. while
            // Bluetooth headphones connect): open a new one shortly.
            if (sink && st == QAudio::StoppedState && sink->error() != QAudio::NoError)
                scheduleReopen();
        });
        sink->start(device);
        qInfo("audio output: %s, %u Hz%s", qPrintable(dev.description()), rate, sink->error() != QAudio::NoError ? " (failed)" : "");
        if (sink->error() != QAudio::NoError) {
            delete sink;
            sink = nullptr;
            device->sink = nullptr;
            nullClock.start(rate);
            scheduleReopen();
        }
    }

    // Reopens after the device situation has settled (routing changes come
    // in bursts); backs off while it keeps failing.
    void scheduleReopen()
    {
        failures = std::min(failures + 1, 6);
        reopenTimer->start(250 * failures);
    }

    static qint64 nowMs()
    {
        return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now().time_since_epoch()).count();
    }

    // Periodic health check: a stream that stopped with an error, that no
    // longer asks for data (some systems drop dead streams silently) or a
    // missing stream while a device exists is replaced.
    void check()
    {
        if (reopenTimer->isActive())
            return;
        const bool stalled = sink && device && nowMs() - device->lastRead > 1500;
        const bool broken = sink ? sink->error() != QAudio::NoError || sink->state() == QAudio::StoppedState || stalled
                                 : !QMediaDevices::defaultAudioOutput().isNull();
        if (broken)
            scheduleReopen();
        else
            failures = 0;
    }
};

AudioOutput *AudioOutput::instance()
{
    static AudioOutput out;
    return &out;
}

void AudioOutput::configure(BlCallbacks &) {}

uint32_t AudioOutput::start()
{
    if (!d) {
        d = new Impl;
        d->thread.setObjectName(QStringLiteral("bl-audio"));
        d->thread.start(QThread::TimeCriticalPriority);
        d->worker = new QObject;
        d->worker->moveToThread(&d->thread);
    }
    QMetaObject::invokeMethod(d->worker, [this]() {
        if (!d->reopenTimer) {
            d->reopenTimer = new QTimer(d->worker);
            d->reopenTimer->setSingleShot(true);
            QObject::connect(d->reopenTimer, &QTimer::timeout, d->worker, [this]() { d->open(); });
            d->watchdog = new QTimer(d->worker);
            QObject::connect(d->watchdog, &QTimer::timeout, d->worker, [this]() { d->check(); });
            d->watchdog->start(1000);
        }
        d->open();
        if (!d->devices) {
            d->devices = new QMediaDevices(d->worker);
            // Device lists change in bursts (e.g. Bluetooth connecting):
            // reopen once things have settled.
            QObject::connect(d->devices, &QMediaDevices::audioOutputsChanged, d->worker, [this]() {
                d->failures = 0;
                d->reopenTimer->start(400);
            });
        }
    }, Qt::BlockingQueuedConnection);
    return d->rate;
}

void AudioOutput::stop()
{
    if (!d)
        return;
    QMetaObject::invokeMethod(d->worker, [this]() {
        d->nullClock.stop();
        if (d->watchdog)
            d->watchdog->stop();
        if (d->reopenTimer)
            d->reopenTimer->stop();
        if (d->sink)
            d->sink->stop();
        delete d->sink;
        d->sink = nullptr;
        delete d->devices;
        d->devices = nullptr;
    }, Qt::BlockingQueuedConnection);
    d->thread.quit();
    d->thread.wait();
}

#elif defined(Q_OS_WASM) || defined(__EMSCRIPTEN__)

// Web Audio push output: an AudioWorklet plays queued sample blocks.
#include <emscripten.h>

EM_JS(void, bl_js_audio_init, (), {
    if (Module.blAudio) return;
    const ctx = new (window.AudioContext || window.webkitAudioContext)({ sampleRate: 48000 });
    const code = `class P extends AudioWorkletProcessor {
        constructor() { super(); this.q = []; this.off = 0; this.queued = 0; this.paused = false; this.gain = 0.25; this.target = 0.25;
            this.port.onmessage = (e) => { const m = e.data;
                if (m.t === 'push') { this.q.push(m.d); this.queued += m.d.length / 2; }
                else if (m.t === 'clear') { this.q = []; this.off = 0; this.queued = 0; }
                else if (m.t === 'pause') { this.paused = m.v; }
                else if (m.t === 'gain') { this.target = m.v; } }; }
        process(inputs, outputs) { const out = outputs[0]; const l = out[0], r = out[1] || out[0];
            for (let i = 0; i < l.length; i++) {
                if (this.paused || this.q.length === 0) { l[i] = 0; r[i] = 0; continue; }
                this.gain += (this.target - this.gain) * 0.0002;
                const b = this.q[0]; l[i] = b[this.off] * this.gain; r[i] = b[this.off + 1] * this.gain; this.off += 2; this.queued--;
                if (this.off >= b.length) { this.q.shift(); this.off = 0; } }
            this.port.postMessage(this.queued); return true; } }
        registerProcessor('bl-out', P);`;
    const url = URL.createObjectURL(new Blob([code], { type: 'application/javascript' }));
    Module.blAudio = { ctx: ctx, node: null, queued: 0, pending: [] };
    ctx.audioWorklet.addModule(url).then(() => {
        const node = new AudioWorkletNode(ctx, 'bl-out', { outputChannelCount: [2] });
        node.port.onmessage = (e) => { Module.blAudio.queued = e.data; };
        node.connect(ctx.destination);
        Module.blAudio.node = node;
        for (const m of Module.blAudio.pending) node.port.postMessage(m);
        Module.blAudio.pending = [];
    });
    const resume = () => { if (ctx.state !== 'running') ctx.resume(); };
    window.addEventListener('pointerdown', resume);
    window.addEventListener('keydown', resume);
});

EM_JS(void, bl_js_audio_post, (int kind, const float *ptr, int count, int flag), {
    const a = Module.blAudio; if (!a) return;
    let m;
    if (kind === 0) { const d = new Float32Array(HEAPF32.buffer, ptr, count).slice(); m = { t: 'push', d: d }; a.queued += count / 2; }
    else if (kind === 1) { m = { t: 'clear' }; a.queued = 0; }
    else if (kind === 3) { m = { t: 'gain', v: HEAPF32[ptr >> 2] }; }
    else { m = { t: 'pause', v: !!flag }; }
    if (a.node) a.node.port.postMessage(m, kind === 0 ? [m.d.buffer] : []); else a.pending.push(m);
});

EM_JS(int, bl_js_audio_queued, (), {
    return Module.blAudio ? Module.blAudio.queued | 0 : 0;
});

namespace {
void pushCb(void *, const float *s, size_t n) { bl_js_audio_post(0, s, int(n), 0); }
uint32_t queuedCb(void *) { return uint32_t(bl_js_audio_queued()); }
void clearCb(void *) { bl_js_audio_post(1, nullptr, 0, 0); }
void pausedCb(void *, int32_t p) { bl_js_audio_post(2, nullptr, 0, p); }
void gainCb(void *, float g) { bl_js_audio_post(3, &g, 1, 0); }
} // namespace

AudioOutput *AudioOutput::instance()
{
    static AudioOutput out;
    return &out;
}

void AudioOutput::configure(BlCallbacks &cb)
{
    bl_js_audio_init();
    cb.audio_push = pushCb;
    cb.audio_queued = queuedCb;
    cb.audio_clear = clearCb;
    cb.audio_set_paused = pausedCb;
    cb.audio_set_gain = gainCb;
}

uint32_t AudioOutput::start() { return 48000; }
void AudioOutput::stop() {}

#else

AudioOutput *AudioOutput::instance()
{
    static AudioOutput out;
    return &out;
}
void AudioOutput::configure(BlCallbacks &) {}
uint32_t AudioOutput::start() { return 0; }
void AudioOutput::stop() {}

#endif
