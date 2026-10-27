// Beatlumen: music player with beat-synchronized visuals.
// Qt Quick user interface on top of the Rust core (crates/bl-core).

#include <QFontDatabase>
#include <QGuiApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QSurfaceFormat>

#include "bridge.h"

int main(int argc, char *argv[])
{
    // The core and the visualizer live on the GUI thread; render there too.
    qputenv("QSG_RENDER_LOOP", "basic");
    QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);
    QSurfaceFormat fmt = QSurfaceFormat::defaultFormat();
#if defined(Q_OS_ANDROID) || defined(Q_OS_WASM) || defined(Q_OS_IOS)
    fmt.setRenderableType(QSurfaceFormat::OpenGLES);
    fmt.setVersion(3, 0);
#else
    fmt.setVersion(3, 3);
    fmt.setProfile(QSurfaceFormat::CoreProfile);
#endif
    fmt.setDepthBufferSize(0);
    fmt.setStencilBufferSize(8);
    fmt.setSwapInterval(1);
    QSurfaceFormat::setDefaultFormat(fmt);

    QGuiApplication app(argc, argv);
    QGuiApplication::setOrganizationName(QStringLiteral("Beatlumen"));
    QGuiApplication::setApplicationName(QStringLiteral("Beatlumen"));
    QGuiApplication::setApplicationVersion(QStringLiteral(BL_VERSION));
    QGuiApplication::setWindowIcon(QIcon(QStringLiteral(":/qt/qml/Beatlumen/assets/icon.png")));
    QQuickStyle::setStyle(QStringLiteral("Material"));

    const int fontId = QFontDatabase::addApplicationFont(QStringLiteral(":/qt/qml/Beatlumen/assets/Inter-Variable.ttf"));
    if (fontId >= 0) {
        QFont f(QFontDatabase::applicationFontFamilies(fontId).value(0));
        f.setPointSizeF(QGuiApplication::font().pointSizeF());
        QGuiApplication::setFont(f);
    }

    QStringList args = QGuiApplication::arguments();
    args.removeFirst();
    for (const QString &a : args) {
        if (a == QLatin1String("-h") || a == QLatin1String("--help")) {
            qInfo("usage: beatlumen [--server URL] [--background PATH] [--preset NAME] [--fullscreen] [audio files...]");
            return 0;
        }
    }

    Bridge bridge(args);
    qmlRegisterSingletonInstance("Beatlumen.Core", 1, 0, "Core", &bridge);

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("startFullscreen"), args.contains(QStringLiteral("--fullscreen")));
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreationFailed, &app, []() { QCoreApplication::exit(1); }, Qt::QueuedConnection);
    engine.loadFromModule("Beatlumen", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;
    if (auto *w = qobject_cast<QQuickWindow *>(engine.rootObjects().first()))
        bridge.attachWindow(w);
    return app.exec();
}
