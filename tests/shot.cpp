// Takes a still of the window: the app as it is built, against the stand-in
// engine (tests/shots/engine.py), put into one view and saved as a PNG.
//
//   qveil_shot <view> <out.png> [width height]
//
// It is run inside a headless compositor by bin/shot, so it draws on the GPU
// (blur and video included) without touching the desktop. The theme is the
// user's own, so a still looks like the app will on this machine.
#include <QDir>
#include <QGuiApplication>
#include <QHash>
#include <QProcessEnvironment>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQmlExpression>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QSettings>
#include <QTemporaryDir>
#include <QTimer>

#include <clocale>
#include <cstdio>

#include "backend.h"
#include "mpvitem.h"
#include "theme.h"
#include "translator.h"

int main(int argc, char *argv[]) {
    if (argc < 3) {
        std::fprintf(stderr, "usage: qveil_shot <view> <out.png> [width height]\n");
        return 2;
    }
    const QString view = argv[1], out = argv[2];
    const QSize size(argc > 4 ? QString(argv[3]).toInt() : 1280, argc > 4 ? QString(argv[4]).toInt() : 720);

    // What each view does to get there, as the window's own functions.
    const QHash<QString, QString> views{
        {"home", ""},
        {"narrow", ""},
        {"lenses", "go('lenses')"},
        {"channels", "go('channels')"},
        {"history", "go('history')"},
        {"settings", "go('settings')"},
        {"search", "go('search'); backend.search('talks over 30 min')"},
        {"watch", "play(backend.feed.get(0))"},
        {"watch-comments", "play(backend.feed.get(0)); watch.sheet.current = 'comments'"},
    };
    if (!views.contains(view)) {
        std::fprintf(stderr, "no view %s; there are: %s\n", argv[1], qPrintable(QStringList(views.keys()).join(", ")));
        return 2;
    }

    QQuickStyle::setStyle("Material");
    QGuiApplication app(argc, argv);
    std::setlocale(LC_NUMERIC, "C");
    // The window's settings are not the user's.
    QTemporaryDir settings;
    QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, settings.path());
    app.setOrganizationName("qveil-shots");
    qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem");
    qmlRegisterSingletonType(QUrl("qrc:/Style.qml"), "Qveil", 1, 0, "Style");

    Translator translator;
    Theme theme;
    Backend backend(QProcessEnvironment::systemEnvironment(), QStringLiteral(SHOTS_DIR "/engine.py"));
    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty("translator", &translator);
    engine.rootContext()->setContextProperty("theme", &theme);
    engine.rootContext()->setContextProperty("backend", &backend);
    engine.load(QUrl("qrc:/Main.qml"));
    if (engine.rootObjects().isEmpty())
        return 1;
    auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (!window)
        return 1;
    window->resize(size);
    backend.start();

    // Once the engine is there and the first page has loaded: go to the
    // view, give it time to load and settle, and take the picture.
    QTimer::singleShot(2500, &app, [&] {
        if (!views.value(view).isEmpty()) {
            // In the window's own context, so that its parts can be named.
            QQmlExpression go(qmlContext(window), window, views.value(view));
            go.evaluate();
            if (go.hasError())
                std::fprintf(stderr, "%s\n", qPrintable(go.error().toString()));
        }
        QTimer::singleShot(view.startsWith("watch") || view == "pip" ? 5000 : 2500, &app, [&] {
            const bool saved = window->grabWindow().save(out);
            std::fprintf(stderr, saved ? "saved %s\n" : "couldn't save %s\n", qPrintable(out));
            QCoreApplication::exit(saved ? 0 : 1);
        });
    });
    return app.exec();
}
