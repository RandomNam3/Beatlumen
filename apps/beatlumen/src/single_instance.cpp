#include "single_instance.h"

#include <QCryptographicHash>
#include <QDir>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLocalServer>
#include <QLocalSocket>
#include <QLockFile>
#include <QStandardPaths>
#include <QTimer>

namespace {

// Longest message a later start may send, and how many files it may name.
constexpr qint64 kMaxMessage = 256 * 1024;
constexpr int kMaxFiles = 1000;

// The local socket of the app using `dataDir`: in the user's runtime folder
// where there is one (not shared with other users), else Qt's default place
// (a named pipe on Windows, restricted to the user below).
QString socketName(const QString &dataDir)
{
    const QByteArray id = QCryptographicHash::hash(QDir(dataDir).absolutePath().toUtf8(), QCryptographicHash::Sha256).toHex().left(16);
    const QString name = QStringLiteral("beatlumen-") + QString::fromLatin1(id);
#if defined(Q_OS_UNIX)
    const QString runtime = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation);
    if (!runtime.isEmpty())
        return runtime + QLatin1Char('/') + name;
#endif
    return name;
}

// The files among a start's arguments, as absolute paths (the running app
// has another working folder). Options are left to the running app's own
// settings.
QStringList filesOf(const QStringList &args)
{
    QStringList files;
    for (int i = 0; i < args.size(); ++i) {
        const QString &a = args[i];
        if (a.startsWith(QLatin1String("--"))) {
            // Options with a value.
            if (a != QLatin1String("--fullscreen"))
                ++i;
            continue;
        }
        files << QFileInfo(a).absoluteFilePath();
    }
    return files;
}

} // namespace

SingleInstance::SingleInstance(QObject *parent)
    : QObject(parent)
{
}

SingleInstance::~SingleInstance()
{
    delete m_lock;
}

SingleInstance *SingleInstance::claim(const QString &dataDir, const QStringList &args, QObject *parent)
{
    QDir().mkpath(dataDir);
    auto *guard = new SingleInstance(parent);
    guard->m_lock = new QLockFile(dataDir + QStringLiteral("/instance.lock"));
    // Stale only when the process that wrote it is gone (Qt checks the
    // process id and name), however long the app has been running.
    guard->m_lock->setStaleLockTime(0);
    const QString name = socketName(dataDir);
    if (guard->m_lock->tryLock(0)) {
        guard->listen(name);
        return guard;
    }
    if (guard->m_lock->error() == QLockFile::LockFailedError) {
        // Another instance runs: hand it the files (or just wake it).
        QLocalSocket socket;
        socket.connectToServer(name);
        if (socket.waitForConnected(2000)) {
            QJsonArray files;
            for (const QString &f : filesOf(args))
                files.append(f);
            socket.write(QJsonDocument(files).toJson(QJsonDocument::Compact) + '\n');
            if (socket.waitForBytesWritten(2000)) {
                socket.disconnectFromServer();
                qInfo("Beatlumen is already running with this data folder; handed over to it");
                delete guard;
                return nullptr;
            }
        }
        qWarning("Beatlumen seems to be running already, but does not answer; starting anyway");
    } else {
        qWarning("cannot lock the data folder (%d); starting anyway", int(guard->m_lock->error()));
    }
    return guard;
}

void SingleInstance::listen(const QString &name)
{
    m_server = new QLocalServer(this);
    m_server->setSocketOptions(QLocalServer::UserAccessOption);
    if (!m_server->listen(name)) {
        // Left behind by a crash (this process holds the lock now).
        QLocalServer::removeServer(name);
        if (!m_server->listen(name)) {
            qWarning("no hand-over socket: %s", qPrintable(m_server->errorString()));
            return;
        }
    }
    connect(m_server, &QLocalServer::newConnection, this, [this]() {
        while (QLocalSocket *c = m_server->nextPendingConnection()) {
            connect(c, &QLocalSocket::disconnected, c, &QObject::deleteLater);
            // A start that sends nothing (or too much) is dropped.
            QTimer::singleShot(3000, c, [c]() { c->abort(); });
            connect(c, &QLocalSocket::readyRead, this, [this, c]() {
                if (c->bytesAvailable() > kMaxMessage) {
                    c->abort();
                    return;
                }
                if (!c->canReadLine())
                    return;
                const QJsonArray a = QJsonDocument::fromJson(c->readLine(kMaxMessage)).array();
                c->disconnectFromServer();
                QStringList files;
                for (const auto &v : a) {
                    const QString f = v.toString();
                    if (!f.isEmpty() && f.size() <= 4096 && files.size() < kMaxFiles)
                        files << f;
                }
                emit received(files);
            });
        }
    });
}
