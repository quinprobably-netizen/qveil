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
    // opens no audio device (see MpvCore in mpvitem.cpp).
    qputenv("QVEIL_MPV_AO", "null");
    // The tests that put a scene on screen do so on the offscreen platform,
    // where only the software renderer is sure to exist. The few that draw
    // video need OpenGL, and run in a process of their own with
    // QVEIL_TEST_OPENGL set (see player_tests.cpp).
    const bool opengl = qEnvironmentVariableIsSet("QVEIL_TEST_OPENGL");
    // The offscreen platform falls back to the software scene graph unless
    // the hardware one is asked for by name.
    if (opengl)
        qputenv("QT_QUICK_BACKEND", "rhi");
    QQuickWindow::setGraphicsApi(opengl ? QSGRendererInterface::OpenGL : QSGRendererInterface::Software);
    QQuickStyle::setStyle("Basic");
    QGuiApplication app(argc, argv);
    // As the app registers it: the parts and pages import it from Qveil.
    qmlRegisterSingletonType(QUrl("qrc:/Style.qml"), "Qveil", 1, 0, "Style");
    qmlRegisterType<VideoModel>("Qveil", 1, 0, "VideoModel");
    // libmpv refuses to start unless numbers are parsed with the C locale.
    std::setlocale(LC_NUMERIC, "C");

    // QVEIL_TEST_SUITES=A,B runs only those suites, by class name. A suite
    // that says Q_CLASSINFO("alone", "yes") runs only when it is named so.
    const QStringList only = qEnvironmentVariable("QVEIL_TEST_SUITES").split(',', Qt::SkipEmptyParts);
    int failed = 0;
    for (QObject *test : Suite::all()) {
        const QMetaObject *meta = test->metaObject();
        const bool alone = meta->indexOfClassInfo("alone") >= 0;
        if (only.isEmpty() ? alone : !only.contains(QString::fromLatin1(meta->className())))
            continue;
        if (QTest::qExec(test, argc, argv) != 0)
            ++failed;
    }
    return failed;
}
