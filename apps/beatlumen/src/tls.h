#pragma once

#include "core.h"

// TLS connections for https:// servers, provided to the Rust core through
// BlCallbacks (QSslSocket: the platform's TLS backend, certificates checked
// against the system's trusted authorities and the host name).
class QQmlEngine;
class QQmlNetworkAccessManagerFactory;
class QNetworkAccessManager;
class QObject;
class QString;
class QByteArray;
class QImage;
class QSize;

namespace tls {
// Fills the tls_* callbacks when the platform supports TLS.
void configure(BlCallbacks &cb);
// Fingerprint (SHA-256, hex) of a certificate the user trusts for
// "host:port"; empty removes it. Applies to Qt's own network requests
// (images loaded by QML) through qmlNetworkFactory().
void setPin(const QString &hostPort, const QByteArray &fingerprint);
QByteArray pinFor(const QString &hostPort);
// Network access for the QML engine that honours trusted fingerprints.
QQmlNetworkAccessManagerFactory *qmlNetworkFactory();
#if !defined(Q_OS_WASM) && !defined(Q_OS_ANDROID)
// Network access that honours trusted fingerprints.
QNetworkAccessManager *pinnedNetworkAccess(QObject *parent);
#endif
// Registers the image providers on native platforms: "server" (pictures
// from the network, fetched by the core's HTTP client and its TLS) and
// "thumb" (local picture files); both decode with the core's decoders.
void addImageProvider(QQmlEngine *engine);
// Decodes one of those pictures (an image://server/… or image://thumb/…
// URL from the core) covering `want`, from any thread; null if unreadable.
QImage loadPicture(const QString &url, QSize want);
} // namespace tls
