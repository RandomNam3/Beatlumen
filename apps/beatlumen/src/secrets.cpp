#include "secrets.h"
#include "bridge.h"

#include <QByteArray>
#include <QPointer>
#include <QString>
#include <QVariantMap>

namespace {

QPointer<Bridge> g_bridge;

// Delivers a read result to the core on the Qt thread.
[[maybe_unused]] void deliver(const QString &key, const QByteArray &value)
{
    QPointer<Bridge> b = g_bridge;
    if (!b)
        return;
    QMetaObject::invokeMethod(b, [b, key, value]() {
        if (b)
            b->call(QStringLiteral("vault.secret"), {{QStringLiteral("key"), key},
                                                    {QStringLiteral("value"), QString::fromLatin1(value.toBase64())}});
    }, Qt::QueuedConnection);
}

} // namespace

#if defined(Q_OS_ANDROID)

// ---- Android: SecretStore.java (AES key in the Android Keystore) -----------

#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>

namespace {

constexpr const char *kClass = "io/github/randomnam3/beatlumen/SecretStore";

void store(void *, const char *key, const uint8_t *data, size_t len)
{
    QJniEnvironment env;
    jbyteArray arr = env->NewByteArray(jsize(len));
    if (!arr)
        return;
    if (len)
        env->SetByteArrayRegion(arr, 0, jsize(len), reinterpret_cast<const jbyte *>(data));
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject::callStaticMethod<jboolean>(kClass, "store", "(Landroid/content/Context;Ljava/lang/String;[B)Z", ctx.object(),
                                           QJniObject::fromString(QString::fromUtf8(key)).object<jstring>(), arr);
    env->DeleteLocalRef(arr);
}

int32_t load(void *, const char *key)
{
    const QString k = QString::fromUtf8(key);
    QJniObject ctx = QNativeInterface::QAndroidApplication::context();
    QJniObject r = QJniObject::callStaticObjectMethod(kClass, "load", "(Landroid/content/Context;Ljava/lang/String;)[B", ctx.object(),
                                                      QJniObject::fromString(k).object<jstring>());
    QByteArray out;
    if (r.isValid()) {
        QJniEnvironment env;
        auto arr = static_cast<jbyteArray>(r.object());
        const jsize n = env->GetArrayLength(arr);
        out.resize(n);
        env->GetByteArrayRegion(arr, 0, n, reinterpret_cast<jbyte *>(out.data()));
    }
    deliver(k, out);
    return 1;
}

} // namespace

void secrets::configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    cb.secret_store = store;
    cb.secret_load = load;
}

#elif defined(Q_OS_WIN)

// ---- Windows: DPAPI (bound to the Windows account) -------------------------

#include <QCryptographicHash>
#include <QDir>
#include <QFile>
#include <QSaveFile>
#include <QStandardPaths>

#include <windows.h>
#include <dpapi.h>

namespace {

const char kEntropy[] = "beatlumen vault v1";

QString pathFor(const QString &key)
{
    const QString dir = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation) + QStringLiteral("/secrets");
    QDir().mkpath(dir);
    return dir + QLatin1Char('/') + QString::fromLatin1(QCryptographicHash::hash(key.toUtf8(), QCryptographicHash::Sha256).toHex()) + QStringLiteral(".bin");
}

void store(void *, const char *key, const uint8_t *data, size_t len)
{
    const QString path = pathFor(QString::fromUtf8(key));
    if (!len) {
        QFile::remove(path);
        return;
    }
    DATA_BLOB in{DWORD(len), const_cast<BYTE *>(data)};
    DATA_BLOB entropy{DWORD(sizeof kEntropy - 1), reinterpret_cast<BYTE *>(const_cast<char *>(kEntropy))};
    DATA_BLOB out{};
    if (!CryptProtectData(&in, L"Beatlumen", &entropy, nullptr, nullptr, CRYPTPROTECT_UI_FORBIDDEN, &out))
        return;
    QSaveFile f(path);
    if (f.open(QIODevice::WriteOnly)) {
        f.write(reinterpret_cast<const char *>(out.pbData), out.cbData);
        f.commit();
    }
    LocalFree(out.pbData);
}

int32_t load(void *, const char *key)
{
    const QString k = QString::fromUtf8(key);
    QByteArray value;
    QFile f(pathFor(k));
    if (f.open(QIODevice::ReadOnly)) {
        QByteArray blob = f.readAll();
        DATA_BLOB in{DWORD(blob.size()), reinterpret_cast<BYTE *>(blob.data())};
        DATA_BLOB entropy{DWORD(sizeof kEntropy - 1), reinterpret_cast<BYTE *>(const_cast<char *>(kEntropy))};
        DATA_BLOB out{};
        if (CryptUnprotectData(&in, nullptr, &entropy, nullptr, nullptr, CRYPTPROTECT_UI_FORBIDDEN, &out)) {
            value = QByteArray(reinterpret_cast<const char *>(out.pbData), int(out.cbData));
            SecureZeroMemory(out.pbData, out.cbData);
            LocalFree(out.pbData);
        }
    }
    deliver(k, value);
    return 1;
}

} // namespace

