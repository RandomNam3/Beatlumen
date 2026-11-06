// A sound card in direct use (Linux): ALSA's hw devices, nothing in between. No
// plug layer that converts, no fall back to the mixer: what the card refuses is
// reported, and the core decides. libasound is loaded at run time, the app
// starts without it.
#include "audio_direct.h"
#include "audio_reserve.h"
#include "bridge.h"

#include <QDir>
#include <QFile>
#include <QProcess>
#include <QStandardPaths>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPointer>
#include <QRegularExpression>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstring>
#include <dlfcn.h>
#include <mutex>
#include <pthread.h>
#include <string>
#include <thread>
#include <vector>

namespace {

QPointer<Bridge> g_bridge;

// Runs a core command on the Qt thread.
void post(const QString &cmd, const QVariantMap &args)
{
    QPointer<Bridge> b = g_bridge;
    if (b)
        QMetaObject::invokeMethod(b, [b, cmd, args]() { if (b) b->call(cmd, args); }, Qt::QueuedConnection);
}

// ---- libasound (the declarations it needs, as in its headers) --------------

using Pcm = void;
using HwParams = void;
using SwParams = void;

const int kStreamPlayback = 0;
const int kNonBlock = 1;
const int kAccessRwInterleaved = 3;
// Beatlumen's PcmFormat, as ALSA numbers them.
const int kAlsaFormats[] = {2 /* S16_LE */, 32 /* S24_3LE */, 6 /* S24_LE */, 10 /* S32_LE */};
const size_t kFormatBytes[] = {2, 3, 4, 4};

struct Alsa {
    int (*pcm_open)(Pcm **, const char *, int, int);
    int (*pcm_close)(Pcm *);
    long (*pcm_writei)(Pcm *, const void *, unsigned long);
    int (*pcm_wait)(Pcm *, int);
    int (*pcm_recover)(Pcm *, int, int);
    int (*pcm_delay)(Pcm *, long *);
    int (*pcm_drain)(Pcm *);
    int (*pcm_drop)(Pcm *);
    int (*hw_params_malloc)(HwParams **);
    void (*hw_params_free)(HwParams *);
    int (*hw_params_any)(Pcm *, HwParams *);
    int (*hw_params)(Pcm *, HwParams *);
    int (*set_rate_resample)(Pcm *, HwParams *, unsigned);
    int (*set_access)(Pcm *, HwParams *, int);
    int (*test_format)(Pcm *, HwParams *, int);
    int (*set_format)(Pcm *, HwParams *, int);
    int (*test_channels)(Pcm *, HwParams *, unsigned);
    int (*get_channels_min)(const HwParams *, unsigned *);
    int (*set_channels)(Pcm *, HwParams *, unsigned);
    int (*test_rate)(Pcm *, HwParams *, unsigned, int);
    int (*set_rate)(Pcm *, HwParams *, unsigned, int);
    int (*set_rate_near)(Pcm *, HwParams *, unsigned *, int *);
    int (*set_buffer_time_near)(Pcm *, HwParams *, unsigned *, int *);
    int (*set_period_time_near)(Pcm *, HwParams *, unsigned *, int *);
    int (*get_period_size)(const HwParams *, unsigned long *, int *);
    int (*sw_params_malloc)(SwParams **);
    void (*sw_params_free)(SwParams *);
    int (*sw_params_current)(Pcm *, SwParams *);
    int (*sw_set_start_threshold)(Pcm *, SwParams *, unsigned long);
    int (*sw_params)(Pcm *, SwParams *);
    const char *(*strerror)(int);
};

template <typename F>
bool load(void *lib, F &f, const char *name)
{
    f = reinterpret_cast<F>(dlsym(lib, name));
    return f != nullptr;
}

const Alsa *alsa()
{
    static Alsa a;
    static const bool ok = []() {
        void *l = dlopen("libasound.so.2", RTLD_NOW | RTLD_LOCAL);
        if (!l)
            return false;
        bool all = load(l, a.pcm_open, "snd_pcm_open");
        all &= load(l, a.pcm_close, "snd_pcm_close");
        all &= load(l, a.pcm_writei, "snd_pcm_writei");
        all &= load(l, a.pcm_wait, "snd_pcm_wait");
        all &= load(l, a.pcm_recover, "snd_pcm_recover");
        all &= load(l, a.pcm_delay, "snd_pcm_delay");
        all &= load(l, a.pcm_drain, "snd_pcm_drain");
        all &= load(l, a.pcm_drop, "snd_pcm_drop");
        all &= load(l, a.hw_params_malloc, "snd_pcm_hw_params_malloc");
        all &= load(l, a.hw_params_free, "snd_pcm_hw_params_free");
        all &= load(l, a.hw_params_any, "snd_pcm_hw_params_any");
        all &= load(l, a.hw_params, "snd_pcm_hw_params");
        all &= load(l, a.set_rate_resample, "snd_pcm_hw_params_set_rate_resample");
        all &= load(l, a.set_access, "snd_pcm_hw_params_set_access");
        all &= load(l, a.test_format, "snd_pcm_hw_params_test_format");
        all &= load(l, a.set_format, "snd_pcm_hw_params_set_format");
        all &= load(l, a.test_channels, "snd_pcm_hw_params_test_channels");
        all &= load(l, a.get_channels_min, "snd_pcm_hw_params_get_channels_min");
        all &= load(l, a.set_channels, "snd_pcm_hw_params_set_channels");
        all &= load(l, a.test_rate, "snd_pcm_hw_params_test_rate");
        all &= load(l, a.set_rate, "snd_pcm_hw_params_set_rate");
        all &= load(l, a.set_rate_near, "snd_pcm_hw_params_set_rate_near");
        all &= load(l, a.set_buffer_time_near, "snd_pcm_hw_params_set_buffer_time_near");
        all &= load(l, a.set_period_time_near, "snd_pcm_hw_params_set_period_time_near");
        all &= load(l, a.get_period_size, "snd_pcm_hw_params_get_period_size");
        all &= load(l, a.sw_params_malloc, "snd_pcm_sw_params_malloc");
        all &= load(l, a.sw_params_free, "snd_pcm_sw_params_free");
        all &= load(l, a.sw_params_current, "snd_pcm_sw_params_current");
        all &= load(l, a.sw_set_start_threshold, "snd_pcm_sw_params_set_start_threshold");
        all &= load(l, a.sw_params, "snd_pcm_sw_params");
        all &= load(l, a.strerror, "snd_strerror");
        return all;
    }();
    return ok ? &a : nullptr;
}

// ---- the cards -------------------------------------------------------------

// Test support: BEATLUMEN_TEST_ALSA_DEVICE=<pcm> offers that ALSA device too,
// and writes to it in real time (a "file" device takes all it gets at once).
const QByteArray &testDevice()
{
    static const QByteArray d = qgetenv("BEATLUMEN_TEST_ALSA_DEVICE");
    return d;
}

struct Card {
    int number;
    QString id;
    QString name;
};

QString readSmall(const QString &path)
{
    QFile f(path);
    return f.open(QIODevice::ReadOnly) ? QString::fromUtf8(f.readAll()) : QString();
}

std::vector<Card> cards()
{
    std::vector<Card> out;
    static const QRegularExpression line(QStringLiteral(R"(^\s*(\d+)\s+\[(\S+)\s*\]:\s+.* - (.*)$)"),
                                         QRegularExpression::MultilineOption);
    auto it = line.globalMatch(readSmall(QStringLiteral("/proc/asound/cards")));
    while (it.hasNext()) {
        const QRegularExpressionMatch m = it.next();
        out.push_back({m.captured(1).toInt(), m.captured(2), m.captured(3).trimmed()});
    }
    return out;
}

// The card number behind a stable name; -1 when it is not a card here.
int cardNumber(const QString &device)
{
    static const QRegularExpression name(QStringLiteral(R"(^hw:CARD=([^,]+))"));
    const QRegularExpressionMatch m = name.match(device);
    if (!m.hasMatch())
        return -1;
    for (const Card &c : cards()) {
        if (c.id == m.captured(1))
            return c.number;
    }
    return -1;
}

// ALSA's device for the PipeWire graph: other programs stay audible on it.
const char *const kPipeWire = "pipewire";

bool isPipeWire(const std::string &device)
{
    return device == kPipeWire;
}

bool pipeWireThere()
{
    QString dir = qEnvironmentVariable("PIPEWIRE_RUNTIME_DIR");
    if (dir.isEmpty())
        dir = qEnvironmentVariable("XDG_RUNTIME_DIR");
    const QString remote = qEnvironmentVariable("PIPEWIRE_REMOTE", QStringLiteral("pipewire-0"));
    return !dir.isEmpty() && QFile::exists(dir + QLatin1Char('/') + remote)
        && !QStandardPaths::findExecutable(QStringLiteral("pw-metadata")).isEmpty();
}

// Sets the rate of the PipeWire graph (0: as it pleases), for the whole graph:
// other programs are converted to it. A program that crashes leaves it set until
// the next start of PipeWire or of this one, which does no harm.
void forcePipeWireRate(uint32_t rate)
{
    QProcess p;
    p.start(QStringLiteral("pw-metadata"),
            {QStringLiteral("-n"), QStringLiteral("settings"), QStringLiteral("0"), QStringLiteral("clock.force-rate"), QString::number(rate)});
    if (!p.waitForFinished(2000))
        p.kill();
    p.waitForFinished(500);
}

// Looks at /proc and opens nothing: a device in use by the sound server stays
// where it is.
QByteArray listJson()
{
    QJsonArray list;
    if (pipeWireThere()) {
        QJsonObject o;
        o[QStringLiteral("id")] = QString::fromLatin1(kPipeWire);
        o[QStringLiteral("name")] = QStringLiteral("PipeWire");
        o[QStringLiteral("shared")] = true;
        list.append(o);
    }
    for (const Card &card : cards()) {
        const QString dir = QStringLiteral("/proc/asound/card%1").arg(card.number);
        QStringList pcms = QDir(dir).entryList({QStringLiteral("pcm*p")}, QDir::Dirs, QDir::Name);
        for (const QString &pcm : pcms) {
            static const QRegularExpression id(QStringLiteral(R"(^id: (.*)$)"), QRegularExpression::MultilineOption);
            const QRegularExpressionMatch m = id.match(readSmall(dir + QLatin1Char('/') + pcm + QStringLiteral("/info")));
            const QString number = pcm.mid(3, pcm.size() - 4);
            QJsonObject o;
            o[QStringLiteral("id")] = QStringLiteral("hw:CARD=%1,DEV=%2").arg(card.id, number);
            o[QStringLiteral("name")] = pcms.size() > 1 && m.hasMatch()
                ? QStringLiteral("%1 – %2").arg(card.name, m.captured(1).trimmed())
                : card.name;
            list.append(o);
        }
    }
    if (!testDevice().isEmpty()) {
        QJsonObject o;
        o[QStringLiteral("id")] = QString::fromUtf8(testDevice());
        o[QStringLiteral("name")] = QStringLiteral("Test");
        list.append(o);
    }
    return QJsonDocument(list).toJson(QJsonDocument::Compact);
}

// ---- the output --------------------------------------------------------------

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
        if (device != m_device)
            releaseRate();
        m_device = device;
        m_rate = rate;
        m_formats = std::move(formats);
        m_idle = false;
        if (!openPcm(err)) {
            m_device.clear();
            m_reservation.release();
            releaseRate();
            return false;
        }
        *opened = m_opened;
        startWorker(false);
        return true;
    }

    void close()
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        stopWorker(false);
        m_device.clear();
        m_idle = false;
        m_reservation.release();
        releaseRate();
    }

    void setIdle(bool idle)
    {
        std::lock_guard<std::mutex> lock(m_mutex);
        if (idle == m_idle || m_device.empty())
            return;
        m_idle = idle;
        stopWorker(false);
        if (idle) {
            m_reservation.release();
            releaseRate();
        } else {
            startWorker(true); // Asking for the card can take seconds.
        }
    }

