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
#include "videomodel.h"
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
        {"settings-about", "go('settings'); settingsPage.category = 'about'"},
        {"search", "go('search'); backend.search('talks over 30 min')"},
        {"watch", "play(backend.feed.get(0), backend.feed, 0)"},
        {"feed", "feedPage.showAll('latest')"},
        // The video, moved to its small window once it plays there.
        {"pip", "play(backend.feed.get(0), backend.feed, 0); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 2500; running: true; onTriggered: win.togglePip() }', win)"},
        {"help", "help.open()"},
        {"route", "route.open()"},
        {"theatre", "play(backend.feed.get(0), backend.feed, 0); watch.theatre = true"},
        {"history-on", "backend.setConfig({ history: true }); backend.loadHistory(); go('history')"},
        {"lens-edit", "go('lenses'); lensesPage.editLens()"},
        {"sort", "feedPage.openSort()"},
        {"watch-comments", "play(backend.feed.get(0)); watch.enterSheet('comments')"},
    };
    if (!views.contains(view)) {
        std::fprintf(stderr, "no view %s; there are: %s\n", argv[1], qPrintable(QStringList(views.keys()).join(", ")));
        return 2;
    }

    QQuickStyle::setStyle("Basic");
    QGuiApplication app(argc, argv);
    std::setlocale(LC_NUMERIC, "C");
    // The window's settings are not the user's.
    QTemporaryDir settings;
    QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, settings.path());
    app.setOrganizationName("qveil-shots");
    qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem");
    qmlRegisterType<VideoModel>("Qveil", 1, 0, "VideoModel");
    qmlRegisterSingletonType(QUrl("qrc:/Style.qml"), "Qveil", 1, 0, "Style");

    // Installed as the app installs it, so that counted strings read as they will.
    Translator translator;
    app.installTranslator(&translator);
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
        QTimer::singleShot(view == "pip" ? 8000 : view.startsWith("watch") ? 5000 : 2500, &app, [&] {
            // The small window is a window of its own.
            QQuickWindow *shown = view == "pip" ? window->property("pipWindow").value<QQuickWindow *>() : window;
            const bool saved = shown && shown->grabWindow().save(out);
            std::fprintf(stderr, saved ? "saved %s\n" : "couldn't save %s\n", qPrintable(out));
            QCoreApplication::exit(saved ? 0 : 1);
        });
    });
    return app.exec();
}