void secrets::configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    cb.secret_store = store;
    cb.secret_load = load;
}

#elif defined(BEATLUMEN_MPRIS)

// ---- Linux: the Secret Service (GNOME Keyring, KWallet, KeePassXC) ---------
// Calls run one after another on a thread of their own: unlocking the
// keyring may show a prompt and wait for the user.

#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusInterface>
#include <QDBusMetaType>
#include <QDBusObjectPath>
#include <QDBusReply>
#include <QDBusVariant>
#include <QEventLoop>
#include <QThread>
#include <QTimer>

namespace {

constexpr const char *kService = "org.freedesktop.secrets";
constexpr const char *kPath = "/org/freedesktop/secrets";
constexpr const char *kServiceIface = "org.freedesktop.Secret.Service";

using StringMap = QMap<QString, QString>;

struct SecretValue
{
    QDBusObjectPath session;
    QByteArray parameters;
    QByteArray value;
    QString contentType;
};

QDBusArgument &operator<<(QDBusArgument &a, const SecretValue &s)
{
    a.beginStructure();
    a << s.session << s.parameters << s.value << s.contentType;
    a.endStructure();
    return a;
}

const QDBusArgument &operator>>(const QDBusArgument &a, SecretValue &s)
{
    a.beginStructure();
    a >> s.session >> s.parameters >> s.value >> s.contentType;
    a.endStructure();
    return a;
}

} // namespace

Q_DECLARE_METATYPE(SecretValue)

namespace {

class Keyring : public QObject
{
    Q_OBJECT
public:
    Keyring()
    {
        qDBusRegisterMetaType<StringMap>();
        qDBusRegisterMetaType<SecretValue>();
    }

public slots:
    void store(const QString &key, const QByteArray &value)
    {
        if (!openSession())
            return;
        QDBusInterface svc(QLatin1String(kService), QLatin1String(kPath), QLatin1String(kServiceIface), bus());
        if (value.isEmpty()) {
            QDBusMessage r = svc.call(QStringLiteral("SearchItems"), QVariant::fromValue(attributes(key)));
            if (r.type() != QDBusMessage::ReplyMessage || r.arguments().size() < 2)
                return;
            QList<QDBusObjectPath> items = qdbus_cast<QList<QDBusObjectPath>>(r.arguments().at(0));
            items += qdbus_cast<QList<QDBusObjectPath>>(r.arguments().at(1));
            for (const QDBusObjectPath &item : items) {
                QDBusInterface it(QLatin1String(kService), item.path(), QStringLiteral("org.freedesktop.Secret.Item"), bus());
                QDBusReply<QDBusObjectPath> prompt = it.call(QStringLiteral("Delete"));
                if (prompt.isValid())
                    waitPrompt(prompt.value());
            }
            return;
        }
        QDBusReply<QDBusObjectPath> alias = svc.call(QStringLiteral("ReadAlias"), QStringLiteral("default"));
        if (!alias.isValid() || alias.value().path() == QLatin1String("/"))
            return;
        const QDBusObjectPath collection = alias.value();
        if (!unlock({collection}))
            return;
        QVariantMap props;
        props.insert(QStringLiteral("org.freedesktop.Secret.Item.Label"), QStringLiteral("Beatlumen: ") + key.section(QLatin1Char(' '), 1));
        props.insert(QStringLiteral("org.freedesktop.Secret.Item.Attributes"), QVariant::fromValue(attributes(key)));
        SecretValue secret{m_session, QByteArray(), value, QStringLiteral("application/json")};
        QDBusInterface col(QLatin1String(kService), collection.path(), QStringLiteral("org.freedesktop.Secret.Collection"), bus());
        QDBusMessage r = col.call(QStringLiteral("CreateItem"), props, QVariant::fromValue(secret), true);
        if (r.type() == QDBusMessage::ReplyMessage && r.arguments().size() >= 2)
            waitPrompt(qdbus_cast<QDBusObjectPath>(r.arguments().at(1)));
    }

    void load(const QString &key)
    {
        QByteArray value;
        if (openSession()) {
            QDBusInterface svc(QLatin1String(kService), QLatin1String(kPath), QLatin1String(kServiceIface), bus());
            QDBusMessage r = svc.call(QStringLiteral("SearchItems"), QVariant::fromValue(attributes(key)));
            if (r.type() == QDBusMessage::ReplyMessage && r.arguments().size() >= 2) {
                QList<QDBusObjectPath> items = qdbus_cast<QList<QDBusObjectPath>>(r.arguments().at(0));
                const QList<QDBusObjectPath> locked = qdbus_cast<QList<QDBusObjectPath>>(r.arguments().at(1));
                if (items.isEmpty() && !locked.isEmpty() && unlock(locked))
                    items = locked;
                if (!items.isEmpty())
                    value = secretOf(svc, items.first());
            }
        }
        deliver(key, value);
    }

private slots:
    void promptCompleted(bool dismissed, const QDBusVariant &)
    {
        m_dismissed = dismissed;
        if (m_loop)
            m_loop->quit();
    }

private:
    static QDBusConnection bus() { return QDBusConnection::sessionBus(); }

