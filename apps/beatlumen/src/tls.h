#pragma once

#include "core.h"

// TLS connections for https:// servers, provided to the Rust core through
// BlCallbacks (QSslSocket: the platform's TLS backend, certificates checked
// against the system's trusted authorities and the host name).
class QQmlEngine;
class QQmlNetworkAccessManagerFactory;
class QString;
class QByteArray;

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
// Registers the "server" image provider (images from the music server,
// loaded by the core's HTTP client and its TLS) on native platforms.
void addImageProvider(QQmlEngine *engine);
} // namespace tls
