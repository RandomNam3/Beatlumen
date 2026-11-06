#include "audio_reserve.h"

#ifdef BEATLUMEN_RESERVE

#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusReply>
#include <QMutex>
#include <QObject>
#include <QThread>

#include <chrono>
#include <thread>

namespace {

// Above the servers, which hold cards at 0 (PulseAudio) or below (WirePlumber).
const int kPriority = 5;
const int kPatienceMs = 3000;

std::function<void()> g_askedToLetGo;

// The object answers on its own thread, because the server may look at it
// while the one asking for the card waits for that server's answer.
QThread *busThread()
{
    static QThread *thread = []() {
        auto *t = new QThread;
        t->setObjectName(QStringLiteral("bl-reserve"));
        t->start();
        return t;
    }();
    return thread;
}

} // namespace

class ReservationObject : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.ReserveDevice1")
    Q_PROPERTY(int Priority READ priority)
    Q_PROPERTY(QString ApplicationName READ applicationName)
    Q_PROPERTY(QString ApplicationDeviceName READ applicationDeviceName)

public:
    explicit ReservationObject(int card) : m_card(card) {}
    int priority() const { return kPriority; }
    QString applicationName() const { return QStringLiteral("Beatlumen"); }
    QString applicationDeviceName() const { return QStringLiteral("ALSA card %1").arg(m_card); }

public slots:
    bool RequestRelease(int priority)
    {
        if (priority <= kPriority)
            return false;
        if (g_askedToLetGo) {
            // Closing the card waits for the output: not on the bus thread.
            std::thread(g_askedToLetGo).detach();
        }
        return true;
    }

private:
    int m_card;
};

static QDBusMessage busCall(const QString &method, const QString &name, uint flags, bool withFlags)
{
    QDBusMessage call = QDBusMessage::createMethodCall(QStringLiteral("org.freedesktop.DBus"), QStringLiteral("/org/freedesktop/DBus"),
                                                       QStringLiteral("org.freedesktop.DBus"), method);
    call << name;
    if (withFlags)
        call << flags;
    return QDBusConnection::sessionBus().call(call, QDBus::Block, kPatienceMs);
}

CardReservation::~CardReservation()
{
    release();
}

bool CardReservation::take(int number, QString *err)
{
    if (m_card == number)
        return true;
    release();
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected())
        return true;
    const QString name = QStringLiteral("org.freedesktop.ReserveDevice1.Audio%1").arg(number);
    const QString path = QStringLiteral("/org/freedesktop/ReserveDevice1/Audio%1").arg(number);

    // Ready to answer before the name is ours: WirePlumber asks the new owner
    // at once, and does not watch one that gives no answer for a hand-back.
    auto *object = new ReservationObject(number);
    object->moveToThread(busThread());
    QMetaObject::invokeMethod(object, [object, path]() {
        QDBusConnection::sessionBus().registerObject(path, object, QDBusConnection::ExportAllSlots | QDBusConnection::ExportAllProperties);
    }, Qt::BlockingQueuedConnection);
    m_object = object;
    m_card = number;

    // In the queue before asking the owner to let go, so that the bus hands
    // the name over at once and nobody can slip in between.
    const QDBusMessage request = busCall(QStringLiteral("RequestName"), name, 0, true);
    if (request.type() != QDBusMessage::ReplyMessage) {
        release();
        return true; // Nobody to ask.
    }
    const uint state = request.arguments().value(0).toUInt();
    if (state == 1 || state == 4)
        return true;

    QDBusMessage ask = QDBusMessage::createMethodCall(name, path, QStringLiteral("org.freedesktop.ReserveDevice1"), QStringLiteral("RequestRelease"));
    ask << kPriority;
    const QDBusMessage answer = bus.call(ask, QDBus::Block, kPatienceMs);
    if (answer.type() != QDBusMessage::ReplyMessage || !answer.arguments().value(0).toBool()) {
        release();
        *err = QStringLiteral("The sound server would not let go of the sound card");
        return false;
    }
    for (int i = 0; i < kPatienceMs / 50; ++i) {
        const QDBusReply<QString> owner = bus.interface()->serviceOwner(name);
        if (owner.isValid() && owner.value() == bus.baseService())
            return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
    release();
    *err = QStringLiteral("The sound server would not let go of the sound card");
    return false;
}

void CardReservation::release()
{
    if (m_card < 0)
        return;
    const QString name = QStringLiteral("org.freedesktop.ReserveDevice1.Audio%1").arg(m_card);
    const QString path = QStringLiteral("/org/freedesktop/ReserveDevice1/Audio%1").arg(m_card);
    busCall(QStringLiteral("ReleaseName"), name, 0, false);
    QDBusConnection::sessionBus().unregisterObject(path);
    if (m_object)
        QMetaObject::invokeMethod(m_object, &QObject::deleteLater);
    m_object = nullptr;
    m_card = -1;
}

void CardReservation::onAskedToLetGo(std::function<void()> handler)
{
    g_askedToLetGo = std::move(handler);
}

void CardReservation::shutdown()
{
    QThread *thread = busThread();
    thread->quit();
    thread->wait();
    delete thread;
}

#include "audio_reserve.moc"

#else

// Without QtDBus there is nobody to ask: a card the sound server holds is
// reported as busy.
class ReservationObject {};

CardReservation::~CardReservation() = default;
bool CardReservation::take(int, QString *) { return true; }
void CardReservation::release() {}
void CardReservation::onAskedToLetGo(std::function<void()>) {}
void CardReservation::shutdown() {}

#endif
