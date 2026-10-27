// Android background playback: mirrors the player state into the Java side
// (android/src/.../MediaBridge.java: MediaSession, media notification,
// foreground service, audio focus) and runs the media button commands that
// come back from it (lock screen, headset, Bluetooth, notification).
#pragma once

#include <QElapsedTimer>
#include <QObject>
#include <QString>

class Bridge;

class AndroidMedia : public QObject
{
public:
    explicit AndroidMedia(Bridge *bridge);
    ~AndroidMedia() override;

    // Called from Java (Android main thread); executed on the Qt thread.
    static void postCommand(const QString &cmd);
    static void postSeek(double seconds);

private:
    struct Snapshot
    {
        QString title, artist, album, cover;
        qint64 durationMs = -1;
        int state = -1;
        bool operator==(const Snapshot &o) const
        {
            return title == o.title && artist == o.artist && album == o.album && cover == o.cover
                && durationMs == o.durationMs && state == o.state;
        }
    };

    void sync(bool force);
    void checkPosition();
    void askNotificationPermission();

    Bridge *m_bridge;
    Snapshot m_last;
    QString m_coverId;    // track whose extracted cover we asked the core for
    QString m_coverUrl;   // and the URL it reported (coverReady)
    double m_lastPos = 0; // position sent with the last update
    QElapsedTimer m_posClock;
    bool m_askedNotifications = false;
};
