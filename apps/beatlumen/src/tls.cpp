#include "tls.h"

#include <QtGlobal>

#include <algorithm>

#include <QCryptographicHash>
#include <QHash>
#include <QImage>
#include <QMutex>
#include <QQmlEngine>
#include <QQmlNetworkAccessManagerFactory>
#include <QQuickImageProvider>
#include <QUrl>
#include <QString>
#if !defined(Q_OS_WASM) && !defined(Q_OS_ANDROID)
#include <QNetworkAccessManager>
#include <QNetworkReply>
#include <QSslError>
#endif

namespace {
QMutex pinMutex;
QHash<QString, QByteArray> pins;

#if !defined(Q_OS_WASM) && !defined(Q_OS_ANDROID)
// QML's network access (cover images): a certificate the system does not
// trust is accepted only if the user trusts its fingerprint.
class PinnedFactory : public QQmlNetworkAccessManagerFactory
{
public:
    QNetworkAccessManager *create(QObject *parent) override
    {
        return tls::pinnedNetworkAccess(parent);
    }
};
#endif
} // namespace

#if !defined(Q_OS_WASM) && !defined(Q_OS_ANDROID)
QNetworkAccessManager *tls::pinnedNetworkAccess(QObject *parent)
{
    auto *nam = new QNetworkAccessManager(parent);
    QObject::connect(nam, &QNetworkAccessManager::sslErrors, nam, [](QNetworkReply *r, const QList<QSslError> &) {
        const QUrl u = r->url();
        const QByteArray want = tls::pinFor(u.host().toLower() + QLatin1Char(':') + QString::number(u.port(443)));
        const QByteArray got = r->sslConfiguration().peerCertificate().digest(QCryptographicHash::Sha256).toHex();
        if (!want.isEmpty() && want == got)
            r->ignoreSslErrors();
    });
    return nam;
}
#endif

void tls::setPin(const QString &hostPort, const QByteArray &fingerprint)
{
    QMutexLocker lock(&pinMutex);
    if (fingerprint.isEmpty())
        pins.remove(hostPort.toLower());
    else
        pins.insert(hostPort.toLower(), fingerprint.toLower());
}

QByteArray tls::pinFor(const QString &hostPort)
{
    QMutexLocker lock(&pinMutex);
    return pins.value(hostPort.toLower());
}

#if defined(Q_OS_ANDROID)

// Android: the system's TLS through TlsBridge.java (Qt's TLS backend would
// need a bundled OpenSSL).
#include <QJniEnvironment>
#include <QJniObject>

#include <cstring>

namespace {

constexpr const char *kTls = "io/github/randomnam3/beatlumen/TlsBridge";

void setError(char *err, size_t len, const QString &msg)
{
    if (!err || len == 0)
        return;
    const QByteArray m = msg.toUtf8();
    const size_t n = std::min(len - 1, size_t(m.size()));
    std::memcpy(err, m.constData(), n);
    err[n] = 0;
}

int64_t tlsOpen(const char *host, uint16_t port, uint32_t timeoutMs, const char *pin, char *err, size_t errLen)
{
    QJniEnvironment env;
    const jlong h = QJniObject::callStaticMethod<jlong>(kTls, "open", "(Ljava/lang/String;IILjava/lang/String;)J",
                                                        QJniObject::fromString(QString::fromUtf8(host)).object<jstring>(), jint(port),
                                                        jint(timeoutMs), QJniObject::fromString(QString::fromUtf8(pin ? pin : "")).object<jstring>());
    if (env.checkAndClearExceptions() || h <= 0) {
        const QString msg = QJniObject::callStaticObjectMethod(kTls, "lastError", "()Ljava/lang/String;").toString();
        env.checkAndClearExceptions();
        setError(err, errLen, msg.isEmpty() ? QStringLiteral("TLS connection failed") : msg);
        return 0;
    }
    return h;
}

int64_t tlsRead(int64_t h, uint8_t *buf, size_t len, uint32_t timeoutMs)
{
    QJniEnvironment env;
    const jint n = jint(std::min<size_t>(len, 64 * 1024));
    jbyteArray arr = env->NewByteArray(n);
    if (!arr)
        return -1;
    const jint got = QJniObject::callStaticMethod<jint>(kTls, "read", "(J[BII)I", jlong(h), arr, n, jint(timeoutMs));
    if (env.checkAndClearExceptions()) {
        env->DeleteLocalRef(arr);
        return -1;
    }
    if (got > 0)
        env->GetByteArrayRegion(arr, 0, got, reinterpret_cast<jbyte *>(buf));
    env->DeleteLocalRef(arr);
    return got;
}

int64_t tlsWrite(int64_t h, const uint8_t *buf, size_t len, uint32_t)
{
    QJniEnvironment env;
    jbyteArray arr = env->NewByteArray(jint(len));
    if (!arr)
        return -1;
    env->SetByteArrayRegion(arr, 0, jint(len), reinterpret_cast<const jbyte *>(buf));
    const jint n = QJniObject::callStaticMethod<jint>(kTls, "write", "(J[BI)I", jlong(h), arr, jint(len));
    env->DeleteLocalRef(arr);
    if (env.checkAndClearExceptions())
        return -1;
    return n;
}

void tlsClose(int64_t h)
{
    QJniEnvironment env;
    QJniObject::callStaticMethod<void>(kTls, "close", "(J)V", jlong(h));
    env.checkAndClearExceptions();
}

} // namespace

