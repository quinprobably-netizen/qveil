#include <QOffscreenSurface>
#include <QOpenGLContext>
#include <QProcess>
#include <QQuickItem>
#include <QQuickWindow>
#include <QSignalSpy>
#include <QtTest>

#include <memory>

#include "mpvitem.h"
#include "suite.h"

// The player as it draws: MpvItems in OpenGL windows, so with render
// contexts made and freed on the render threads. The rest of the tests use
// the software renderer, which is all the offscreen platform is sure to
// have, and one process can't have both; so these run in a process of their
// own (QVEIL_TEST_OPENGL), started by PlayerTestsRun. Where no OpenGL
// context can be made they are skipped. Nothing is played: a player gets
// its render context whether or not it has a file.
class PlayerTests : public QObject {
    Q_OBJECT
    Q_CLASSINFO("alone", "yes")

    // A shown window with one player in it.
    struct Shown {
        std::unique_ptr<QQuickWindow> window = std::make_unique<QQuickWindow>();
        MpvItem *item = new MpvItem(window->contentItem());
        explicit Shown(bool releaseWhenHidden = false) {
            item->setReleaseWhenHidden(releaseWhenHidden);
            item->setSize(QSizeF(320, 180));
            window->resize(320, 180);
            window->show();
        }
        // The window the player's render context was made in, if it has one.
        QQuickWindow *drawnIn() const {
            const std::shared_ptr<MpvCore> core = item->core();
            if (!core || !core->ready)
                return nullptr;
            QMutexLocker lock(&core->mutex);
            return core->window;
        }
    };

private slots:
    void initTestCase() {
        QOpenGLContext context;
        QOffscreenSurface surface;
        surface.create();
        if (!context.create() || !context.makeCurrent(&surface))
            QSKIP("no OpenGL context can be made here");
        context.doneCurrent();
    }

    void theTestsPlayersPlayToNoSoundDevice() {
        // QVEIL_MPV_AO is set for them (see PlayerTestsRun).
        MpvItem item;
        char *ao = mpv_get_property_string(item.core()->handle, "ao");
        const QByteArray said = ao ? QByteArray(ao) : QByteArray();
        mpv_free(ao);
        QCOMPARE(said, QByteArray("null"));
    }

    void aHandoverMovesTheRenderContextBetweenShownWindows() {
        {
            Shown page, small(true);
            QVERIFY(QTest::qWaitForWindowExposed(page.window.get()));
            QVERIFY(QTest::qWaitForWindowExposed(small.window.get()));
            QTRY_COMPARE(small.drawnIn(), small.window.get());
            QTRY_COMPARE(page.drawnIn(), page.window.get());
            QCOMPARE(MpvCore::players(), 2);

            QSignalSpy failed(page.item, &MpvItem::takeOverFailed);
            const std::shared_ptr<MpvCore> moved = small.item->core();
            QVERIFY(page.item->takeOver(small.item));
            // Freed where it was made (as that window synchronizes) and made
            // again here, with nothing opened again.
            QTRY_COMPARE(page.drawnIn(), page.window.get());
            QCOMPARE(page.item->core(), moved);
            QTRY_VERIFY(!small.item->handingOver());
            QCOMPARE(failed.size(), 0);
            // The page's own player was let go of.
            QTRY_COMPARE(MpvCore::players(), 1);
        }
        QTRY_COMPARE(MpvCore::players(), 0);
    }

    void aSmallWindowHiddenMidHandoverLetsGoOfThePlayer() {
        {
            Shown page, small(true);
            QVERIFY(QTest::qWaitForWindowExposed(page.window.get()));
            QVERIFY(QTest::qWaitForWindowExposed(small.window.get()));
            QTRY_COMPARE(small.drawnIn(), small.window.get());
            QTRY_COMPARE(page.drawnIn(), page.window.get());
            QCOMPARE(MpvCore::players(), 2);

            QSignalSpy failed(page.item, &MpvItem::takeOverFailed);
            QVERIFY(page.item->takeOver(small.item));
            // The small window goes before it could free the render context
            // itself. Hidden, it draws nothing; what it drew with must still
            // be let go of, and the handover finish, not hang on for as long
            // as the window stays hidden.
            small.window->hide();
            QTRY_COMPARE(page.drawnIn(), page.window.get());
            QCOMPARE(failed.size(), 0);
            QTRY_COMPARE_WITH_TIMEOUT(MpvCore::players(), 1, 6000);
            // Past the handover's own timeout, nothing more happens.
            QTest::qWait(3000);
            QCOMPARE(failed.size(), 0);
            QCOMPARE(MpvCore::players(), 1);
        }
        QTRY_COMPARE(MpvCore::players(), 0);
    }

    void aHandoverOutOfAHiddenWindowLeavesNoPlayerBehind() {
        // The window that drew it is hidden before the handover is asked
        // for, and its player is taken over with nothing drawing it.
        {
            Shown page, small(true);
            QVERIFY(QTest::qWaitForWindowExposed(page.window.get()));
            QVERIFY(QTest::qWaitForWindowExposed(small.window.get()));
            QTRY_COMPARE(small.drawnIn(), small.window.get());
            small.window->hide();
            QTRY_VERIFY(!small.item->core()->ready);

            QSignalSpy failed(page.item, &MpvItem::takeOverFailed);
            QVERIFY(page.item->takeOver(small.item));
            QTRY_COMPARE(page.drawnIn(), page.window.get());
            QTRY_COMPARE(MpvCore::players(), 1);
            QCOMPARE(failed.size(), 0);
        }
        QTRY_COMPARE(MpvCore::players(), 0);
    }
};

// Runs PlayerTests in a process of their own, with OpenGL.
class PlayerTestsRun : public QObject {
    Q_OBJECT

private slots:
    void thePlayerDrawsAndHandsOverWithOpenGL() {
        QProcess child;
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_OPENGL", "1");
        environment.insert("QVEIL_TEST_SUITES", "PlayerTests");
        environment.insert("QT_QPA_PLATFORM", "offscreen");
        // The loop that renders on a thread of its own, as on the desktop.
        environment.insert("QSG_RENDER_LOOP", "threaded");
        environment.insert("QVEIL_MPV_AO", "null");
        child.setProcessEnvironment(environment);
        child.setProcessChannelMode(QProcess::MergedChannels);
        child.start(QCoreApplication::applicationFilePath(), {});
        QVERIFY(child.waitForStarted());
        QVERIFY2(child.waitForFinished(120000), "the player tests didn't finish");
        const QString output = QString::fromUtf8(child.readAll());
        if (output.contains("SKIP   : PlayerTests::initTestCase()"))
            QSKIP("no OpenGL context can be made here");
        QVERIFY2(child.exitStatus() == QProcess::NormalExit && child.exitCode() == 0, qPrintable(output));
        QVERIFY2(output.contains("Totals: 6 passed, 0 failed"), qPrintable(output));
    }
};

QVEIL_SUITE(PlayerTests)
QVEIL_SUITE(PlayerTestsRun)
#include "player_tests.moc"
