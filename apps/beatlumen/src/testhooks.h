// Test support (only active when BEATLUMEN_TEST_DUMP is set): writes the
// visible controls with their screen geometry to a JSON file twice a second
// so an external driver can click them by label, and logs frame timing.
// A driver can also write commands to "<dump file>.cmd" (one per line, the
// file is removed once they ran): shot <file> (also dumps), click <x> <y> [ctrl|shift],
// press <x> <y>, release <x> <y>, hover <x> <y>, drag <x1> <y1> <x2> <y2>, scroll <x> <y> <steps>, key <sequence>, text <text>, size <w> <h>,
// set <window property> <JSON value>, core <command> <JSON arguments>.
// BEATLUMEN_TEST_SIZE=<w>x<h> sets the size of the window at the start.
// BEATLUMEN_TEST_MOBILE=1 lays the app out for fingers, as on a phone.
// frames <file> <seconds>: writes the brightness of the window's picture, on
// a grid of 8x6 cells, about 20 times a second (for measuring flashes).
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
    void poll();
    void run(const QString &line);
    void sampleFrame();

    QQuickWindow *m_window;
    QString m_path;
    QTimer m_timer;
    QTimer m_commands;
    QTimer m_sampler;
    QString m_framesPath;
    QElapsedTimer m_framesClock;
    int m_framesMs = 0;
    QElapsedTimer m_clock;
    QVector<double> m_frames;
    QElapsedTimer m_statsClock;
};
