// Test support (only active when BEATLUMEN_TEST_DUMP is set): writes the
// visible controls with their screen geometry to a JSON file twice a second
// so an external driver can click them by label, and logs frame timing.
#pragma once

#include <QElapsedTimer>
#include <QObject>
#include <QTimer>
#include <QVector>

class QQuickItem;
class QQuickWindow;

class TestHooks : public QObject
{
    Q_OBJECT
public:
    static void attach(QQuickWindow *window);

private:
    explicit TestHooks(QQuickWindow *window, const QString &path);
    void dump();
    void frame();

    QQuickWindow *m_window;
    QString m_path;
    QTimer m_timer;
    QElapsedTimer m_clock;
    QVector<double> m_frames;
    QElapsedTimer m_statsClock;
};
