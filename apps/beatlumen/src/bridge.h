// Connects the QML user interface with the Rust core: commands, state,
// events, the visualizer underlay and audio output.
#pragma once

#include <QColor>
#include <QJSValue>
#include <QElapsedTimer>
#include <QObject>
#include <QTimer>
#include <QUrl>
#include <QVariant>
#include <QVariantMap>

#include "core.h"

class QQuickWindow;
class AudioOutput;

class Bridge : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QVariantMap state READ state NOTIFY stateChanged)
    Q_PROPERTY(double position READ position NOTIFY positionChanged)
    Q_PROPERTY(bool systemDark READ systemDark NOTIFY systemThemeChanged)
    Q_PROPERTY(QColor systemAccent READ systemAccent NOTIFY systemThemeChanged)
    Q_PROPERTY(QString platform READ platform CONSTANT)
    Q_PROPERTY(bool mobile READ mobile CONSTANT)
    Q_PROPERTY(QString version READ version CONSTANT)
    // File dialogs of the system can be used (Linux: the XDG desktop portal
    // answers); otherwise the app shows its own file picker.
    Q_PROPERTY(bool nativeDialogs READ nativeDialogs NOTIFY nativeDialogsChanged)

public:
    Bridge(const QStringList &args, QObject *parent = nullptr);
    ~Bridge() override;

    QVariantMap state() const { return m_state; }
    double position() const { return m_position; }
    bool systemDark() const;
    QColor systemAccent() const;
    QString platform() const;
    bool mobile() const;
    QString version() const;
    bool nativeDialogs() const { return m_nativeDialogs; }

    // Executes a core command ({"cmd": name, ...args}) and returns the result.
    Q_INVOKABLE QJSValue call(const QString &cmd, const QVariantMap &args = {});
    // Renders the visualizer underneath the QML scene of `window`.
    Q_INVOKABLE void attachWindow(QQuickWindow *window);
    // Opens files: local paths/URLs, Android content URIs, browser uploads.
    Q_INVOKABLE void openUrls(const QList<QUrl> &urls, bool play = true);
    void openPaths(const QStringList &paths, bool play);
    // Converts file URLs from dialogs into local paths (content URIs are
    // copied into the cache first).
    Q_INVOKABLE QStringList localPaths(const QList<QUrl> &urls);
    Q_INVOKABLE void setKeepScreenOn(bool on);
    // Hides Android's status and navigation bars (immersive mode).
    Q_INVOKABLE void setSystemBarsHidden(bool hidden);
    Q_INVOKABLE void requestMediaPermission();
    Q_INVOKABLE void copyText(const QString &text);
    // Android: sends the app to the background (returns false elsewhere).
    Q_INVOKABLE bool moveTaskToBack();
    // Browser file picker (WebAssembly): files are handed to the core as bytes.
    Q_INVOKABLE void pickFiles(const QString &accept, bool multiple = true);

    BlCore *core() const { return m_core; }
    void log(const QString &msg);

signals:
    void stateChanged();
    void positionChanged();
    void systemThemeChanged();
    void nativeDialogsChanged();
    void toast(const QString &text, bool error);
    void coverReady(const QString &id, const QString &url);
    void mediaPermissionGranted();

private:
    void tick();
    void handleEvents(const QByteArray &json);
    void renderUnderlay();

    void probeNativeDialogs();

    BlCore *m_core = nullptr;
#if defined(Q_OS_LINUX) && !defined(Q_OS_ANDROID)
    bool m_nativeDialogs = false;
#else
    bool m_nativeDialogs = true;
#endif
    QVariantMap m_state;
    double m_position = 0;
    QTimer m_timer;
    QQuickWindow *m_window = nullptr;
    class DesktopMedia *m_desktopMedia = nullptr;
    bool m_glReady = false;
    bool m_glFailed = false;
    QElapsedTimer m_frameClock;
    AudioOutput *m_audio = nullptr;
    bool m_keepScreenOn = false;
    int m_persistCounter = 0;
};
