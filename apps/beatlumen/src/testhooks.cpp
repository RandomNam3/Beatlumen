#include "testhooks.h"

#include <QAccessible>
#include <QCoreApplication>
#include <QFile>
#include <QJSValue>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QKeySequence>
#include <QMouseEvent>
#include <QQmlEngine>
#include <QQuickItem>
#include <QQuickWindow>
#include <QWheelEvent>
#include <algorithm>

namespace {

QString label(QQuickItem *item)
{
    for (const char *prop : {"displayText", "text", "title", "tip", "iconName", "name", "placeholderText"}) {
        const QVariant v = item->property(prop);
        if (v.isValid() && v.typeId() == QMetaType::QString && !v.toString().isEmpty())
            return v.toString();
    }
    return {};
}

bool effectivelyVisible(QQuickItem *item)
{
    for (QQuickItem *p = item; p; p = p->parentItem()) {
        if (!p->isVisible() || p->opacity() <= 0.01)
            return false;
    }
    return true;
}

// Inside something that scrolls horizontally (overflow there is intended).
bool inHorizontalScroller(QQuickItem *item)
{
    for (QQuickItem *p = item->parentItem(); p; p = p->parentItem()) {
        if (p->inherits("QQuickFlickable")) {
            const qreal cw = p->property("contentWidth").toReal();
            if (cw > p->width() + 1)
                return true;
        }
    }
    return false;
}

void collect(QQuickItem *item, QJsonArray &out, QRectF clip)
{
    if (!item->isVisible() || item->opacity() <= 0.01)
        return;
    const QString text = label(item);
    const char *cls = item->metaObject()->className();
    const bool interactive = item->acceptedMouseButtons() != Qt::NoButton || item->inherits("QQuickAbstractButton") || item->inherits("QQuickTextInput")
                             || item->inherits("QQuickSlider") || item->inherits("QQuickComboBox") || item->inherits("QQuickSwitch");
    const QRectF full = item->mapRectToScene(QRectF(0, 0, item->width(), item->height()));
    if (!text.isEmpty() || interactive) {
        // Only the part not clipped away by scroll views etc. is clickable.
        const QRectF r = full.intersected(clip);
        if (r.width() > 0 && r.height() > 0 && effectivelyVisible(item)) {
            QJsonObject o;
            o.insert(QStringLiteral("text"), text);
            o.insert(QStringLiteral("type"), QString::fromLatin1(cls));
            o.insert(QStringLiteral("x"), r.x());
            o.insert(QStringLiteral("y"), r.y());
            o.insert(QStringLiteral("w"), r.width());
            o.insert(QStringLiteral("h"), r.height());
            o.insert(QStringLiteral("enabled"), item->isEnabled());
            o.insert(QStringLiteral("button"), item->inherits("QQuickAbstractButton"));
            // Unclipped extent, to find content wider than the window.
            o.insert(QStringLiteral("right"), full.right());
            o.insert(QStringLiteral("hscroll"), inHorizontalScroller(item));
            const QVariant checked = item->property("checked");
            if (checked.isValid())
                o.insert(QStringLiteral("checked"), checked.toBool());
            if (item->hasActiveFocus() && item == item->window()->activeFocusItem())
                o.insert(QStringLiteral("focused"), true);
            if (item->property("activeFocusOnTab").toBool())
                o.insert(QStringLiteral("tabStop"), true);
            // Text cut off, and what a screen reader says.
            if (item->property("truncated").toBool())
                o.insert(QStringLiteral("truncated"), true);
            if (QAccessibleInterface *a = QAccessible::queryAccessibleInterface(item)) {
                o.insert(QStringLiteral("a11yName"), a->text(QAccessible::Name));
                o.insert(QStringLiteral("a11yRole"), int(a->role()));
            }
            out.append(o);
        }
    }
    if (item->clip())
        clip = clip.intersected(full);
    const auto children = item->childItems();
    for (QQuickItem *c : children)
        collect(c, out, clip);
}

} // namespace

void TestHooks::attach(QQuickWindow *window)
{
    const QString path = qEnvironmentVariable("BEATLUMEN_TEST_DUMP");
    if (!path.isEmpty())
        new TestHooks(window, path);
}

TestHooks::TestHooks(QQuickWindow *window, const QString &path)
    : QObject(window), m_window(window), m_path(path)
{
    connect(&m_timer, &QTimer::timeout, this, &TestHooks::dump);
    m_timer.start(500);
    connect(&m_commands, &QTimer::timeout, this, &TestHooks::poll);
    m_commands.start(50);
    connect(window, &QQuickWindow::frameSwapped, this, &TestHooks::frame);
    m_clock.start();
    m_statsClock.start();
}

void TestHooks::dump()
{
    QJsonArray items;
    collect(m_window->contentItem(), items, QRectF(0, 0, m_window->width(), m_window->height()));
    QFile f(m_path + QStringLiteral(".tmp"));
    if (f.open(QIODevice::WriteOnly)) {
        f.write(QJsonDocument(items).toJson(QJsonDocument::Compact));
        f.close();
        QFile::remove(m_path);
        f.rename(m_path);
    }
}

