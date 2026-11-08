// One running app per data folder on desktops. Two instances would write
// the same settings, library and offline files and announce the same
// speakers (DLNA, AirPlay) on the network. A second start hands its files
// to the running app over a local socket of the user and ends.
#pragma once

#include <QObject>
#include <QStringList>

class QLocalServer;
class QLockFile;

class SingleInstance : public QObject
{
    Q_OBJECT
public:
    // Claims `dataDir` for this process. Returns null when another running
    // instance took `args` (this one should end); otherwise the guard, also
    // when the check is not possible (the app then starts as before).
    static SingleInstance *claim(const QString &dataDir, const QStringList &args, QObject *parent);
    ~SingleInstance() override;

signals:
    // A later start's files (absolute paths), to play here.
    void received(const QStringList &files);

private:
    explicit SingleInstance(QObject *parent);
    void listen(const QString &name);

    QLockFile *m_lock = nullptr;
    QLocalServer *m_server = nullptr;
};
