// qveil: a private, account-free YouTube app. This is the window; the Rust
// engine (qveil-core) does all networking, behind a loopback-only server.

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
#include "datadirs.h"
#include "mpvitem.h"
#include "theme.h"
#include "translator.h"

// qveil.pro reads it from core/Cargo.toml, so the engine and the window can't
// disagree about the version.
#ifndef QVEIL_VERSION
#error "QVEIL_VERSION is defined by qveil.pro"
#endif

int main(int argc, char *argv[]) {
    // `qveil` and `qveil open` open the window. Anything else is a command for
    // the engine (`qveil feed --json`, `qveil sub add @x`), so scripts and agents
    // can drive qveil without a display.
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

    // The app was called veil before it was qveil, and its directories must
    // move before Qt exists: Qt creates ~/.cache/qveil for its QML cache on its
    // own, and would leave the old cache stranded next to it. The engine starts
    // later and finds nothing left to move.
    DataDirs::prepare();

    // Qt quotes the URL of a failed request in its warnings, and the engine's
    // token is in every URL the window uses.
    Backend::scrubLogs();

    // libmpv renders through OpenGL into the scene.
    QQuickWindow::setGraphicsApi(QSGRendererInterface::OpenGL);

    QGuiApplication app(argc, argv);
    app.setOrganizationName("qveil");
    app.setApplicationName("qveil");
    app.setApplicationVersion(QStringLiteral(QVEIL_VERSION));

    // Matches the window to qveil.desktop, so the compositor (Wayland app_id)
    // and the launcher pick up the installed icon.
    app.setDesktopFileName("qveil");
    app.setWindowIcon(QIcon::fromTheme("qveil"));

    // libmpv parses numbers with the C locale and refuses to start otherwise.
    std::setlocale(LC_NUMERIC, "C");

    // The engine gets the environment as it was, but its Direct route ignores
    // proxy variables, so what the environment says can't sit between it and
    // YouTube. The window and mpv only ever talk to the engine on 127.0.0.1,
    // so no proxy may sit in between there either.
    const auto environment = QProcessEnvironment::systemEnvironment();
    for (const char *variable : {"http_proxy", "HTTP_PROXY", "https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"})
        qunsetenv(variable);
    QNetworkProxy::setApplicationProxy(QNetworkProxy::NoProxy);

    QQuickStyle::setStyle("Material");
    qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem");

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
    const int code = app.exec();
    // Qt has written its settings by now, whatever mode it gave the file.
    DataDirs::tightenSettings();
    return code;
}
