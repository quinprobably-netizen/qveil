#include <QClipboard>
#include <QGuiApplication>
#include <QNetworkAccessManager>
#include <QNetworkProxy>
#include <QNetworkReply>
#include <QProcessEnvironment>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickItem>
#include <QQuickWindow>
#include <QSettings>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QtTest>

#include <memory>

#include "backend.h"
#include "mpvitem.h"
#include "suite.h"
#include "theme.h"
#include "translator.h"

namespace {

// The whole window, as the app loads it, on the offscreen platform and
// against the stand-in engine (tests/shots/engine.py), which answers from
// memory and keeps a list of what it was asked.
struct App {
    QTemporaryDir settings, themeDir;
    std::unique_ptr<Theme> theme;
    Translator translator;
    std::unique_ptr<Backend> backend;
    std::unique_ptr<QQmlApplicationEngine> qml;
    QQuickWindow *window = nullptr;
    QNetworkAccessManager web;
    bool ok = false;

    // `empty`: an engine with no lenses and nothing to resume.
    explicit App(bool empty = false, QSize size = QSize(1280, 720))
    {
        // The window's settings (Shorts, volume) are not the user's.
        QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, settings.path());
        QCoreApplication::setOrganizationName("qveil-tests");
        web.setProxy(QNetworkProxy::NoProxy);

        QDir().mkpath(themeDir.filePath("theme"));
        QFile colours(themeDir.filePath("theme/colors.toml"));
        if (colours.open(QIODevice::WriteOnly))
            colours.write("accent = \"#a08dff\"\nbackground = \"#151326\"\nforeground = \"#dad6ec\"\n");
        colours.close();
        theme = std::make_unique<Theme>(themeDir.path());

        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        if (empty)
            environment.insert("QVEIL_STANDIN_EMPTY", "1");
        environment.remove("QVEIL_STANDIN_CLIP");
        backend = std::make_unique<Backend>(environment, QFINDTESTDATA("shots/engine.py"));

        qml = std::make_unique<QQmlApplicationEngine>();
        qml->rootContext()->setContextProperty("translator", &translator);
        qml->rootContext()->setContextProperty("theme", theme.get());
        qml->rootContext()->setContextProperty("backend", backend.get());
        qml->load(QUrl("qrc:/Main.qml"));
        if (qml->rootObjects().isEmpty())
            return;
        window = qobject_cast<QQuickWindow *>(qml->rootObjects().first());
        if (!window)
            return;
        window->resize(size);
        window->requestActivate();
        backend->start();
        ok = QTest::qWaitFor([this] { return backend->ready() && !backend->config().isEmpty(); }, 10000)
             && QTest::qWaitForWindowExposed(window);
    }

    ~App()
    {
        // The window goes before what it is bound to.
        qml.reset();
    }

    // The item called `name`, wherever it is in the scene. (What a Repeater
    // or a view makes is a child item of its parent, not its QObject child.)
    static QQuickItem *under(QQuickItem *root, const QString &name)
    {
        if (root->objectName() == name)
            return root;
        for (QQuickItem *child : root->childItems())
            if (QQuickItem *found = under(child, name))
                return found;
        return nullptr;
    }
    QQuickItem *item(const char *name) const { return window ? under(window->contentItem(), name) : nullptr; }
    // Every item called `name` that is showing, in the order the scene has them.
    static void collect(QQuickItem *root, const QString &name, QList<QQuickItem *> &found)
    {
        if (root->objectName() == name && root->isVisible())
            found << root;
        for (QQuickItem *child : root->childItems())
            collect(child, name, found);
    }
    QList<QQuickItem *> items(const char *name) const
    {
        QList<QQuickItem *> found;
        if (window)
            collect(window->contentItem(), name, found);
        return found;
    }
    QObject *object(const char *name) const { return window ? window->findChild<QObject *>(name) : nullptr; }

    void click(QQuickItem *target) const
    {
        // What has only just appeared or moved is laid out with the next
        // frame; a click sent before that would go where the item was.
        QTest::qWait(40);
        QTest::mouseClick(window, Qt::LeftButton, {}, target->mapToScene(QPointF(target->width() / 2, target->height() / 2)).toPoint());
    }
    void hover(QQuickItem *target) const
    {
        QTest::mouseMove(window, target->mapToScene(QPointF(target->width() / 2, target->height() / 2)).toPoint());
    }
    void type(const char *text) const
    {
        for (const char *c = text; *c; ++c)
            QTest::keyClick(window, *c);
    }
    QString page() const { return window->property("page").toString(); }

