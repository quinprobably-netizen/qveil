// veil: a private, account-free YouTube app. This is the window; the Rust
// engine (veil-core) does all networking, behind a loopback-only server.

#include <QGuiApplication>
#include <QIcon>
#include <QNetworkProxy>
#include <QProcessEnvironment>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QUrl>

#include <clocale>
#include <cstdio>
#include <cstring>
#include <unistd.h>
#include <vector>

#include "backend.h"
#include "mpvitem.h"
#include "theme.h"
#include "translator.h"

int main(int argc, char *argv[]) {
    // `veil` and `veil open` open the window. Anything else is a command for
    // the engine (`veil feed --json`, `veil sub add @x`), so scripts and agents
    // can drive veil without a display.
    if (argc > 1 && std::strcmp(argv[1], "open") != 0) {
        const QByteArray core = Backend::corePath().toLocal8Bit();
        std::vector<char *> args{const_cast<char *>(core.constData())};
        for (int i = 1; i < argc; ++i)
            args.push_back(argv[i]);
        args.push_back(nullptr);
        execv(core.constData(), args.data());
        std::perror(core.constData());
        return 127;
    }

    // libmpv renders through OpenGL into the scene.
    QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);

    QGuiApplication app(argc, argv);
    app.setOrganizationName("veil");
    app.setApplicationName("veil");
    app.setApplicationVersion("0.2.0");

    // Matches the window to veil.desktop, so the compositor (Wayland app_id)
    // and the launcher pick up the installed icon.
    app.setDesktopFileName("veil");
    app.setWindowIcon(QIcon::fromTheme("veil"));

    // libmpv parses numbers with the C locale and refuses to start otherwise.
    std::setlocale(LC_NUMERIC, "C");

    // The engine gets the environment as it was, so a proxy set there still
    // applies to its Direct route. The window and mpv only ever talk to the
    // engine on 127.0.0.1, so no proxy may sit in between.
    const auto environment = QProcessEnvironment::systemEnvironment();
    for (const char *variable : {"http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"})
        qunsetenv(variable);
    QNetworkProxy::setApplicationProxy(QNetworkProxy::NoProxy);

    QQuickStyle::setStyle("Material");
    qmlRegisterType<MpvItem>("Veil", 1, 0, "MpvItem");

    // Installed before anything is translated, and every qsTr() binding is
    // re-evaluated when the Omarchy language changes.
    Translator translator;
    app.installTranslator(&translator);

    Theme theme;
    Backend backend(environment);

    QQmlApplicationEngine engine;
    QObject::connect(&translator, &Translator::changed, &engine, &QQmlEngine::retranslate);
    engine.rootContext()->setContextProperty("translator", &translator);
    engine.rootContext()->setContextProperty("theme", &theme);
    engine.rootContext()->setContextProperty("backend", &backend);
    engine.load(QUrl("qrc:/Main.qml"));
    if (engine.rootObjects().isEmpty())
        return 1;

    backend.start();
    return app.exec();
}