QQmlNetworkAccessManagerFactory *tls::qmlNetworkFactory()
{
    return nullptr; // images from servers go through the "server" image provider
}

void tls::configure(BlCallbacks &cb)
{
    cb.tls_open = tlsOpen;
    cb.tls_read = tlsRead;
    cb.tls_write = tlsWrite;
    cb.tls_close = tlsClose;
}

#elif !defined(Q_OS_WASM)

#include <QSslCertificate>
#include <QSslSocket>

#include <atomic>
#include <cstring>
#include <memory>
#include <mutex>
#include <unordered_map>

namespace {

// Connections by handle. Each socket is created, used and destroyed on the
// thread that opened it (the core's worker threads); the map itself is
// shared, hence the mutex.
std::mutex connMutex;
std::unordered_map<int64_t, std::unique_ptr<QSslSocket>> conns;
std::atomic<int64_t> nextHandle{1};

QSslSocket *socketFor(int64_t h)
{
    std::lock_guard<std::mutex> lock(connMutex);
    auto it = conns.find(h);
    return it == conns.end() ? nullptr : it->second.get();
}

void setError(char *err, size_t len, const QString &msg)
{
    if (!err || len == 0)
        return;
    const QByteArray m = msg.toUtf8();
    const size_t n = std::min(len - 1, size_t(m.size()));
    std::memcpy(err, m.constData(), n);
    err[n] = 0;
}

// Opens a verified TLS connection. A certificate the system does not trust
// is accepted only if its SHA-256 fingerprint equals `pin` (one the user
// chose to trust); otherwise the error names its fingerprint so the user
// can compare and trust it.
int64_t tlsOpen(const char *host, uint16_t port, uint32_t timeoutMs, const char *pin, char *err, size_t errLen)
{
    if (!QSslSocket::supportsSsl()) {
        setError(err, errLen, QStringLiteral("TLS is not available on this system"));
        return 0;
    }
    auto sock = std::make_unique<QSslSocket>();
    sock->setPeerVerifyMode(QSslSocket::VerifyPeer);
    sock->setProtocol(QSsl::TlsV1_2OrLater);
    const QByteArray wanted = QByteArray(pin ? pin : "").toLower();
    QSslSocket *raw = sock.get();
    QObject::connect(raw, &QSslSocket::sslErrors, raw, [raw, wanted](const QList<QSslError> &) {
        const QByteArray got = raw->peerCertificate().digest(QCryptographicHash::Sha256).toHex();
        if (!wanted.isEmpty() && got == wanted)
            raw->ignoreSslErrors();
    }, Qt::DirectConnection);
    sock->connectToHostEncrypted(QString::fromUtf8(host), port);
    if (!sock->waitForEncrypted(int(timeoutMs))) {
        QString msg = sock->errorString();
        const auto errors = sock->sslHandshakeErrors();
        if (!errors.isEmpty()) {
            msg = errors.first().errorString();
            const QSslCertificate cert = sock->peerCertificate();
            if (!cert.isNull())
                msg += QStringLiteral(" [sha256:%1]").arg(QString::fromLatin1(cert.digest(QCryptographicHash::Sha256).toHex()));
        }
        setError(err, errLen, msg);
        return 0;
    }
    const int64_t h = nextHandle++;
    std::lock_guard<std::mutex> lock(connMutex);
    conns.emplace(h, std::move(sock));
    return h;
}

int64_t tlsRead(int64_t h, uint8_t *buf, size_t len, uint32_t timeoutMs)
{
    QSslSocket *s = socketFor(h);
    if (!s)
        return -1;
    if (s->bytesAvailable() == 0) {
        if (s->state() != QAbstractSocket::ConnectedState)
            return 0;
        if (!s->waitForReadyRead(int(timeoutMs)))
            return s->state() == QAbstractSocket::ConnectedState ? -2 : 0; // timeout vs. closed
    }
    const qint64 n = s->read(reinterpret_cast<char *>(buf), qint64(len));
    return n < 0 ? -1 : n;
}

int64_t tlsWrite(int64_t h, const uint8_t *buf, size_t len, uint32_t timeoutMs)
{
    QSslSocket *s = socketFor(h);
    if (!s)
        return -1;
    const qint64 n = s->write(reinterpret_cast<const char *>(buf), qint64(len));
    if (n < 0)
        return -1;
    while (s->encryptedBytesToWrite() > 0 || s->bytesToWrite() > 0) {
        if (!s->waitForBytesWritten(int(timeoutMs)))
            return -1;
    }
    return n;
}

void tlsClose(int64_t h)
{
    std::unique_ptr<QSslSocket> s;
    {
        std::lock_guard<std::mutex> lock(connMutex);
        auto it = conns.find(h);
        if (it == conns.end())
            return;
        s = std::move(it->second);
        conns.erase(it);
    }
    s->abort();
}

} // namespace

