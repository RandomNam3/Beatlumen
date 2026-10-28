#include "testhooks.h"

#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QQuickItem>
#include <QQuickWindow>
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
