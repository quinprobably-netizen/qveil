#include "videomodel.h"
#include <QGuiApplication>
#include <QQmlEngine>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QTest>

#include <clocale>

#include "suite.h"

int main(int argc, char *argv[]) {
    // The desktop's platform theme is of no use here, and the GTK one gives up
    // when there is no display.
    qputenv("QT_QPA_PLATFORMTHEME", "generic");
    // A video played by a test is decoded, and heard by nobody: the player
    // opens no audio device (where it has the setting for it; see MpvItem).
    qputenv("QVEIL_MPV_AO", "null");
    // The tests that put a scene on screen do so on the offscreen platform,
    // where only the software renderer is sure to exist. (No test draws video.)
    QQuickWindow::setGraphicsApi(QSGRendererInterface::Software);
    QQuickStyle::setStyle("Basic");
    QGuiApplication app(argc, argv);
    // As the app registers it: the parts and pages import it from Qveil.
    qmlRegisterSingletonType(QUrl("qrc:/Style.qml"), "Qveil", 1, 0, "Style");
    qmlRegisterType<VideoModel>("Qveil", 1, 0, "VideoModel");
    // libmpv refuses to start unless numbers are parsed with the C locale.
    std::setlocale(LC_NUMERIC, "C");

    int failed = 0;
    for (QObject *test : Suite::all())
        if (QTest::qExec(test, argc, argv) != 0)
            ++failed;
    return failed;
}
