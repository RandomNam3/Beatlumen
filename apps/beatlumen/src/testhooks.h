// Test support (only active when BEATLUMEN_TEST_DUMP is set): writes the
// visible controls with their screen geometry to a JSON file twice a second
// so an external driver can click them by label, and logs frame timing.
// A driver can also write commands to "<dump file>.cmd" (one per line, the
// file is removed once they ran): shot <file>, click <x> <y>, hover <x> <y>,
// scroll <x> <y> <steps>, key <sequence>, text <text>, size <w> <h>,
// set <window property> <JSON value>, core <command> <JSON arguments>.
// BEATLUMEN_TEST_SIZE=<w>x<h> sets the size of the window at the start.
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

    QQuickWindow *m_window;
    QString m_path;
    QTimer m_timer;
    QTimer m_commands;
    QElapsedTimer m_clock;
    QVector<double> m_frames;
    QElapsedTimer m_statsClock;
};
