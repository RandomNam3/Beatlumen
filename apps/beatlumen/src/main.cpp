// Beatlumen: music player with beat-synchronized visuals.
// Qt Quick user interface on top of the Rust core (crates/bl-core).

#include <QFile>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QIcon>
#include <QJsonDocument>
#include <QJsonObject>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QSurfaceFormat>

#include "bridge.h"
#include "tls.h"
#include "web.h"
#if !defined(Q_OS_ANDROID) && !defined(Q_OS_WASM)
#include "single_instance.h"
#endif

namespace {

QtMessageHandler previousHandler = nullptr;

// Removes login tokens from log messages (e.g. URLs in image load errors),
// which on Android end up in the system log. (No QRegularExpression: its
// JIT would allocate executable memory.)
void redactingHandler(QtMsgType type, const QMessageLogContext &ctx, const QString &msg)
{
    QString clean = msg;
    const QLatin1StringView key("token=");
    for (qsizetype i = clean.indexOf(key); i >= 0; i = clean.indexOf(key, i + 1)) {
        qsizetype end = i + key.size();
        while (end < clean.size() && clean[end] != u'&' && !clean[end].isSpace() && clean[end] != u'"' && clean[end] != u'\'')
            ++end;
        clean.replace(i + key.size(), end - i - key.size(), QStringLiteral("…"));
    }
    if (previousHandler)
        previousHandler(type, ctx, clean);
}

} // namespace

int main(int argc, char *argv[])
{
    previousHandler = qInstallMessageHandler(redactingHandler);
    // The core and the visualizer live on the GUI thread; render there too.
    qputenv("QSG_RENDER_LOOP", "basic");
#if defined(Q_OS_ANDROID)
    // QML is compiled ahead of time; the interpreter covers the rest. This
    // avoids JIT pages that hardened Android builds (GrapheneOS) report as
    // dynamic code loading. BeatlumenApplication.java sets the same before
    // Qt is loaded (Qt's Android plugin uses QRegularExpression earlier);
    // repeated here for launches without it.
    qputenv("QV4_FORCE_INTERPRETER", "1");
    // Same for the PCRE2 JIT behind QRegularExpression.
    qputenv("QT_ENABLE_REGEXP_JIT", "0");
#endif
#if defined(Q_OS_LINUX) && !defined(Q_OS_ANDROID)
    // Native file dialogs through the XDG desktop portal (KDE, GNOME, …).
    // The theme keeps the desktop's own theme underneath. The AppImage ships
    // no other theme plugin; its start hook asks for gtk2 on GNOME.
    {
        const QByteArray theme = qgetenv("QT_QPA_PLATFORMTHEME");
        if (theme.isEmpty() || (qEnvironmentVariableIsSet("APPIMAGE") && theme == "gtk2"))
            qputenv("QT_QPA_PLATFORMTHEME", "xdgdesktopportal");
    }
#endif
    QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);
    QSurfaceFormat fmt = QSurfaceFormat::defaultFormat();
#if defined(Q_OS_ANDROID) || defined(Q_OS_WASM) || defined(Q_OS_IOS)
    fmt.setRenderableType(QSurfaceFormat::OpenGLES);
    fmt.setVersion(3, 0);
#else
    fmt.setVersion(3, 3);
    fmt.setProfile(QSurfaceFormat::CoreProfile);
#endif
    // Qt Quick draws opaque items first and relies on the depth buffer to
    // keep translucent items behind them; without one (Windows picks a
    // depth-less pixel format for 0) panels cover opaque images.
    fmt.setDepthBufferSize(24);
    fmt.setStencilBufferSize(8);
    fmt.setSwapInterval(1);
    QSurfaceFormat::setDefaultFormat(fmt);

    QGuiApplication::setOrganizationName(QStringLiteral("Beatlumen"));
    QGuiApplication::setApplicationName(QStringLiteral("Beatlumen"));