    // The small window of picture in picture, and something in it.
    QQuickWindow *pip() const { return window->property("pipWindow").value<QQuickWindow *>(); }
    QQuickItem *inPip(const char *name) const { return pip() ? under(pip()->contentItem(), name) : nullptr; }
    // Makes the engine exit, as if it had crashed.
    void killEngine()
    {
        const QString base = backend->thumbBase().chopped(QString("/thumb/").size());
        QNetworkReply *reply = web.post(QNetworkRequest(QUrl(base + "/api/_die")), QByteArray());
        QObject::connect(reply, &QNetworkReply::finished, reply, &QObject::deleteLater);
    }

    // Opens `video` the way a card does, from no list.
    void play(const QVariantMap &video) const
    {
        QMetaObject::invokeMethod(window, "play", Q_ARG(QVariant, video), Q_ARG(QVariant, QVariant()), Q_ARG(QVariant, -1));
    }
    // What every piece of text that is showing under `root` says.
    static void texts(QQuickItem *root, QStringList &found)
    {
        if (!root->isVisible())
            return;
        if (root->property("text").isValid() && root->property("font").isValid())
            found << root->property("text").toString();
        for (QQuickItem *child : root->childItems())
            texts(child, found);
    }
    static int count(QQuickItem *root, const QString &name)
    {
        QList<QQuickItem *> found;
        collect(root, name, found);
        return found.size();
    }

    // What the engine was asked so far: "GET api/feed?stream=1", …
    QStringList asked()
    {
        const QString base = backend->thumbBase().chopped(QString("/thumb/").size());
        QNetworkReply *reply = web.get(QNetworkRequest(QUrl(base + "/api/_log")));
        QSignalSpy done(reply, &QNetworkReply::finished);
        done.wait(5000);
        QStringList lines;
        for (const QJsonValue &line : QJsonDocument::fromJson(reply->readAll()).array())
            lines << line.toString();
        reply->deleteLater();
        return lines;
    }
    int asked(const QString &start) { return asked().filter(QRegularExpression("^" + QRegularExpression::escape(start))).size(); }
};

#define READY(app) QVERIFY2((app).ok, "the window didn't load, or the stand-in engine didn't start")

} // namespace