    static StringMap attributes(const QString &key)
    {
        return {{QStringLiteral("application"), QStringLiteral("io.github.randomnam3.beatlumen")},
                {QStringLiteral("beatlumen-key"), key}};
    }

    // "plain": the session bus belongs to this user only.
    bool openSession()
    {
        if (!m_session.path().isEmpty())
            return true;
        QDBusInterface svc(QLatin1String(kService), QLatin1String(kPath), QLatin1String(kServiceIface), bus());
        QDBusMessage r = svc.call(QStringLiteral("OpenSession"), QStringLiteral("plain"), QVariant::fromValue(QDBusVariant(QString())));
        if (r.type() != QDBusMessage::ReplyMessage || r.arguments().size() < 2)
            return false;
        m_session = qdbus_cast<QDBusObjectPath>(r.arguments().at(1));
        return !m_session.path().isEmpty();
    }

    bool unlock(const QList<QDBusObjectPath> &objects)
    {
        QDBusInterface svc(QLatin1String(kService), QLatin1String(kPath), QLatin1String(kServiceIface), bus());
        QDBusMessage r = svc.call(QStringLiteral("Unlock"), QVariant::fromValue(objects));
        if (r.type() != QDBusMessage::ReplyMessage || r.arguments().size() < 2)
            return false;
        return waitPrompt(qdbus_cast<QDBusObjectPath>(r.arguments().at(1)));
    }

    // Shows the keyring's prompt (if it needs one) and waits for it.
    bool waitPrompt(const QDBusObjectPath &prompt)
    {
        if (prompt.path().isEmpty() || prompt.path() == QLatin1String("/"))
            return true;
        bus().connect(QLatin1String(kService), prompt.path(), QStringLiteral("org.freedesktop.Secret.Prompt"), QStringLiteral("Completed"),
                      this, SLOT(promptCompleted(bool, QDBusVariant)));
        QEventLoop loop;
        m_loop = &loop;
        m_dismissed = true;
        QTimer::singleShot(120000, &loop, &QEventLoop::quit);
        QDBusInterface p(QLatin1String(kService), prompt.path(), QStringLiteral("org.freedesktop.Secret.Prompt"), bus());
        p.call(QStringLiteral("Prompt"), QString());
        loop.exec();
        m_loop = nullptr;
        bus().disconnect(QLatin1String(kService), prompt.path(), QStringLiteral("org.freedesktop.Secret.Prompt"), QStringLiteral("Completed"),
                         this, SLOT(promptCompleted(bool, QDBusVariant)));
        return !m_dismissed;
    }

    QByteArray secretOf(QDBusInterface &svc, const QDBusObjectPath &item)
    {
        QDBusMessage r = svc.call(QStringLiteral("GetSecrets"), QVariant::fromValue(QList<QDBusObjectPath>{item}),
                                  QVariant::fromValue(m_session));
        if (r.type() != QDBusMessage::ReplyMessage || r.arguments().isEmpty())
            return {};
        const QDBusArgument arg = r.arguments().at(0).value<QDBusArgument>();
        QByteArray value;
        arg.beginMap();
        while (!arg.atEnd()) {
            QDBusObjectPath path;
            SecretValue s;
            arg.beginMapEntry();
            arg >> path >> s;
            arg.endMapEntry();
            if (path == item)
                value = s.value;
        }
        arg.endMap();
        return value;
    }

    QDBusObjectPath m_session;
    QEventLoop *m_loop = nullptr;
    bool m_dismissed = false;
};

Keyring *g_keyring = nullptr;

void store(void *, const char *key, const uint8_t *data, size_t len)
{
    const QString k = QString::fromUtf8(key);
    const QByteArray v(reinterpret_cast<const char *>(data), qsizetype(len));
    QMetaObject::invokeMethod(g_keyring, [k, v]() { g_keyring->store(k, v); }, Qt::QueuedConnection);
}

int32_t load(void *, const char *key)
{
    const QString k = QString::fromUtf8(key);
    QMetaObject::invokeMethod(g_keyring, [k]() { g_keyring->load(k); }, Qt::QueuedConnection);
    return 1;
}

} // namespace

void secrets::configure(BlCallbacks &cb, Bridge *bridge)
{
    g_bridge = bridge;
    QDBusConnectionInterface *bus = QDBusConnection::sessionBus().interface();
    if (!bus)
        return;
    const bool available = bus->isServiceRegistered(QLatin1String(kService)).value()
        || bus->activatableServiceNames().value().contains(QLatin1String(kService));
    if (!available)
        return;
    auto *thread = new QThread(bridge);
    thread->setObjectName(QStringLiteral("keyring"));
    g_keyring = new Keyring;
    g_keyring->moveToThread(thread);
    QObject::connect(thread, &QThread::finished, g_keyring, &QObject::deleteLater);
    QObject::connect(bridge, &QObject::destroyed, thread, [thread]() { thread->quit(); thread->wait(2000); });
    thread->start();
    cb.secret_store = store;
    cb.secret_load = load;
}

#include "secrets.moc"

#else

void secrets::configure(BlCallbacks &, Bridge *bridge)
{
    g_bridge = bridge;
}

#endif