#if !defined(Q_OS_ANDROID) && !defined(Q_OS_WASM)
    // "Size of everything" in the settings; Qt needs it before it starts.
    if (!qEnvironmentVariableIsSet("QT_SCALE_FACTOR")) {
        QFile settings(QStandardPaths::writableLocation(QStandardPaths::AppDataLocation) + QStringLiteral("/settings.json"));
        if (settings.open(QIODevice::ReadOnly)) {
            const double scale = QJsonDocument::fromJson(settings.readAll()).object()
                                     .value(QStringLiteral("ui")).toObject()
                                     .value(QStringLiteral("uiScale")).toDouble(1.0);
            if (scale > 1.0 && scale <= 2.0)
                qputenv("QT_SCALE_FACTOR", QByteArray::number(scale));
        }
    }
#endif
    QGuiApplication app(argc, argv);
    QGuiApplication::setApplicationVersion(QStringLiteral(BL_VERSION));
    QGuiApplication::setWindowIcon(QIcon(QStringLiteral(":/qt/qml/Beatlumen/assets/icon.png")));
#if !defined(Q_OS_ANDROID) && !defined(Q_OS_IOS) && !defined(Q_OS_WASM)
    // Mouse and keyboard: the compact controls. Fingers keep the large ones.
    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_MATERIAL_VARIANT"))
        qputenv("QT_QUICK_CONTROLS_MATERIAL_VARIANT", "Dense");
#endif
    QQuickStyle::setStyle(QStringLiteral("Material"));

    const int fontId = QFontDatabase::addApplicationFont(QStringLiteral(":/qt/qml/Beatlumen/assets/Inter-Variable.ttf"));
    if (fontId >= 0) {
        QFont f(QFontDatabase::applicationFontFamilies(fontId).value(0));
        f.setPointSizeF(QGuiApplication::font().pointSizeF());
        QGuiApplication::setFont(f);
    }

    QStringList args = QGuiApplication::arguments();
    args.removeFirst();
    args += web::pageArguments();
    for (const QString &a : args) {
        if (a == QLatin1String("-h") || a == QLatin1String("--help")) {
            qInfo("usage: beatlumen [--server URL] [--background PATH] [--preset NAME] [--fullscreen] [audio files...]");
            return 0;
        }
    }

#if !defined(Q_OS_ANDROID) && !defined(Q_OS_WASM)
    // One app per data folder: a second start hands its files over and ends.
    SingleInstance *instance = SingleInstance::claim(QStandardPaths::writableLocation(QStandardPaths::AppDataLocation), args, &app);
    if (!instance)
        return 0;
#endif

    // The browser build first loads its persistent storage (async).
    QQmlApplicationEngine *engine = nullptr;
    Bridge *bridge = nullptr;
    auto start = [&]() {
        bridge = new Bridge(args, &app);
        qmlRegisterSingletonInstance("Beatlumen.Core", 1, 0, "Core", bridge);
        engine = new QQmlApplicationEngine(&app);
        if (auto *f = tls::qmlNetworkFactory())
            engine->setNetworkAccessManagerFactory(f);
        tls::addImageProvider(engine);
        engine->rootContext()->setContextProperty(QStringLiteral("startFullscreen"), args.contains(QStringLiteral("--fullscreen")));
        QObject::connect(engine, &QQmlApplicationEngine::objectCreationFailed, &app, []() { QCoreApplication::exit(1); }, Qt::QueuedConnection);
        engine->loadFromModule("Beatlumen", "Main");
        if (!engine->rootObjects().isEmpty()) {
            if (auto *w = qobject_cast<QQuickWindow *>(engine->rootObjects().first())) {
                bridge->attachWindow(w);
#if !defined(Q_OS_ANDROID) && !defined(Q_OS_WASM)
                // A later start: play its files here and come to the front.
                QObject::connect(instance, &SingleInstance::received, bridge, [bridge, w](const QStringList &files) {
                    if (!files.isEmpty())
                        bridge->openPaths(files, true);
                    if (w->visibility() == QWindow::Minimized)
                        w->showNormal();
                    w->show();
                    w->raise();
                    w->requestActivate();
                });
#endif
            }
        }
    };
    web::initStorage(start);
    QObject::connect(&app, &QCoreApplication::aboutToQuit, [&]() {
        delete engine;
        engine = nullptr;
        delete bridge;
        bridge = nullptr;
    });
    return app.exec();
}