class WindowTests : public QObject {
    Q_OBJECT

private slots:
    void initTestCase() { qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem"); }

    void init()
    {
        // No test may leave a QML warning behind. Where the engine is killed
        // on purpose, pictures that were loading from it say so, which is not
        // a fault; a script error still is.
        const bool engineDies = QByteArray(QTest::currentTestFunction()) == "theSmallWindowGoesWhenTheEngineDoes";
        QTest::failOnWarning(QRegularExpression(engineDies ? "TypeError|ReferenceError|Binding loop"
                                                           : "qrc:/|\\.qml|QML|TypeError|ReferenceError"));
    }

    void theWindowLoadsCleanWithARailAndNoTabBar()
    {
        App app;
        READY(app);
        for (const char *name : {"rail-home", "rail-lenses", "rail-channels", "rail-history", "rail-route", "rail-settings", "search"})
            QVERIFY2(app.item(name), name);
        QVERIFY(!app.object("topBar"));
        QCOMPARE(app.page(), QString("home"));
        QVERIFY(app.item("rail-home")->property("current").toBool());

        app.click(app.item("rail-channels"));
        QCOMPARE(app.page(), QString("channels"));
        QVERIFY(app.item("rail-channels")->property("current").toBool());
        QVERIFY(!app.item("rail-home")->property("current").toBool());
        app.click(app.item("rail-history"));
        QCOMPARE(app.page(), QString("history"));

        // What the route indicator says when pointed at is the engine's sentence.
        QCOMPARE(app.item("rail-route")->property("tip").toString(), app.backend->privacy().value("summary").toString());
        QVERIFY(!app.item("rail-route")->property("tip").toString().isEmpty());
    }

    void theRouteIndicatorSaysWhereYouStand()
    {
        App app;
        READY(app);
        QQuickItem *route = app.item("rail-route");
        QVERIFY(route);
        QCOMPARE(route->property("label").toString(), QString("Direct"));
        QCOMPARE(route->property("tone").value<QColor>(), QColor("#e0a93b"));
        app.backend->setConfig({{"route", "tor"}});
        QTRY_COMPARE(route->property("label").toString(), QString("Tor"));
        QCOMPARE(route->property("tone").value<QColor>(), QColor(app.theme->protectedColor()));
    }

    void typingNeverTriggersAShortcut()
    {
        App app;
        READY(app);
        QSignalSpy quit(app.qml.get(), &QQmlApplicationEngine::quit);
        QQuickItem *search = app.item("search");
        QVERIFY(search);
        QMetaObject::invokeMethod(search, "edit");
        QTRY_VERIFY(app.window->property("typing").toBool());
        app.type("itc[q4 r");
        QCOMPARE(search->property("text").toString(), QString("itc[q4 r"));
        QCOMPARE(app.page(), QString("home"));
        QCOMPARE(quit.count(), 0);
        QVERIFY(!app.window->property("watching").toBool());

        // Escape empties the field, then lets go of the keyboard; after that keys are keys again.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QCOMPARE(search->property("text").toString(), QString());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.window->property("typing").toBool());
        QTest::keyClick(app.window, Qt::Key_2);
        QCOMPARE(app.page(), QString("lenses"));
        QTest::keyClick(app.window, Qt::Key_1);
        QCOMPARE(app.page(), QString("home"));
        // The slash goes to the search field from anywhere.
        QTest::keyClick(app.window, Qt::Key_Slash);
        QTRY_VERIFY(search->property("editing").toBool());
        QCOMPARE(search->property("text").toString(), QString());
    }

    void theSearchFieldShowsHowItWasReadBeforeEnter()
    {
        App app;
        READY(app);
        QQuickItem *search = app.item("search");
        QQuickItem *tags = app.item("reading-tags");
        QVERIFY(search && tags);
        QVERIFY(!tags->isVisible());
        QMetaObject::invokeMethod(search, "edit");
        app.type("talks over 30 min");
        QTRY_VERIFY(tags->isVisible());
        QCOMPARE(tags->property("count").toInt(), 2);
        // Nothing has been searched yet: reading asks YouTube for nothing.
        QCOMPARE(app.asked("GET api/search"), 0);

        // A plain search has nothing to show.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!tags->isVisible());
        app.type("rust");
        QTest::qWait(400);
        QVERIFY(!tags->isVisible());

        QTest::keyClick(app.window, Qt::Key_Return);
        QCOMPARE(app.page(), QString("search"));
        QTRY_COMPARE(app.asked("GET api/search"), 1);
        QCOMPARE(app.backend->query(), QString("rust"));
    }

    void unfinishedVideosSitOnTopAndVanishWhenThereAreNone()
    {
        App app;
        READY(app);
        QQuickItem *row = app.item("resume-row");
        QVERIFY(row);
        QTRY_VERIFY(row->isVisible());
        QTRY_COMPARE(app.items("resume-entry").size(), 3);
        QQuickItem *first = app.items("resume-entry").first();
        QCOMPARE(first->property("title").toString(), QString("Why the borrow checker says no"));
        QCOMPARE(first->property("remaining").toString(), QString("26:58 left"));
        // How long they are kept is the setting's number.
        QVERIFY(row->property("note").toString().contains("7 days"));

        app.backend->forgetResume("vid00000001");
        QTRY_COMPARE(app.items("resume-entry").size(), 2);
        app.backend->clearResume();
        QTRY_VERIFY(!row->isVisible());

        App none(true);
        READY(none);
        QTest::qWait(300);
        QVERIFY(!none.item("resume-row")->isVisible());
    }

    void aLensTabShowsThatLensInTheGrid()
    {
        App app;
        READY(app);
        QQuickItem *tabs = app.item("feed-tabs");
        QQuickItem *grid = app.item("home-grid");
        QVERIFY(tabs && grid);
        QTRY_VERIFY(app.item("feed-tab-aaaa0002"));
        QVERIFY(app.item("feed-tab-latest") && app.item("feed-tab-aaaa0001") && app.item("feed-tab-new"));
        QVERIFY(app.item("feed-tab-latest")->property("current").toBool());
        QTRY_COMPARE(grid->property("count").toInt(), 12);
        QVERIFY(app.items("card-why").isEmpty());

        app.click(app.item("feed-tab-aaaa0002"));
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0002"));
        QVERIFY(app.item("feed-tab-aaaa0002")->property("current").toBool());
        QTRY_COMPARE(grid->property("count").toInt(), 6);
        // Each of a lens's videos says why it is there.
        QTRY_VERIFY(!app.items("card-why").isEmpty());
        QVERIFY(app.items("card-why").first()->property("text").toString().contains("over 30:00"));
        // It was fetched two hours ago: opening it asks for nothing new.
        QCOMPARE(app.asked("GET api/lens?id=aaaa0002&stream=1&refresh=1"), 1);

        app.click(app.item("feed-tab-latest"));
        QTRY_COMPARE(grid->property("count").toInt(), 12);
    }

    void aCardClearsUnderThePointerAndUnderTheKeyboard()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.items("card").size() >= 4);
        const QList<QQuickItem *> cards = app.items("card");
        QQuickItem *frost = App::under(cards.at(0), "frost"), *next = App::under(cards.at(1), "frost");
        QVERIFY(frost && next);
        QVERIFY(frost->property("veiled").toBool());
        QVERIFY(!frost->property("clear").toBool());
        app.hover(cards.at(0));
        QTRY_VERIFY(frost->property("clear").toBool());
        app.hover(app.item("search"));
        QTRY_VERIFY(!frost->property("clear").toBool());

