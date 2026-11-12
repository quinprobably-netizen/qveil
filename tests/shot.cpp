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
        {"settings-home", "go('settings'); settingsPage.category = 'home'"},
        {"search", "go('search'); backend.search('talks over 30 min')"},
        {"watch", "playListed(qsTr('Latest from your channels'), backend.feed.get(0), backend.feed, 0)"},
        {"feed", "feedPage.showAll()"},
        // The video, moved to its small window once it plays there.
        {"pip", "playListed(qsTr('Latest from your channels'), backend.feed.get(0), backend.feed, 0); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 2500; running: true; onTriggered: win.togglePip() }', win)"},
        {"help", "help.open()"},
        {"route", "route.open()"},
        {"home-tor", "backend.setConfig({ route: 'tor' })"},
        // Home with the live line (bin/shot sets QVEIL_STANDIN_LIVE for it):
        // streams running and announced, among the shelves.
        {"home-live", ""},
        // A channel's Streams tab: running, announced, then past broadcasts.
        {"channel-streams", "openChannel('UCaaaaaaaaaaaaaaaaaaaaaa'); backend.openChannel('UCaaaaaaaaaaaaaaaaaaaaaa', 'streams')"},
        {"theatre", "playListed(qsTr('Latest from your channels'), backend.feed.get(0), backend.feed, 0); watch.theatre = true"},
        {"history-on", "backend.setConfig({ history: true }); backend.loadHistory(); go('history')"},
        {"lens-edit", "go('lenses'); lensesPage.editLens()"},
        {"sort", "feedPage.openSort()"},
        // A clip of a minute, played to its last moments.
        {"watch-ended", "playListed(qsTr('Latest from your channels'), backend.feed.get(0), backend.feed, 0); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 2500; running: true; onTriggered: watch.surface.mpv.seek(59.8, false) }', win)"},
        {"watch-comments", "play(backend.feed.get(0)); watch.enterSheet('comments')"},
        // A running stream (bin/shot makes its segments), at the edge, and
        // fallen behind it; the controls brought up just before the still.
        {"watch-live", "play({ id: 'live0000001', title: 'Live: the long one, part 2', channel: 'Low Level Hours', live: 'now' }); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 4600; running: true; onTriggered: watch.surface.reveal() }', win)"},
        // A running stream opens on its live chat (T1).
        {"watch-chat", "play({ id: 'live0000001', title: 'Live: the long one, part 2', channel: 'Low Level Hours', live: 'now' })"},
        // An announced stream's waiting room, counting down.
        {"waiting", "play({ id: 'waiting0001', title: 'Questions, answered live', channel: 'Low Level Hours', live: 'upcoming' })"},
        // Its Up next: the channel's other running and announced streams.
        {"waiting-next", "play({ id: 'waiting0001', title: 'Questions, answered live', channel: 'Low Level Hours', live: 'upcoming' }); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 1500; running: true; onTriggered: watch.sheet.current = \\'next\\' }', win)"},
        {"watch-live-behind", "play({ id: 'live0000001', title: 'Live: the long one, part 2', channel: 'Low Level Hours', live: 'now' }); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 3000; running: true; onTriggered: watch.surface.seekTo(4) }', win); "
                "Qt.createQmlObject('import QtQuick; Timer { interval: 4600; running: true; onTriggered: watch.surface.reveal() }', win)"},
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
    // A size that was asked for is the window's, whatever the compositor
    // would make of it. Cage maximizes a window, or one larger than its
    // screen, but leaves alone one that bypasses the window manager (under
    // X11, as bin/shot runs it): this one is as large as it is told to be,
    // even larger than the headless screen (a still is taken from the scene,
    // not from the screen).
    if (argc > 4) {
        window->hide();
        window->setFlags(window->flags() | Qt::X11BypassWindowManagerHint);
        window->setMinimumSize(size);
        window->setMaximumSize(size);
        window->resize(size);
        window->show();
    } else {
        window->resize(size);
    }
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
        QTimer::singleShot(view == "pip" ? 8000 : view == "watch-ended" ? 9500 : view == "home-tor" ? 6000 : view.startsWith("watch") ? 5000 : 2500, &app, [&] {
            // The small window is a window of its own.
            QQuickWindow *shown = view == "pip" ? window->property("pipWindow").value<QQuickWindow *>() : window;
            const bool saved = shown && shown->grabWindow().save(out);
            std::fprintf(stderr, saved ? "saved %s\n" : "couldn't save %s\n", qPrintable(out));
            QCoreApplication::exit(saved ? 0 : 1);
        });
    });
    return app.exec();
}