private:
    void releaseRate()
    {
        if (m_rateForced) {
            m_rateForced = false;
            forcePipeWireRate(0);
        }
    }

    bool openPcm(QString *err)
    {
        const Alsa *a = alsa();
        if (!a) {
            *err = QStringLiteral("The sound card is not available");
            return false;
        }
        const QString device = QString::fromStdString(m_device);
        const int card = cardNumber(device);
        if (device.startsWith(QStringLiteral("hw:")) && card < 0) {
            *err = QStringLiteral("The sound card is not available");
            return false;
        }
        if (card >= 0 && !m_reservation.take(card, err))
            return false;
        if (isPipeWire(m_device)) {
            forcePipeWireRate(m_rate);
            m_rateForced = true;
        }
        // The server closes the card right after letting go of its name.
        Pcm *pcm = nullptr;
        int r = -EBUSY;
        for (int i = 0; i < 20 && r == -EBUSY; ++i) {
            if (i > 0)
                std::this_thread::sleep_for(std::chrono::milliseconds(100));
            r = a->pcm_open(&pcm, m_device.c_str(), kStreamPlayback, kNonBlock);
        }
        if (r < 0) {
            qWarning("direct output: %s: %s", m_device.c_str(), a->strerror(r));
            *err = r == -EBUSY ? QStringLiteral("The sound card is in use by another program")
                               : QStringLiteral("The sound card is not available");
            return false;
        }
        HwParams *hw = nullptr;
        a->hw_params_malloc(&hw);
        const bool ok = configure(a, pcm, hw);
        a->hw_params_free(hw);
        if (!ok) {
            a->pcm_close(pcm);
            *err = QStringLiteral("The sound card does not take this music");
            return false;
        }
        m_pcm = pcm;
        m_failed = false;
        qInfo("direct output: %s, %u Hz (asked for %u), format %u, %u channels", m_device.c_str(), m_opened.rate, m_rate,
              m_opened.format, m_opened.channels);
        return true;
    }

    bool configure(const Alsa *a, Pcm *pcm, HwParams *hw)
    {
        if (a->hw_params_any(pcm, hw) < 0 || a->set_access(pcm, hw, kAccessRwInterleaved) < 0)
            return false;
        a->set_rate_resample(pcm, hw, 0);
        int format = -1;
        for (uint32_t f : m_formats) {
            if (f < 4 && a->test_format(pcm, hw, kAlsaFormats[f]) == 0) {
                format = int(f);
                break;
            }
        }
        if (format < 0 || a->set_format(pcm, hw, kAlsaFormats[format]) < 0)
            return false;
        // Many interfaces only take all of their channels at once.
        unsigned channels = 2;
        if (a->test_channels(pcm, hw, 2) != 0 && (a->get_channels_min(hw, &channels) < 0 || channels < 2))
            return false;
        if (a->set_channels(pcm, hw, channels) < 0)
            return false;
        unsigned rate = m_rate;
        if (a->test_rate(pcm, hw, rate, 0) == 0) {
            if (a->set_rate(pcm, hw, rate, 0) < 0)
                return false;
        } else if (a->set_rate_near(pcm, hw, &rate, nullptr) < 0) {
            return false;
        }
        unsigned buffer = 200000, period = 50000;
        a->set_buffer_time_near(pcm, hw, &buffer, nullptr);
        a->set_period_time_near(pcm, hw, &period, nullptr);
        if (a->hw_params(pcm, hw) < 0)
            return false;
        unsigned long frames = 0;
        if (a->get_period_size(hw, &frames, nullptr) < 0 || frames == 0)
            return false;
        m_periodFrames = frames;
        SwParams *sw = nullptr;
        a->sw_params_malloc(&sw);
        if (a->sw_params_current(pcm, sw) == 0) {
            a->sw_set_start_threshold(pcm, sw, frames * 2);
            a->sw_params(pcm, sw);
        }
        a->sw_params_free(sw);
        m_opened.rate = rate;
        m_opened.format = uint32_t(format);
        m_opened.channels = channels;
        return true;
    }

    void startWorker(bool reopen)
    {
        m_run = true;
        m_thread = std::thread([this, reopen]() { worker(reopen); });
    }

    void stopWorker(bool playOut)
    {
        m_playOut = playOut;
        m_run = false;
        if (m_thread.joinable())
            m_thread.join();
        if (m_pcm) {
            const Alsa *a = alsa();
            if (playOut && !m_failed)
                a->pcm_drain(m_pcm);
            else
                a->pcm_drop(m_pcm);
            a->pcm_close(m_pcm);
            m_pcm = nullptr;
        }
    }

    void fail(const QString &error)
    {
        m_failed = true;
        post(QStringLiteral("audio.directFailed"), {{QStringLiteral("error"), error}});
    }

    void worker(bool reopen)
    {
        // Above the rest where the system allows it; fine without.
        sched_param param{};
        param.sched_priority = 10;
        pthread_setschedparam(pthread_self(), SCHED_FIFO, &param);
        if (reopen) {
            QString err;
            if (!openPcm(&err)) {
                fail(err);
                return;
            }
        }
        const Alsa *a = alsa();
        const size_t frameBytes = kFormatBytes[m_opened.format] * m_opened.channels;
        std::vector<uint8_t> buffer(m_periodFrames * frameBytes);
        const auto periodTime = std::chrono::microseconds(uint64_t(m_periodFrames) * 1000000 / std::max(m_opened.rate, 1u));
        auto next = std::chrono::steady_clock::now();
        const bool paced = !testDevice().isEmpty();
        while (m_run) {
            long delay = 0;
            if (a->pcm_delay(m_pcm, &delay) < 0 || delay < 0)
                delay = 0;
            bl_audio_render_pcm(buffer.data(), m_periodFrames, m_opened.format, m_opened.channels, uint32_t(delay));
            size_t done = 0;
            // What was taken from the queue still goes out when the card is to
            // play out; otherwise a song would lose its end at a change of rate.
            int patience = 30;
            while (done < m_periodFrames && (m_run || (m_playOut && patience > 0))) {
                long n = a->pcm_writei(m_pcm, buffer.data() + done * frameBytes, m_periodFrames - done);
                // Never blocked for good: a stream the sound server has not
                // started, or a card that hangs, must not keep this from stopping.
                if (n == -EAGAIN) {
                    a->pcm_wait(m_pcm, 100);
                    if (!m_run)
                        --patience;
                    continue;
                }
                if (n < 0)
                    n = a->pcm_recover(m_pcm, int(n), 1);
                if (n < 0) {
                    qWarning("direct output: %s", a->strerror(int(n)));
                    fail(QStringLiteral("The sound card is not available"));
                    return;
                }
                done += size_t(n);
            }
            if (paced) {
                next += periodTime;
                std::this_thread::sleep_until(next);
            }
        }
    }

    std::mutex m_mutex;
    std::thread m_thread;
    std::atomic<bool> m_run{false};
    std::atomic<bool> m_failed{false};
    std::atomic<bool> m_playOut{false};
    bool m_idle = false;
    bool m_rateForced = false;
    Pcm *m_pcm = nullptr;
    std::string m_device;
    uint32_t m_rate = 0;
    std::vector<uint32_t> m_formats;
    BlDirectOpened m_opened{};
    size_t m_periodFrames = 0;
    CardReservation m_reservation;
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
    CardReservation::onAskedToLetGo([]() {
        post(QStringLiteral("audio.directFailed"), {{QStringLiteral("error"), QStringLiteral("Another program asked for the sound card")}});
        direct().close();
    });
    cb.direct_list = listCallback;
    cb.direct_open = openCallback;
    cb.direct_close = closeCallback;
    cb.direct_idle = idleCallback;
}

void shutdown()
{
    direct().close();
    CardReservation::shutdown();
}

} // namespace directaudio