        // The keyboard lifts the veil of the card it is on.
        app.item("home-grid")->forceActiveFocus();
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_VERIFY(next->property("clear").toBool());
        QVERIFY(!frost->property("clear").toBool());

        // Thumbnails shown as they are have nothing laid over them.
        app.window->setProperty("thumbs", "clear");
        QTRY_VERIFY(!frost->property("veiled").toBool());
        QVERIFY(!frost->property("effect").toBool());
    }

    void theGridDropsColumnsInANarrowWindow()
    {
        for (const auto &[width, columns] : {std::pair{1280, 4}, std::pair{950, 3}, std::pair{760, 2}}) {
            App app(false, QSize(width, 600));
            READY(app);
            QQuickItem *grid = app.item("home-grid");
            QVERIFY(grid);
            QTRY_VERIFY(app.items("card").size() >= columns);
            QTRY_COMPARE(grid->property("columns").toInt(), columns);
            // Nothing reaches past the window's edge.
            for (QQuickItem *card : app.items("card"))
                QVERIFY(card->mapToScene(QPointF(card->width(), 0)).x() <= width);
            for (const char *name : {"resume-row", "feed-tabs", "search"})
                QVERIFY2(app.item(name)->mapToScene(QPointF(app.item(name)->width(), 0)).x() <= width, name);
        }
    }

    void nothingOfTheDyeIsLeft()
    {
        for (const char *file : {":/DyeWash.qml", ":/Wordmark.qml", ":/shaders/dye.frag.qsb"})
            QVERIFY2(!QFile::exists(file), file);
    }

    void openingAVideoShowsTheWatchPageWithItsDetails()
    {
        App app;
        READY(app);
        QQuickItem *watch = app.item("watch");
        QVERIFY(watch);
        QVERIFY(!watch->isVisible());
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}, {"channel", "Northlight Woodworks"}});
        // At once, with what the card knew; the rest comes with the engine's answer.
        QVERIFY(watch->isVisible());
        QVERIFY(app.window->property("watching").toBool());
        QCOMPARE(app.item("watch-title")->property("text").toString(), QString("A dovetail box with four hand tools"));
        QTRY_VERIFY(app.item("watch-about")->property("text").toString().contains("The error everyone hits"));
        QTRY_COMPARE(App::count(app.item("seek"), "segment"), 7);
        QVERIFY(app.item("watch-stats")->property("text").toString().contains("59K views"));
        QVERIFY(app.item("seek-chapter")->property("text").toString().startsWith("1/7"));
        // The rail is still there, and the search field has made room.
        QVERIFY(app.item("rail-home")->isVisible());
        QVERIFY(!app.item("search")->isVisible());

        // Escape goes back to where the video was opened from.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!watch->isVisible());
        QCOMPARE(app.page(), QString("home"));
        QVERIFY(app.item("search")->isVisible());
    }

    void aVideoWithNothingButStreamsStillLooksWhole()
    {
        App app;
        READY(app);
        app.play({{"id", "bare0000001"}});
        QQuickItem *watch = app.item("watch");
        QTRY_COMPARE(app.item("watch-title")->property("text").toString(), QString("A video with nothing but streams"));
        QCOMPARE(App::count(app.item("seek"), "segment"), 1);
        QVERIFY(!app.item("seek-chapter")->isVisible());
        QVERIFY(!app.item("watch-stats")->isVisible());
        app.click(app.item("tab-about"));
        QVERIFY(app.items("chapter").isEmpty());
        QStringList said;
        App::texts(watch, said);
        QVERIFY(said.size() > 3);
        for (const QString &text : said)
            QVERIFY2(!text.contains("undefined") && !text.contains("NaN") && !text.contains("null"), qPrintable(text));
    }

    void aKeptPositionIsResumedAndCanBeStartedOver()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000001"}, {"title", "Why the borrow checker says no"}});
        QQuickItem *surface = app.item("surface");
        QVERIFY(surface);
        QTRY_COMPARE(surface->property("startedAt").toInt(), 845);
        QQuickItem *resumed = app.item("resumed");
        QVERIFY(resumed && resumed->isVisible());
        QVERIFY(resumed->property("text").toString().contains("14:05"));
        QCOMPARE(surface->property("lastSeek").toInt(), -1);
        app.click(app.item("start-over"));
        QCOMPARE(surface->property("lastSeek").toInt(), 0);
        QVERIFY(!resumed->isVisible());

        // A video nothing is remembered of starts at the start, and says nothing.
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!surface->property("pending").toBool());
        QCOMPARE(surface->property("startedAt").toInt(), 0);
        QVERIFY(!resumed->isVisible());
    }

    void chaptersAndTheListItCameFromAreInTheSheet()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.items("card").size() >= 4);
        // Opened from the feed's second card: what comes next is the rest of the feed.
        app.click(app.items("card").at(1));
        QQuickItem *surface = app.item("surface");
        QTRY_VERIFY(!surface->property("pending").toBool());
        app.click(app.item("tab-about"));
        QTRY_COMPARE(app.items("chapter").size(), 7);
        app.click(app.items("chapter").at(2));
        QCOMPARE(surface->property("lastSeek").toInt(), 760);

        app.click(app.item("tab-next"));
        QTRY_VERIFY(!app.items("next-entry").isEmpty());
        QCOMPARE(app.items("next-entry").first()->property("title").toString(), QString("The 1970s synth that refuses to stay in tune"));
        app.click(app.items("next-entry").first());
        QTRY_COMPARE(app.item("watch-title")->property("text").toString(), QString("The 1970s synth that refuses to stay in tune"));
        // Nothing was asked of the engine for the list: it was already here.
        QCOMPARE(app.asked("GET api/feed"), 2);
    }

    void theSheetMovesUnderTheVideoInANarrowWindow()
    {
        {
            App app;
            READY(app);
            app.play({{"id", "vid00000002"}});
            QQuickItem *sheet = app.item("sheet"), *surface = app.item("surface");
            QVERIFY(sheet && surface);
            QTRY_VERIFY(sheet->mapToScene(QPointF(0, 0)).x() >= surface->mapToScene(QPointF(surface->width(), 0)).x());
            QVERIFY(sheet->mapToScene(QPointF(sheet->width(), 0)).x() <= 1280);
            // Theatre: the video takes the width, and the sheet steps aside.
            QTest::keyClick(app.window, Qt::Key_T);
            QTRY_VERIFY(!sheet->isVisible());
            QVERIFY(surface->width() > 1000);
        }
        App app(false, QSize(950, 600));
        READY(app);
        app.play({{"id", "vid00000002"}});
        QQuickItem *sheet = app.item("sheet"), *about = app.item("watch-about"), *surface = app.item("surface");
        QVERIFY(sheet && about && surface);
        QTRY_VERIFY(sheet->mapToScene(QPointF(0, 0)).y() >= about->mapToScene(QPointF(0, about->height())).y());
        QVERIFY(surface->mapToScene(QPointF(surface->width(), 0)).x() <= 950);
        QVERIFY(sheet->mapToScene(QPointF(sheet->width(), 0)).x() <= 950);
    }

    void aLinkInADescriptionLeadsBackIntoTheAppOrIsOnlyCopied()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QQuickItem *watch = app.item("watch"), *surface = app.item("surface");
        QTRY_VERIFY(!surface->property("pending").toBool());
        QMetaObject::invokeMethod(watch, "follow", Q_ARG(QVariant, "qveil:time/760"));
        QCOMPARE(surface->property("lastSeek").toInt(), 760);
        // Another site is never opened: its address goes to the clipboard, and the window says so.
        QMetaObject::invokeMethod(watch, "follow", Q_ARG(QVariant, "qveil:copy/https%3A%2F%2Fexample.com%2Fa%3Fb%3D1"));
        QCOMPARE(QGuiApplication::clipboard()->text(), QString("https://example.com/a?b=1"));
        QTRY_VERIFY(app.item("toast")->isVisible());
        // A video link plays that video here.
        QMetaObject::invokeMethod(watch, "follow", Q_ARG(QVariant, "qveil:video/vid00000005"));
        QTRY_COMPARE(app.item("watch-title")->property("text").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        // Anything else is not followed at all.
        QMetaObject::invokeMethod(watch, "follow", Q_ARG(QVariant, "https://example.com/"));
        QMetaObject::invokeMethod(watch, "follow", Q_ARG(QVariant, "javascript:alert(1)"));
        QCOMPARE(app.page(), QString("home"));
    }

    void commentsAreOnlyFetchedWhenTheTabIsOpened()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::qWait(300);
        QCOMPARE(app.asked("GET api/comments"), 0);

        app.click(app.item("tab-comments"));
        QTRY_COMPARE(app.items("comment").size(), 5);
        QCOMPARE(app.asked("GET api/comments"), 1);
        QVERIFY(app.asked().filter("GET api/comments").first().contains("sort=top&n=40&replies=0"));
        QQuickItem *first = app.items("comment").first();
        QVERIFY(first->property("pinned").toBool());
        QCOMPARE(first->property("author").toString(), QString("Low Level Hours"));
        // How many there are in all is on the tab.
        QVERIFY(app.item("tab-comments")->property("text").toString().contains("1,204"));
        QVERIFY(app.items("reply").isEmpty());

        // Coming back to the tab asks for nothing.
        app.click(app.item("tab-about"));
        app.click(app.item("tab-comments"));
        QTest::qWait(300);
        QCOMPARE(app.asked("GET api/comments"), 1);
        QCOMPARE(app.items("comment").size(), 5);
    }

    void moreNewestAndRepliesAskAgain()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        app.click(app.item("tab-comments"));
        QTRY_COMPARE(app.items("comment").size(), 5);

        app.click(app.item("comments-replies"));
        QTRY_VERIFY(!app.items("reply").isEmpty());
        QVERIFY(app.asked().filter("GET api/comments").last().contains("replies=1"));
        QVERIFY(!app.item("comments-replies")->isVisible());

        app.click(app.item("comments-new"));
        QTRY_VERIFY(app.asked().filter("GET api/comments").last().contains("sort=new"));
        QTRY_COMPARE(app.items("comment").first()->property("author").toString(), QString("perennial_nico"));

        QMetaObject::invokeMethod(app.item("comments"), "more");
        QTRY_VERIFY(app.asked().filter("GET api/comments").last().contains("n=80"));
        QTRY_COMPARE(app.item("comments")->property("shown").toInt(), 45);
    }

    void aTimestampInACommentSeeksAndLeavingStopsTheLookup()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QQuickItem *surface = app.item("surface");
        app.click(app.item("tab-comments"));
        QTRY_COMPARE(app.items("comment").size(), 5);
        QQuickItem *text = App::under(app.items("comment").at(1), "comment-text");
        QVERIFY(text);
        QVERIFY(text->property("text").toString().contains("<a href=\"qveil:time/760\">12:40</a>"));
        QMetaObject::invokeMethod(text, "linkActivated", Q_ARG(QString, "qveil:time/760"));
        QCOMPARE(surface->property("lastSeek").toInt(), 760);

        // A lookup that is still running when the page is left is hung up on.
        app.play({{"id", "slowslowslo"}});
        app.click(app.item("tab-comments"));
        QTRY_VERIFY(app.backend->busy().contains("comments"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY_WITH_TIMEOUT(!app.backend->busy().contains("comments"), 1000);
        QVERIFY(app.backend->comments().isEmpty());
        QVERIFY2(app.backend->error().isEmpty(), qPrintable(app.backend->error()));
    }

    void theCommentsNoteIsTheEnginesOwnSentenceAndNoneIsSaidPlainly()
    {
        App app;
        READY(app);
        app.play({{"id", "nocomments0"}});
        app.click(app.item("tab-comments"));
        QTRY_VERIFY(app.item("comments-empty")->isVisible());
        QCOMPARE(app.item("comments-empty")->property("text").toString(), QString("No comments here, or they are turned off."));
        QCOMPARE(app.item("comments-note")->property("text").toString(),
                 app.backend->privacy().value("pages").toMap().value("comments").toString());
        QVERIFY(!app.item("comments-note")->property("text").toString().isEmpty());
        QVERIFY(app.items("comment").isEmpty());
    }

    void aLensIsWrittenReadSavedAndShown()
    {
        App app(true);
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QCOMPARE(app.page(), QString("lenses"));
        QQuickItem *text = app.item("lens-text");
        QVERIFY(text);
        // With no lens yet, the page opens ready to write one.
        QTRY_VERIFY(text->property("editing").toBool());
        QVERIFY(app.items("lens-row").isEmpty());
        QVERIFY(!app.item("lens-save")->isEnabled());
        app.type("bread, sourdough over 30 min");
        QTRY_COMPARE(app.item("lens-tags")->property("count").toInt(), 3);
        // What a refresh will ask YouTube for, word for word.
        const QString searches = app.item("lens-searches")->property("text").toString();
        QVERIFY2(searches.contains("\u201cbread\u201d") && searches.contains("\u201csourdough\u201d"), qPrintable(searches));
        QCOMPARE(app.asked("GET api/lens?"), 0);

        QVERIFY(app.item("lens-save")->isEnabled());
        app.click(app.item("lens-save"));
        QTRY_COMPARE(app.items("lens-row").size(), 1);
        QVERIFY(app.items("lens-row").first()->property("current").toBool());
        QTRY_COMPARE(app.item("lens-grid")->property("count").toInt(), 6);
        QVERIFY(!app.items("card-why").isEmpty());
        QCOMPARE(text->property("text").toString(), QString("bread, sourdough over 30 min"));
        // Saved, and nothing changed since: there is nothing to save.
        QVERIFY(!app.item("lens-save")->isEnabled());
    }

    void aLensWithoutATopicCannotBeSaved()
    {
        App app(true);
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_VERIFY(app.item("lens-text")->property("editing").toBool());
        app.type("no music");
        QQuickItem *error = app.item("lens-error");
        QTRY_VERIFY(error->isVisible());
        QVERIFY(error->property("text").toString().startsWith("Say what it should be about"));
        QVERIFY(!app.item("lens-save")->isEnabled());
        // Enter doesn't save it either.
        QTest::keyClick(app.window, Qt::Key_Return);
        QTest::qWait(300);
        QCOMPARE(app.asked("POST api/lenses"), 0);
    }

    void deletingALensAsksOnceInPlace()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.items("lens-row").size(), 2);
        // The first lens is open, with its words in the editor.
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString("slow woodworking with hand tools, no music"));
        QQuickItem *del = app.item("lens-delete");
        QCOMPARE(del->property("text").toString(), QString("Delete"));
        app.click(del);
        QCOMPARE(del->property("text").toString(), QString("Delete?"));
        QCOMPARE(app.items("lens-row").size(), 2);
        QTRY_VERIFY(app.item("lens-cancel")->isVisible());
        app.click(app.item("lens-cancel"));
        QTRY_COMPARE(del->property("text").toString(), QString("Delete"));
        app.click(del);
        app.click(del);
        QTRY_COMPARE(app.items("lens-row").size(), 1);
        // The one that is left is the one that is open.
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString("systems talks over 30 min, this year"));

        // Another lens is a click away.
        app.window->setProperty("page", "home");
    }

    void aSearchCanBeKeptAsALensAndHomeCanStartOne()
    {
        App app;
        READY(app);
        QMetaObject::invokeMethod(app.item("search"), "edit");
        app.type("talks over 30 min");
        QTest::keyClick(app.window, Qt::Key_Return);
        QCOMPARE(app.page(), QString("search"));
        QTRY_VERIFY(app.item("reading-tags")->isVisible());
        QCOMPARE(app.item("reading-tags")->property("count").toInt(), 2);
        QTRY_VERIFY(!app.items("card-why").isEmpty());
        QQuickItem *keep = app.item("keep-lens");
        QVERIFY(keep && keep->isVisible());
        app.click(keep);
        QTRY_COMPARE(app.page(), QString("lenses"));
        QTRY_COMPARE(app.items("lens-row").size(), 3);
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString("talks over 30 min"));
        QVERIFY(app.items("lens-row").last()->property("current").toBool());

        // Home's "New lens" opens the page with nothing written yet.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTest::keyClick(app.window, Qt::Key_1);
        app.click(app.item("feed-tab-new"));
        QCOMPARE(app.page(), QString("lenses"));
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString());
        QTRY_VERIFY(app.item("lens-text")->property("editing").toBool());
        for (QQuickItem *row : app.items("lens-row"))
            QVERIFY(!row->property("current").toBool());
    }

    void theLensNoteIsTheEnginesOwnSentence()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QQuickItem *note = app.item("lens-note");
        QVERIFY(note);
        QTRY_COMPARE(note->property("text").toString(), app.backend->privacy().value("pages").toMap().value("lenses").toString());
        QVERIFY(!note->property("text").toString().isEmpty());
    }

    void pictureInPictureMovesTheVideoOutAndBack()
    {
        App app;
        READY(app);
        QQuickWindow *pip = app.pip();
        QVERIFY(pip);
        QVERIFY(!pip->isVisible());
        // A video that is picked up at 14:05.
        app.play({{"id", "vid00000001"}, {"title", "Why the borrow checker says no"}});
        QQuickItem *main = app.item("surface");
        QTRY_COMPARE(main->property("startedAt").toInt(), 845);
        QVERIFY(!app.item("now-playing")->isVisible());

        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(pip->isVisible());
        QCOMPARE(pip->title(), QString("Picture-in-Picture"));
        QVERIFY(app.window->property("pip").toBool());
        QQuickItem *small = app.inPip("pip-surface");
        QVERIFY(small);
        // The video left the page for the small window, and carried on where it was.
        QVERIFY(small->property("active").toBool() && !main->property("active").toBool());
        QCOMPARE(small->property("startedAt").toInt(), 845);
        QCOMPARE(small->property("info").toMap().value("title").toString(), QString("Why the borrow checker says no"));
        QVERIFY(!app.item("watch")->isVisible());
        QCOMPARE(app.page(), QString("home"));
        QVERIFY(app.item("search")->isVisible());
        QQuickItem *bar = app.item("now-playing");
        QVERIFY(bar->isVisible());
        QCOMPARE(bar->property("title").toString(), QString("Why the borrow checker says no"));

        // And back, by the bar's own action.
        app.click(app.item("now-back"));
        QTRY_VERIFY(!pip->isVisible());
        QVERIFY(main->property("active").toBool() && !small->property("active").toBool());
        QCOMPARE(main->property("startedAt").toInt(), 845);
        QVERIFY(app.item("watch")->isVisible());
        QVERIFY(!bar->isVisible());
        QVERIFY(!app.window->property("pip").toBool());
    }

    void anotherVideoOpenedMeanwhilePlaysInTheSmallWindow()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QTRY_VERIFY(app.items("card").size() >= 4);
        app.click(app.items("card").at(2));
        QQuickItem *small = app.inPip("pip-surface");
        QTRY_COMPARE(small->property("info").toMap().value("id").toString(), QString("vid00000003"));
        QTRY_VERIFY(!small->property("pending").toBool());
        // The main window stays where it was, browsing.
        QVERIFY(!app.item("watch")->isVisible());
        QVERIFY(!app.item("surface")->property("active").toBool());
        QCOMPARE(app.item("now-playing")->property("title").toString(), QString("The 1970s synth that refuses to stay in tune"));
    }

    void closingTheSmallWindowStopsTheVideo()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        app.pip()->close();
        QTRY_VERIFY(!app.pip()->isVisible());
        QVERIFY(!app.inPip("pip-surface")->property("active").toBool());
        QVERIFY(!app.item("surface")->property("active").toBool());
        QVERIFY(!app.item("now-playing")->isVisible());
        QVERIFY(!app.window->property("pip").toBool());

        // The bar's Close does the same.
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        app.click(app.item("now-close"));
        QTRY_VERIFY(!app.pip()->isVisible());
        QVERIFY(!app.window->property("pip").toBool());
        // Nothing is playing, so there is nothing to send to a small window.
        QTest::keyClick(app.window, Qt::Key_I);
        QTest::qWait(200);
        QVERIFY(!app.pip()->isVisible());
    }

    void theSmallWindowGoesWhenTheEngineDoes()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QSignalSpy readiness(app.backend.get(), &Backend::readyChanged);
        app.killEngine();
        QTRY_VERIFY_WITH_TIMEOUT(readiness.count() >= 1, 5000);
        QTRY_VERIFY_WITH_TIMEOUT(!app.pip()->isVisible(), 2000);
        QVERIFY(!app.inPip("pip-surface")->property("active").toBool());
        QVERIFY(!app.item("now-playing")->isVisible());
    }
};

QVEIL_SUITE(WindowTests)
#include "window_tests.moc"
