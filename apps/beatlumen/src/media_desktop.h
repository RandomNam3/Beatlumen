// The desktop's media controls: media keys, panel and lock screen players.
// Linux: MPRIS on the session bus (GNOME, KDE, … send media keys there).
// Windows: the System Media Transport Controls (media keys, the volume
// flyout, the lock screen). Mirrors the player state and runs the commands
// that come back, like AndroidMedia on Android.
#pragma once

#include <QElapsedTimer>
#include <QObject>
#include <QPointer>
#include <QString>
#include <QStringList>

class Bridge;
class QNetworkAccessManager;
class QWindow;

class DesktopMedia : public QObject
{
    Q_OBJECT
public:
    explicit DesktopMedia(Bridge *bridge);
    ~DesktopMedia() override;

    // The main window (Windows needs it for the media controls).
    void setWindow(QWindow *window);

    struct Snapshot
    {
        QString id, title, album, artUrl; // artUrl: file:// URL or empty
        QStringList artists;
        qint64 lengthUs = 0;
        QString status = QStringLiteral("Stopped"); // MPRIS PlaybackStatus
        QString loop = QStringLiteral("None");      // MPRIS LoopStatus
        bool shuffle = false;
        double volume = 1.0;
        bool canNext = false, canPrevious = false, canPlay = false, canSeek = false;
        bool operator==(const Snapshot &o) const
        {
            return id == o.id && title == o.title && album == o.album && artUrl == o.artUrl && artists == o.artists
                && lengthUs == o.lengthUs && status == o.status && loop == o.loop && shuffle == o.shuffle
                && volume == o.volume && canNext == o.canNext && canPrevious == o.canPrevious && canPlay == o.canPlay
                && canSeek == o.canSeek;
        }
    };

    const Snapshot &snapshot() const { return m_last; }
    qint64 positionUs() const;

    // Commands from the system (any thread).
    void command(const QString &cmd);
    void seekTo(qint64 us);
    void seekBy(qint64 us);
    void setVolume(double v);
    void setShuffle(bool on);
    void setLoop(const QString &mpris);
    void raise();

signals:
    void changed(const DesktopMedia::Snapshot &before);
    void seeked(qint64 us);

private:
    void sync();
    void checkPosition();
    void fetchCover(const QString &url);

    Bridge *m_bridge;
    QPointer<QWindow> m_window;
    Snapshot m_last;
    QString m_coverId;     // track whose extracted cover we asked the core for
    QString m_coverUrl;    // and the URL it reported (coverReady)
    QString m_coverSource; // server cover being or been downloaded
    QString m_coverFile;   // its local copy
    QNetworkAccessManager *m_net = nullptr;
    double m_lastPos = 0;
    QElapsedTimer m_posClock;
    class Backend;
    Backend *m_backend = nullptr;
};