QQmlNetworkAccessManagerFactory *tls::qmlNetworkFactory()
{
    static PinnedFactory factory;
    return &factory;
}

void tls::configure(BlCallbacks &cb)
{
    cb.tls_open = tlsOpen;
    cb.tls_read = tlsRead;
    cb.tls_write = tlsWrite;
    cb.tls_close = tlsClose;
}

#else

// In the browser, requests go through fetch(), which handles https itself.
void tls::configure(BlCallbacks &) {}
QQmlNetworkAccessManagerFactory *tls::qmlNetworkFactory()
{
    return nullptr;
}

#endif

#if !defined(Q_OS_WASM)
namespace {
// Pixels from the core (RGBA, straight alpha), copied into a QImage; frees them.
QImage fromCore(uint8_t *data, uint32_t w, uint32_t h, size_t len)
{
    if (!data)
        return {};
    QImage img;
    if (w > 0 && h > 0 && w <= 16384 && h <= 16384 && len >= size_t(w) * h * 4)
        img = QImage(data, int(w), int(h), int(w) * 4, QImage::Format_RGBA8888).copy();
    bl_bytes_free(data, len);
    return img;
}

// Decodes the picture behind a provider id (the percent-encoded URL or
// path; a raw '#' only starts a suffix that makes the URL new for the
// image cache) with the core's decoders, covering `want`.
QImage decodeId(const QString &id, bool server, QSize want)
{
    const QString encoded = id.section(QLatin1Char('#'), 0, 0);
    const QByteArray target = QUrl::fromPercentEncoding(encoded.toUtf8()).toUtf8();
    if (target.isEmpty())
        return {};
    const uint32_t w = uint32_t(qBound(1, want.width(), 4096));
    const uint32_t h = uint32_t(qBound(1, want.height(), 4096));
    uint32_t ow = 0, oh = 0;
    size_t len = 0;
    uint8_t *data = server ? bl_image_fetch(target.constData(), w, h, &ow, &oh, &len)
                           : bl_image_thumbnail(target.constData(), w, h, &ow, &oh, &len);
    return fromCore(data, ow, oh, len);
}

// Pictures the core hands out: every image from servers, other devices,
// music files and other programs is decoded by the core's own decoders
// (Rust, with limits for sizes and memory), never by Qt's image plugins.
// image://server/<percent-encoded URL>: fetched by the core's HTTP client
// (https servers and trusted certificates work as for the music itself).
// image://thumb/<percent-encoded path>: a local file (covers taken from
// music files, artist pictures, backgrounds; very large pictures are
// scaled while they are read).
class CoreImageProvider : public QQuickImageProvider
{
public:
    CoreImageProvider(bool server, QSize fallback)
        : QQuickImageProvider(QQuickImageProvider::Image, QQmlImageProviderBase::ForceAsynchronousImageLoading)
        , m_server(server)
        , m_fallback(fallback)
    {
    }

    QImage requestImage(const QString &id, QSize *size, const QSize &requested) override
    {
        QSize want = requested.isValid() ? requested : m_fallback;
        if (want.width() <= 0)
            want.setWidth(want.height() > 0 ? want.height() : m_fallback.width());
        if (want.height() <= 0)
            want.setHeight(want.width());
        const QImage img = decodeId(id, m_server, want);
        if (size && !img.isNull())
            *size = img.size();
        return img;
    }

private:
    bool m_server;
    QSize m_fallback;
};
} // namespace

QImage tls::loadPicture(const QString &url, QSize want)
{
    static const QString server = QStringLiteral("image://server/");
    static const QString thumb = QStringLiteral("image://thumb/");
    if (url.startsWith(server))
        return decodeId(url.mid(server.size()), true, want);
    if (url.startsWith(thumb))
        return decodeId(url.mid(thumb.size()), false, want);
    return {};
}

void tls::addImageProvider(QQmlEngine *engine)
{
    engine->addImageProvider(QStringLiteral("server"), new CoreImageProvider(true, QSize(1024, 1024)));
    engine->addImageProvider(QStringLiteral("thumb"), new CoreImageProvider(false, QSize(320, 200)));
}
#else
void tls::addImageProvider(QQmlEngine *) {}
QImage tls::loadPicture(const QString &, QSize)
{
    return {};
}
#endif