void TestHooks::frame()
{
    m_frames.append(m_clock.restart());
    if (m_statsClock.elapsed() >= 5000 && m_frames.size() > 2) {
        QVector<double> v = m_frames;
        std::sort(v.begin(), v.end());
        double sum = 0;
        for (double x : v)
            sum += x;
        const double mean = sum / v.size();
        const double p95 = v.at(int(v.size() * 0.95));
        qInfo("frames: %d fps %.1f mean %.1f ms p95 %.1f ms max %.1f ms", int(v.size()), 1000.0 / mean, mean, p95, v.last());
        m_frames.clear();
        m_statsClock.restart();
    }
}

void TestHooks::poll()
{
    QFile f(m_path + QStringLiteral(".cmd"));
    if (!f.open(QIODevice::ReadOnly))
        return;
    const QString text = QString::fromUtf8(f.readAll());
    f.close();
    for (const QString &line : text.split(QLatin1Char('\n'), Qt::SkipEmptyParts))
        run(line);
    dump();
    f.remove();
}

void TestHooks::run(const QString &line)
{
    const qsizetype sp = line.indexOf(QLatin1Char(' '));
    const QString cmd = line.left(sp);
    const QString rest = sp < 0 ? QString() : line.mid(sp + 1);
    const QStringList a = rest.split(QLatin1Char(' '), Qt::SkipEmptyParts);
    auto mouse = [&](QEvent::Type type, QPointF pos, Qt::MouseButton button, Qt::MouseButtons held) {
        QMouseEvent e(type, pos, m_window->mapToGlobal(pos), button, held, Qt::NoModifier);
        QCoreApplication::sendEvent(m_window, &e);
    };
    if (cmd == QLatin1String("shot") && !a.isEmpty()) {
        m_window->grabWindow().save(a.at(0));
    } else if (cmd == QLatin1String("hover") && a.size() >= 2) {
        mouse(QEvent::MouseMove, QPointF(a.at(0).toDouble(), a.at(1).toDouble()), Qt::NoButton, Qt::NoButton);
    } else if (cmd == QLatin1String("click") && a.size() >= 2) {
        const QPointF pos(a.at(0).toDouble(), a.at(1).toDouble());
        mouse(QEvent::MouseMove, pos, Qt::NoButton, Qt::NoButton);
        mouse(QEvent::MouseButtonPress, pos, Qt::LeftButton, Qt::LeftButton);
        mouse(QEvent::MouseButtonRelease, pos, Qt::LeftButton, Qt::NoButton);
    } else if (cmd == QLatin1String("scroll") && a.size() >= 3) {
        const QPointF pos(a.at(0).toDouble(), a.at(1).toDouble());
        QWheelEvent e(pos, m_window->mapToGlobal(pos), QPoint(), QPoint(0, a.at(2).toInt() * 120), Qt::NoButton,
                      Qt::NoModifier, Qt::NoScrollPhase, false);
        QCoreApplication::sendEvent(m_window, &e);
    } else if (cmd == QLatin1String("key") && !a.isEmpty()) {
        const QKeyCombination combo = QKeySequence(a.at(0))[0];
        QKeyEvent press(QEvent::KeyPress, combo.key(), combo.keyboardModifiers());
        QKeyEvent release(QEvent::KeyRelease, combo.key(), combo.keyboardModifiers());
        QCoreApplication::sendEvent(m_window, &press);
        QCoreApplication::sendEvent(m_window, &release);
    } else if (cmd == QLatin1String("text")) {
        for (const QChar c : rest) {
            QKeyEvent press(QEvent::KeyPress, 0, Qt::NoModifier, QString(c));
            QKeyEvent release(QEvent::KeyRelease, 0, Qt::NoModifier, QString(c));
            QCoreApplication::sendEvent(m_window, &press);
            QCoreApplication::sendEvent(m_window, &release);
        }
    } else if (cmd == QLatin1String("size") && a.size() >= 2) {
        m_window->resize(a.at(0).toInt(), a.at(1).toInt());
    } else if (cmd == QLatin1String("set") && a.size() >= 2) {
        const QJsonDocument d = QJsonDocument::fromJson("[" + a.at(1).toUtf8() + "]");
        m_window->setProperty(a.at(0).toUtf8().constData(), d.array().at(0).toVariant());
    } else if (cmd == QLatin1String("core") && !a.isEmpty()) {
        const qsizetype args = rest.indexOf(QLatin1Char(' '));
        const QVariantMap map = args < 0 ? QVariantMap()
            : QJsonDocument::fromJson(rest.mid(args + 1).toUtf8()).object().toVariantMap();
        if (QObject *core = qmlEngine(m_window)->singletonInstance<QObject *>("Beatlumen.Core", "Core")) {
            QJSValue result;
            QMetaObject::invokeMethod(core, "call", Q_RETURN_ARG(QJSValue, result), Q_ARG(QString, a.at(0)),
                                      Q_ARG(QVariantMap, map));
        }
    } else {
        qWarning("test command not understood: %s", qPrintable(line));
    }
}
