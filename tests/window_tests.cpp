#include <QClipboard>
#include <QDirIterator>
#include <QElapsedTimer>
#include <QFont>
#include <QQuickStyle>
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
#include "videomodel.h"
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
    explicit App(bool empty = false, QSize size = QSize(1280, 720), const QMap<QString, QString> &extra = {})
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
        for (auto it = extra.begin(); it != extra.end(); ++it)
            environment.insert(it.key(), it.value());
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
        // A page that is still filling moves what is on it; the click waits
        // until the target has stood still for a moment.
        QPointF was(-1, -1);
        for (int i = 0; i < 25; ++i) {
            QTest::qWait(40);
            const QPointF now = target->mapToScene(QPointF(0, 0));
            if (now == was)
                break;
            was = now;
        }
        // What is outside the view of a page that scrolls is scrolled to first.
        const qreal top = target->mapToScene(QPointF(0, 0)).y(), bottom = top + target->height();
        qreal by = 0;
        for (QQuickItem *up = target->parentItem(); up; up = up->parentItem()) {
            // (Only what scrolls up and down: a strip that scrolls sideways is not it.)
            if (!up->property("contentY").isValid() || !up->property("contentHeight").isValid()
                || up->property("contentHeight").toReal() <= up->height() + 1)
                continue;
            const qreal viewTop = up->mapToScene(QPointF(0, 0)).y(), viewBottom = qMin(viewTop + up->height(), qreal(window->height()));
            if (bottom > viewBottom - 8)
                by = bottom - viewBottom + 40;
            else if (top < viewTop + 8)
                by = top - viewTop - 40;
            if (by != 0) {
                // (A list's header lies before its start, so the view's own bounds decide.)
                up->setProperty("contentY", up->property("contentY").toReal() + by);
                QMetaObject::invokeMethod(up, "returnToBounds");
                QTest::qWait(40);
            }
            break;
        }
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
    // Home's feed in full (the grid with its tabs), as a shelf's "See all" opens it.
    void seeAll(const char *feed = "latest") const
    {
        QMetaObject::invokeMethod(item("feedPage"), "showAll", Q_ARG(QVariant, QString(feed)));
    }

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

// Something to play: a clip, made with ffmpeg in `dir`, which the stand-in
// engine serves as the video. Empty when there is no ffmpeg to make it.
QString makeClip(const QTemporaryDir &dir)
{
    const QString clip = dir.filePath("clip.mkv");
    QProcess make;
    make.start("ffmpeg", {"-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=d=120:s=160x90:r=5", "-f", "lavfi", "-i", "anullsrc",
                          "-shortest", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", clip});
    return make.waitForFinished(60000) && make.exitCode() == 0 && QFile::exists(clip) ? clip : QString();
}

#define READY(app) QVERIFY2((app).ok, "the window didn't load, or the stand-in engine didn't start")

} // namespace

class WindowTests : public QObject {
    Q_OBJECT

private slots:
    void initTestCase()
    {
        qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem");
        qmlRegisterType<VideoModel>("Qveil", 1, 0, "VideoModel");
    }

    void init()
    {
        // No test may leave a QML warning behind. Where the engine is killed
        // on purpose, pictures that were loading from it say so, which is not
        // a fault; a script error still is.
        const bool engineDies = QByteArray(QTest::currentTestFunction()) == "theSmallWindowGoesWhenTheEngineDoes";
        QTest::failOnWarning(QRegularExpression(engineDies ? "TypeError|ReferenceError|Binding loop"
                                                           : "qrc:/|\\.qml|QML|TypeError|ReferenceError"));
    }

    void theBarAcrossTheTopHasThePlacesSearchAndTheRoute()
    {
        App app;
        READY(app);
        QQuickItem *bar = app.item("bar");
        QVERIFY(bar);
        QCOMPARE(bar->mapToScene(QPointF(0, 0)), QPointF(0, 0));
        QCOMPARE(bar->width(), qreal(1280));
        for (const char *name : {"wordmark", "nav-home", "nav-lenses", "nav-channels", "nav-history", "nav-route", "nav-settings", "search"}) {
            QVERIFY2(app.item(name), name);
            // All of it is in the bar.
            QVERIFY2(app.item(name)->mapToScene(QPointF(0, app.item(name)->height())).y() <= bar->height(), name);
        }
        // The places are words.
        QStringList said;
        App::texts(bar, said);
        for (const char *word : {"Home", "Lenses", "Channels", "History", "Direct"})
            QVERIFY2(said.contains(word), word);
        QCOMPARE(app.page(), QString("home"));
        QVERIFY(app.item("nav-home")->property("current").toBool());
        // There is nothing down the side: the page starts at the window's edge.
        QTRY_VERIFY(app.item("lead")->isVisible());
        QVERIFY(app.item("lead")->mapToScene(QPointF(0, 0)).x() <= 40);

        app.click(app.item("nav-channels"));
        QCOMPARE(app.page(), QString("channels"));
        QVERIFY(app.item("nav-channels")->property("current").toBool());
        QVERIFY(!app.item("nav-home")->property("current").toBool());
        app.click(app.item("nav-history"));
        QCOMPARE(app.page(), QString("history"));
        app.click(app.item("nav-settings"));
        QCOMPARE(app.page(), QString("settings"));
        QVERIFY(app.item("nav-settings")->property("current").toBool());

        // What the route indicator says when pointed at is the engine's sentence,
        // and pressing it opens the route, under it and inside the window.
        QCOMPARE(app.item("nav-route")->property("tip").toString(), app.backend->privacy().value("summary").toString());
        QVERIFY(!app.item("nav-route")->property("tip").toString().isEmpty());
        app.click(app.item("nav-route"));
        QObject *popup = app.object("routePopup");
        QTRY_VERIFY(popup->property("opened").toBool());
        QQuickItem *background = popup->property("background").value<QQuickItem *>();
        QTest::qWait(200);
        const QRectF at = background->mapRectToScene(QRectF(0, 0, background->width(), background->height()));
        QVERIFY2(at.top() >= bar->height() - 1 && at.right() <= 1280 && at.left() > 640,
                 qPrintable(QString("%1,%2 %3x%4").arg(at.x()).arg(at.y()).arg(at.width()).arg(at.height())));
    }

    void theBarFitsANarrowWindow()
    {
        App app(false, QSize(760, 520));
        READY(app);
        const auto edges = [&](const char *name) {
            QQuickItem *item = app.item(name);
            return item->mapRectToScene(QRectF(0, 0, item->width(), item->height()));
        };
        QVERIFY2(edges("nav-settings").right() <= 760, qPrintable(QString::number(edges("nav-settings").right())));
        QVERIFY2(edges("search").width() >= 150, qPrintable(QString::number(edges("search").width())));
        // Nothing lies over anything else.
        QVERIFY(edges("nav-history").right() <= edges("search").left());
        QVERIFY(edges("search").right() <= edges("nav-route").left());
        QVERIFY(edges("nav-route").right() <= edges("nav-settings").left());
        // The route is still said, by its colour and when pointed at.
        QVERIFY(!app.item("nav-route")->property("tip").toString().isEmpty());
    }

    void theRouteIndicatorSaysWhereYouStand()
    {
        App app;
        READY(app);
        QQuickItem *route = app.item("nav-route");
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
        QQuickItem *lead = app.item("lead");
        QVERIFY(lead);
        QTRY_VERIFY(lead->isVisible());
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
        QVERIFY(app.item("lead-meta")->property("text").toString().contains("26:58 left"));
        // How long they are kept is the setting's number.
        QVERIFY(app.item("lead-kept")->property("text").toString().contains("7 days"));
        // The others have a shelf, each with what is left of it.
        QTRY_VERIFY(app.item("shelf-resume"));
        QTRY_COMPARE(App::count(app.item("shelf-resume"), "card"), 2);
        QCOMPARE(App::under(app.item("shelf-resume"), "card-why")->property("text").toString(), QString("48:20 left"));

        app.backend->forgetResume("vid00000001");
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        QTRY_COMPARE(App::count(app.item("shelf-resume"), "card"), 1);
        app.backend->clearResume();
        QTRY_VERIFY(!lead->isVisible());
        QTRY_VERIFY(!app.item("shelf-resume"));

        // With nothing unfinished, and with resume off, there is no lead.
        App none(true);
        READY(none);
        QTRY_VERIFY(none.item("shelf-latest"));
        QVERIFY(!none.item("lead")->isVisible());
    }

    void aLensTabShowsThatLensInTheGrid()
    {
        App app;
        READY(app);
        app.seeAll();
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
        // With thumbnails set to frosted, in the feed in full.
        app.window->setProperty("thumbs", "frosted");
        app.seeAll();
        QTRY_VERIFY(app.items("card").size() >= 4);
        const QList<QQuickItem *> cards = app.items("card");
        QQuickItem *first = App::under(cards.at(0), "frost"), *second = App::under(cards.at(1), "frost"),
                   *third = App::under(cards.at(2), "frost");
        QVERIFY(first && second && third);
        QVERIFY(third->property("veiled").toBool());
        // The keyboard's cursor is on the first card from the start, and lifts its veil.
        QTRY_VERIFY(app.item("home-grid")->hasActiveFocus());
        QTRY_VERIFY(first->property("clear").toBool());
        QVERIFY(!third->property("clear").toBool());
        // So does the pointer, for the card it is on (and the cursor steps aside).
        app.hover(cards.at(1));
        app.hover(cards.at(2));
        QTRY_VERIFY(third->property("clear").toBool());
        QTRY_VERIFY(!first->property("clear").toBool());
        app.hover(app.item("search"));
        QTRY_VERIFY(!third->property("clear").toBool());

        // The keyboard again: the card it moves to.
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_VERIFY(second->property("clear").toBool());
        QVERIFY(!third->property("clear").toBool());

        // Thumbnails shown as they are have nothing laid over them.
        app.window->setProperty("thumbs", "clear");
        QTRY_VERIFY(!third->property("veiled").toBool());
        QVERIFY(!third->property("effect").toBool());
    }

    void theGridDropsColumnsInANarrowWindow()
    {
        for (const auto &[width, columns] : {std::pair{1280, 4}, std::pair{950, 3}, std::pair{760, 2}}) {
            App app(false, QSize(width, 600));
            READY(app);
            app.seeAll();
            QQuickItem *grid = app.item("home-grid");
            QVERIFY(grid);
            QTRY_VERIFY(app.items("card").size() >= columns);
            QTRY_COMPARE(grid->property("columns").toInt(), columns);
            // Nothing reaches past the window's edge.
            for (QQuickItem *card : app.items("card"))
                QVERIFY(card->mapToScene(QPointF(card->width(), 0)).x() <= width);
            for (const char *name : {"feed-tabs", "search"})
                QVERIFY2(app.item(name)->mapToScene(QPointF(app.item(name)->width(), 0)).x() <= width, name);
        }
    }

    void theLeadAndTheShelvesFitANarrowWindow()
    {
        for (const int width : {950, 760}) {
            App app(false, QSize(width, 760));
            READY(app);
            QTRY_VERIFY(app.item("lead")->isVisible());
            QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 2);
            const auto right = [&](const char *name) {
                QQuickItem *item = app.item(name);
                return item->mapToScene(QPointF(item->width(), 0)).x();
            };
            QVERIFY2(right("lead") <= width, qPrintable(QString::number(right("lead"))));
            // The title has room to be read, and the buttons stay on the lead's own ground.
            QVERIFY2(app.item("lead-title")->width() >= 220, qPrintable(QString::number(app.item("lead-title")->width())));
            QQuickItem *picture = app.item("lead-picture");
            QVERIFY(picture);
            const qreal edge = picture->isVisible() ? picture->mapToScene(QPointF(0, 0)).x() + 100 : right("lead");
            QVERIFY2(right("lead-forget") <= edge, qPrintable(QString("%1 past %2").arg(right("lead-forget")).arg(edge)));
            QQuickItem *all = App::under(app.item("shelf-latest"), "shelf-all");
            QVERIFY(all->mapToScene(QPointF(all->width(), 0)).x() <= width);
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
        // The bar is still there, with the search field in it.
        QVERIFY(app.item("nav-home")->isVisible());
        QVERIFY(!app.item("nav-home")->property("current").toBool());
        QVERIFY(app.item("search")->isVisible());

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
        // No chapters, so no tab for them.
        QVERIFY(!app.item("tab-chapters")->isVisible());
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
        // Opened from the feed's second video (Latest's first card: the first
        // video leads, above it): what comes next is the rest of the feed.
        app.click(app.items("card").at(0));
        QQuickItem *surface = app.item("surface");
        QTRY_VERIFY(!surface->property("pending").toBool());
        app.click(app.item("tab-chapters"));
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
        // A narrow column has no room beside the video: there the sheet is under it.
        App app(false, QSize(630, 1030));
        READY(app);
        app.play({{"id", "vid00000002"}});
        QQuickItem *sheet = app.item("sheet"), *about = app.item("watch-about"), *surface = app.item("surface");
        QVERIFY(sheet && about && surface);
        QTRY_VERIFY(sheet->mapToScene(QPointF(0, 0)).y() >= about->mapToScene(QPointF(0, about->height())).y());
        QVERIFY(surface->mapToScene(QPointF(surface->width(), 0)).x() <= 630);
        QVERIFY(sheet->mapToScene(QPointF(sheet->width(), 0)).x() <= 630);
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
        app.click(app.item("tab-next"));
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
        QTRY_VERIFY(!app.items("card-why").isEmpty());
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
        // Every result is there for the one reason, which the tags above already say:
        // it isn't repeated under each card.
        QTRY_VERIFY(app.item("searchPage")->property("grid").value<QQuickItem *>()->property("count").toInt() > 0);
        QVERIFY(app.items("card-why").isEmpty());
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
        QMetaObject::invokeMethod(app.item("shelves"), "positionViewAtEnd");
        app.click(app.item("home-new-lens"));
        QCOMPARE(app.page(), QString("lenses"));
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString());
        // So does the feed's own tab row.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTest::keyClick(app.window, Qt::Key_1);
        app.seeAll();
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
        const QString said = app.backend->privacy().value("pages").toMap().value("lenses").toString();
        const QString shortly = app.backend->privacy().value("pages").toMap().value("lenses_short").toString();
        QVERIFY(!shortly.isEmpty() && shortly != said);
        // The engine's short sentence is always there, as it wrote it (the
        // window cuts nothing out of the engine's text)...
        QQuickItem *first = app.item("lens-note-first"), *note = app.item("lens-note");
        QVERIFY(first && note);
        QTRY_COMPARE(first->property("text").toString(), shortly);
        QVERIFY(first->isVisible() && !note->isVisible());
        // ...and the whole of it opens in place.
        app.click(app.item("lens-note-more"));
        QTRY_VERIFY(note->isVisible());
        QCOMPARE(note->property("text").toString(), said);
        app.click(app.item("lens-note-more"));
        QTRY_VERIFY(!note->isVisible());
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

    void theLeadsVideoIsNotAlsoTheFirstOfLatest()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")) && App::count(shelf, "card") >= 3);
        // The lead is the first of the feed; Latest starts at the second.
        QCOMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
        QStringList ids;
        for (QQuickItem *card : app.items("card"))
            if (App::under(shelf, "card") && card->parentItem() && shelf->isAncestorOf(card))
                ids << card->property("videoId").toString();
        QVERIFY2(!ids.contains("vid00000001"), qPrintable(ids.join(",")));
        QVERIFY2(ids.contains("vid00000002"), qPrintable(ids.join(",")));
        // Playing from Latest goes on through the feed as it is.
        QTest::keyClick(app.window, Qt::Key_Down);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000002"));
        QTRY_VERIFY(app.items("next-entry").size() >= 2);
    }

    void leavingAPlayingVideoHandsItToTheSmallWindow()
    {
        App app;
        READY(app);
        QQuickWindow *pip = app.pip();
        app.play({{"id", "vid00000001"}, {"title", "Why the borrow checker says no"}});
        QTRY_COMPARE(app.item("surface")->property("startedAt").toInt(), 845);
        // A place key: the video goes on in the small window, from where it was.
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_VERIFY(pip->isVisible());
        QCOMPARE(app.page(), QString("lenses"));
        QVERIFY(!app.window->property("watching").toBool());
        QQuickItem *small = app.inPip("pip-surface");
        QVERIFY(small->property("active").toBool());
        QCOMPARE(small->property("info").toMap().value("id").toString(), QString("vid00000001"));
        QCOMPARE(small->property("startedAt").toInt(), 845);
        app.pip()->close();
        QTRY_VERIFY(!pip->isVisible());

        // Esc from the watch page does the same.
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTRY_VERIFY(app.window->property("watching").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(pip->isVisible());
        QTRY_COMPARE(app.inPip("pip-surface")->property("info").toMap().value("id").toString(), QString("vid00000002"));
        app.pip()->close();
        QTRY_VERIFY(!pip->isVisible());

        // A video that is paused is put away instead (where it was is kept).
        app.play({{"id", "vid00000003"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QObject *mpv = app.item("surface")->property("mpv").value<QObject *>();
        QMetaObject::invokeMethod(mpv, "togglePause");
        QTRY_VERIFY(mpv->property("paused").toBool());
        QTest::keyClick(app.window, Qt::Key_1);
        QTRY_VERIFY(!app.window->property("watching").toBool());
        QTest::qWait(200);
        QVERIFY(!pip->isVisible());
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
        app.click(app.items("card").at(1));
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

    void changingTheRouteMidVideoOpensItAgainWhereItWas()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QQuickItem *surface = app.item("surface");
        QObject *mpv = surface->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mpv && mpv->property("started").toBool(), 15000);
        QMetaObject::invokeMethod(surface, "seekTo", Q_ARG(QVariant, 40));
        QTRY_VERIFY_WITH_TIMEOUT(mpv->property("position").toDouble() >= 40, 10000);
        const int opened = app.asked("POST api/play");

        // The engine ends the video's streams with the old route: it is looked up
        // and opened again by the new one, from where it had got to.
        app.backend->setConfig({{"route", "tor"}});
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("tor"));
        QTRY_COMPARE(app.asked("POST api/play"), opened + 1);
        QTRY_VERIFY_WITH_TIMEOUT(mpv->property("started").toBool() && !surface->property("pending").toBool(), 15000);
        QVERIFY(app.window->property("watching").toBool());
        QCOMPARE(surface->property("info").toMap().value("id").toString(), QString("vid00000002"));
        // (What the window asked of the player: the stand-in's stream can't be seeked, so how far it gets is its own.)
        const double at = surface->property("startedAt").toDouble();
        QVERIFY2(at >= 38 && at < 100, qPrintable(QString::number(at)));
        QVERIFY(app.item("errorBar") && !app.item("errorBar")->isVisible());
    }

    void changingTheRouteMidVideoKeepsPictureInPicture()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QQuickItem *surface = app.item("surface");
        QObject *mpv = surface->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mpv && mpv->property("started").toBool(), 15000);
        QMetaObject::invokeMethod(surface, "seekTo", Q_ARG(QVariant, 40));
        QTRY_VERIFY_WITH_TIMEOUT(mpv->property("position").toDouble() >= 40, 10000);
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QQuickItem *small = app.inPip("pip-surface");
        QObject *pipMpv = small->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(pipMpv->property("started").toBool(), 15000);
        const int opened = app.asked("POST api/play");

        app.backend->setConfig({{"route", "tor"}});
        QTRY_COMPARE(app.asked("POST api/play"), opened + 1);
        QTRY_VERIFY_WITH_TIMEOUT(pipMpv->property("started").toBool() && !small->property("pending").toBool(), 15000);
        // Still in the small window, and not on the page.
        QVERIFY(app.pip()->isVisible());
        QVERIFY(!app.window->property("watching").toBool());
        QCOMPARE(small->property("info").toMap().value("id").toString(), QString("vid00000002"));
        QVERIFY(small->property("startedAt").toDouble() >= 38);
    }

    void pictureInPictureHandsOverThePlayerWithoutOpeningTheVideoAgain()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QQuickItem *main = app.item("surface");
        QObject *mainMpv = main->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mainMpv && mainMpv->property("started").toBool(), 15000);
        QMetaObject::invokeMethod(main, "seekTo", Q_ARG(QVariant, 40));
        QTRY_VERIFY_WITH_TIMEOUT(mainMpv->property("position").toDouble() >= 40, 10000);
        QTRY_VERIFY(!mainMpv->property("buffering").toBool());
        // (Where a seek got to is told once it has rested.)
        QTest::qWait(1000);
        // What opening the video asks of the engine: a lookup, and its stream.
        const int looked = app.asked("POST api/play"), streamed = app.asked("GET s/");
        QVERIFY(streamed >= 1);
        // A handover says nothing of where the video is: it is the same playing
        // on. (The player that has it tells, every so often, as it did.)
        int noted = app.asked("POST api/resume");
        auto noteless = [&] {
            QTest::qWait(200);
            const int now = app.asked("POST api/resume");
            const bool same = now == noted;
            noted = now;
            return same;
        };

        // Out to the small window: the player it was playing in goes with it.
        const double before = mainMpv->property("position").toDouble();
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QVERIFY(noteless());
        QQuickItem *small = app.inPip("pip-surface");
        QObject *smallMpv = small->property("mpv").value<QObject *>();
        QVERIFY(small->property("active").toBool() && !main->property("active").toBool());
        // Where it was, at once: not at the start of a file being opened.
        QVERIFY(smallMpv->property("started").toBool() && smallMpv->property("loaded").toBool());
        QVERIFY2(qAbs(smallMpv->property("position").toDouble() - before) < 1.5,
                 qPrintable(QString::number(smallMpv->property("position").toDouble())));
        // The page's player has nothing any more.
        QVERIFY(!mainMpv->property("loaded").toBool() && !mainMpv->property("started").toBool());
        // And it goes on playing.
        QTRY_VERIFY_WITH_TIMEOUT(smallMpv->property("position").toDouble() > before + 0.5, 5000);
        QVERIFY(!App::under(small, "resumed")->isVisible());

        // And back.
        const double out = smallMpv->property("position").toDouble();
        QTest::keyClick(app.pip(), Qt::Key_I);
        QTRY_VERIFY(!app.pip()->isVisible());
        QVERIFY(noteless());
        QVERIFY(main->property("active").toBool() && !small->property("active").toBool());
        QVERIFY(app.window->property("watching").toBool());
        QVERIFY(mainMpv->property("started").toBool());
        QVERIFY2(mainMpv->property("position").toDouble() >= out - 0.5 && mainMpv->property("position").toDouble() < out + 1.5,
                 qPrintable(QString::number(mainMpv->property("position").toDouble())));
        QTRY_VERIFY_WITH_TIMEOUT(mainMpv->property("position").toDouble() > out + 0.5, 5000);
        // The "Resumed at" line doesn't come up again, as for a video picked up.
        QVERIFY(!App::under(main, "resumed")->isVisible());

        // Leaving the page while it plays hands it over the same way.
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_VERIFY(app.pip()->isVisible());
        QVERIFY(noteless());
        QVERIFY(smallMpv->property("started").toBool());
        QTRY_VERIFY_WITH_TIMEOUT(smallMpv->property("position").toDouble() > out + 1, 5000);

        // Nothing was looked up or opened again.
        QTest::qWait(300);
        QCOMPARE(app.asked("POST api/play"), looked);
        QCOMPARE(app.asked("GET s/"), streamed);
        QVERIFY(app.item("errorBar") && !app.item("errorBar")->isVisible());
    }

    void onlyThePlayerThatIsShowingTellsWhereTheVideoIs()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QQuickItem *main = app.item("surface");
        QTRY_VERIFY(!main->property("pending").toBool());
        QObject *mainTimer = main->findChild<QObject *>("report-timer");
        QVERIFY(mainTimer);
        QTRY_VERIFY(mainTimer->property("running").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QObject *smallTimer = app.inPip("pip-surface")->findChild<QObject *>("report-timer");
        QVERIFY(smallTimer->property("running").toBool());
        QVERIFY(!mainTimer->property("running").toBool());
        QTest::keyClick(app.pip(), Qt::Key_I);
        QTRY_VERIFY(!app.pip()->isVisible());
        QVERIFY(mainTimer->property("running").toBool());
        QVERIFY(!smallTimer->property("running").toBool());
    }

    void theLastWordOfALinesActionsStandsAtTheEdgeOfWhatIsUnderIt()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        // A button's word is `pad` inside its edge; the row of them is pulled out by as much.
        const qreal pad = 10;
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        // Home: the shelf's last action ends `pad` past the lead above it.
        QQuickItem *refresh = nullptr;
        QTRY_VERIFY((refresh = app.item("shelf-refresh")) && refresh->isVisible());
        QCOMPARE(box(refresh).right(), box(app.item("lead")).right() + pad);

        // A search: "Keep as a lens" ends `pad` past the rule under the heading.
        QMetaObject::invokeMethod(app.item("search"), "edit");
        app.type("rust");
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.page(), QString("search"));
        QQuickItem *keep = app.item("keep-lens");
        QTRY_VERIFY(keep && keep->isVisible());
        QTRY_COMPARE(box(keep).right(), qreal(1280 - 24) + pad);

        // Lenses: Delete, the last of the lens's actions.
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.page(), QString("lenses"));
        QQuickItem *del = nullptr;
        QTRY_VERIFY((del = app.item("lens-delete")) && del->isVisible());
        QCOMPARE(box(del).right(), qreal(1280 - 24) + pad);
    }

    void theChannelsPaneHeadingStandsOverItsFirstPicture()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        for (const QSize size : {QSize(1280, 720), QSize(1920, 1080)}) {
            App app(false, size);
            READY(app);
            QTRY_COMPARE(app.backend->feed()->count(), 12);
            QTest::keyClick(app.window, Qt::Key_3);
            QTRY_COMPARE(app.page(), QString("channels"));
            QQuickItem *pane = nullptr;
            QTRY_VERIFY((pane = app.item("channel-pane")) && pane->property("count").toInt() == 3);
            QQuickItem *name = app.item("channel-pane-name");
            QTRY_VERIFY(App::under(pane, "frost"));
            QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
            const qreal picture = box(App::under(pane, "frost")).left();
            // (The heading's own text has no side bearing to speak of; a pixel is allowed for it.)
            QVERIFY2(qAbs(box(name).left() - picture) <= 1, qPrintable(QString("%1 vs %2 at %3").arg(box(name).left()).arg(picture).arg(size.width())));
        }
    }

    void aLensTabIsCutByTheRoomThereIsAndNotBeforeThat()
    {
        {
            // Plenty of room: the whole sentence, whatever its length.
            App app;
            READY(app);
            QTest::keyClick(app.window, Qt::Key_2);
            QTRY_COMPARE(app.items("lens-row").size(), 2);
            for (QQuickItem *tab : app.items("lens-row"))
                QVERIFY2(!tab->property("cut").toBool(), qPrintable(tab->property("text").toString()));
            QCOMPARE(app.items("lens-row").first()->property("text").toString(), QString("slow woodworking with hand tools, no music"));
            QVERIFY(app.items("lens-row").first()->width() > 300);
        }
        {
            // Twelve of them: each is cut to share what there is, and the row scrolls from there.
            App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_LENSES", "12"}});
            READY(app);
            QTest::keyClick(app.window, Qt::Key_2);
            QTRY_COMPARE(app.items("lens-row").size(), 12);
            QVERIFY(app.items("lens-row").last()->property("cut").toBool());
            for (QQuickItem *tab : app.items("lens-row"))
                QVERIFY2(tab->width() <= 141, qPrintable(QString::number(tab->width())));
        }
    }

    void settingsChangeTheEngineAndUseItsWords()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_5);
        QCOMPARE(app.page(), QString("settings"));
        QVERIFY(app.item("nav-settings")->property("current").toBool());
        const auto pages = [&] { return app.backend->privacy().value("pages").toMap(); };

        // Resume: the days, and the engine's sentence for them.
        QVERIFY(app.item("resume-7")->property("current").toBool());
        QCOMPARE(app.item("set-resume-note")->property("text").toString(), pages().value("resume").toString());
        app.click(app.item("resume-30"));
        QTRY_COMPARE(app.backend->config().value("resumeDays").toInt(), 30);
        QTRY_VERIFY(app.item("resume-30")->property("current").toBool());
        QVERIFY(app.item("set-resume-note")->property("text").toString().contains("30 days"));
        QCOMPARE(app.item("set-resume-note")->property("text").toString(), pages().value("resume").toString());
        QVERIFY(!app.backend->resume().isEmpty());
        app.click(app.item("resume-0"));
        QTRY_COMPARE(app.backend->config().value("resumeDays").toInt(), 0);
        QTRY_VERIFY(app.backend->resume().isEmpty());
        QVERIFY(app.item("set-resume-note")->property("text").toString().startsWith("Off:"));

        // History, quality, thumbnails and the route.
        QCOMPARE(app.item("set-history-note")->property("text").toString(), pages().value("history_off").toString());
        app.click(app.item("history-on"));
        QTRY_VERIFY(app.backend->config().value("history").toBool());
        QTRY_COMPARE(app.item("set-history-note")->property("text").toString(), pages().value("history_on").toString());
        app.click(app.item("settings-cat-playback"));
        app.click(app.item("quality-720"));
        QTRY_COMPARE(app.backend->config().value("maxHeight").toInt(), 720);
        app.click(app.item("settings-cat-home"));
        app.click(app.item("thumbs-clear"));
        QCOMPARE(app.window->property("thumbs").toString(), QString("clear"));
        app.click(app.item("thumbs-off"));
        QTRY_VERIFY(!app.backend->config().value("thumbnails").toBool());
        app.click(app.item("thumbs-frosted"));
        QTRY_VERIFY(app.backend->config().value("thumbnails").toBool());
        QCOMPARE(app.window->property("thumbs").toString(), QString("frosted"));
        app.click(app.item("settings-cat-privacy"));
        app.click(app.item("route-tor"));
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("tor"));
        QTRY_COMPARE(app.item("nav-route")->property("label").toString(), QString("Tor"));
        QCOMPARE(app.item("route-note")->property("text").toString(),
                 app.backend->privacy().value("routes").toMap().value("tor").toString());
    }

    void unsubscribingAndClearingAskOnce()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_3);
        QTRY_COMPARE(app.items("channel-row").size(), 5);
        QQuickItem *unsubscribe = App::under(app.items("channel-row").first(), "unsubscribe");
        QVERIFY(unsubscribe);
        app.hover(app.items("channel-row").first());
        app.click(unsubscribe);
        QCOMPARE(unsubscribe->property("text").toString(), QString("Unsubscribe?"));
        QCOMPARE(app.asked("POST api/subs/rm"), 0);
        app.click(unsubscribe);
        QTRY_COMPARE(app.items("channel-row").size(), 4);
        QCOMPARE(app.asked("POST api/subs/rm"), 1);
        // The line about what subscriptions are is the engine's.
        QCOMPARE(app.item("channels-note")->property("text").toString(),
                 app.backend->privacy().value("pages").toMap().value("subscriptions").toString());

        // History: off says so in the engine's words, and offers to turn it on.
        QTest::keyClick(app.window, Qt::Key_4);
        QCOMPARE(app.page(), QString("history"));
        QCOMPARE(app.item("history-note")->property("text").toString(),
                 app.backend->privacy().value("pages").toMap().value("history_off").toString());
        app.click(app.item("history-turn-on"));
        QTRY_COMPARE(app.items("history-row").size(), 4);
        QQuickItem *clear = app.item("clear-history");
        app.click(clear);
        QCOMPARE(clear->property("text").toString(), QString("Clear history?"));
        QCOMPARE(app.asked("POST api/history/clear"), 0);
        app.click(clear);
        QTRY_COMPARE(app.asked("POST api/history/clear"), 1);
    }

    void historyWhileOffShowsWhatIsKeptWithPicturesAndKeys()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTest::keyClick(app.window, Qt::Key_4);
        QTRY_COMPARE(app.page(), QString("history"));
        QTRY_COMPARE(app.items("history-kept").size(), 3);
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        // The history's sentence and its switch share one line; what is kept
        // has a heading of its own, with its own sentence under it.
        QQuickItem *note = app.item("history-note"), *turnOn = app.item("history-turn-on");
        QVERIFY2(qAbs(box(note).center().y() - box(turnOn).center().y()) < 4,
                 qPrintable(QString("%1 vs %2").arg(box(note).center().y()).arg(box(turnOn).center().y())));
        QVERIFY(box(turnOn).left() > box(note).left());
        QQuickItem *heading = app.item("history-kept-head");
        QVERIFY(heading && heading->isVisible());
        QVERIFY(box(heading).top() > box(note).bottom() && box(app.item("history-kept-note")).top() > box(heading).top());
        // Each row has its picture, and the keyboard is on the first.
        QQuickItem *first = app.items("history-kept").first();
        QVERIFY(App::under(first, "history-kept-picture"));
        QQuickItem *kept = app.item("history-kept-list");
        QTRY_VERIFY(kept->hasActiveFocus());
        QCOMPARE(kept->property("currentIndex").toInt(), 0);
        QVERIFY(App::under(first, "cursor")->isVisible());
        QVERIFY2(box(app.items("history-kept").at(1)).bottom() <= 508, qPrintable(QString::number(box(app.items("history-kept").at(1)).bottom())));
        // j and k move; Delete forgets the one the keyboard is on; Enter plays.
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(kept->property("currentIndex").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_Delete);
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 1);
        QTRY_COMPARE(app.items("history-kept").size(), 2);
        QTest::keyClick(app.window, Qt::Key_K);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000001"));
    }

    void everyPageLoadsCleanInBothDirections()
    {
        for (const Qt::LayoutDirection direction : {Qt::LeftToRight, Qt::RightToLeft}) {
            App app;
            READY(app);
            // (After the window is up: the translator sets the direction of
            // its language when it is made.)
            QGuiApplication::setLayoutDirection(direction);
            const auto check = [&](const QString &where) {
                QTest::qWait(250);
                QStringList said;
                App::texts(app.window->contentItem(), said);
                QVERIFY2(said.size() > 3, qPrintable(where));
                for (const QString &text : said)
                    QVERIFY2(!text.contains("undefined") && !text.contains("NaN"), qPrintable(where + ": " + text));
            };
            for (const char *page : {"home", "lenses", "channels", "history", "settings"}) {
                QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString(page)));
                QCOMPARE(app.page(), QString(page));
                check(page);
            }
            QMetaObject::invokeMethod(app.window, "openChannel", Q_ARG(QVariant, QString("UCaaaaaaaaaaaaaaaaaaaaaa")));
            QCOMPARE(app.page(), QString("channel"));
            check("channel");
            QMetaObject::invokeMethod(app.item("search"), "edit");
            app.type("rust");
            QTest::keyClick(app.window, Qt::Key_Return);
            check("search");
            app.play({{"id", "vid00000002"}});
            QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
            check("watch");
            QTest::keyClick(app.window, Qt::Key_C);
            QTRY_COMPARE(app.items("comment").size(), 5);
            check("comments");
            // The bar starts on the side reading starts from.
            const qreal markX = app.item("wordmark")->mapToScene(QPointF(0, 0)).x();
            QVERIFY2(direction == Qt::LeftToRight ? markX < 100 : markX > 1100, qPrintable(QString::number(markX)));
        }
        QGuiApplication::setLayoutDirection(Qt::LeftToRight);
    }

    void noMaterialIsLeft()
    {
        QDirIterator files(":/", {"*.qml"}, QDir::Files, QDirIterator::Subdirectories);
        int read = 0;
        while (files.hasNext()) {
            const QString name = files.next();
            if (name.startsWith(":/qt") || name.startsWith(":/qt-project.org"))
                continue;
            QFile file(name);
            QVERIFY(file.open(QIODevice::ReadOnly));
            const QString text = QString::fromUtf8(file.readAll());
            ++read;
            QVERIFY2(!text.contains("Controls.Material"), qPrintable(name));
            QVERIFY2(!text.contains("Material."), qPrintable(name));
        }
        QVERIFY2(read > 30, qPrintable(QString::number(read)));
        QCOMPARE(QQuickStyle::name(), QString("Basic"));
    }

    void theHelpListsEveryKeyTheWindowBinds()
    {
        App app;
        READY(app);
        QFile main(":/Main.qml");
        QVERIFY(main.open(QIODevice::ReadOnly));
        const QString source = QString::fromUtf8(main.readAll());
        QStringList bound;
        QRegularExpressionMatchIterator one = QRegularExpression("sequence: \"([^\"]+)\"").globalMatch(source);
        while (one.hasNext())
            bound << one.next().captured(1);
        QRegularExpressionMatchIterator many = QRegularExpression("sequences: \\[([^\\]]+)\\]").globalMatch(source);
        while (many.hasNext())
            for (const QString &key : many.next().captured(1).split(","))
                bound << key.trimmed().remove('"');
        QVERIFY2(bound.size() > 20, qPrintable(bound.join(" ")));
        QObject *help = app.object("help");
        QVERIFY(help);
        const QString listed = " " + help->property("listed").toStringList().join(" ") + " ";
        for (const QString &key : bound)
            QVERIFY2(listed.contains(" " + key + " "), qPrintable("the help doesn't list " + key + "; it lists:" + listed));
        // And it opens and closes.
        QTest::keyClick(app.window, '?');
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!help->property("opened").toBool());
    }

    // --- Home as a lead and shelves. ---

    void homeLeadsWithTheVideoYouWereInAndShelvesFollow()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *lead = app.item("lead");
        QVERIFY(lead);
        QTRY_VERIFY(lead->isVisible());
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
        QVERIFY(app.item("lead-meta")->property("text").toString().contains("Low Level Hours"));
        QVERIFY(app.item("lead-meta")->property("text").toString().contains("26:58 left"));
        QVERIFY(app.item("lead-kept")->property("text").toString().contains("7 days"));
        // The grid and its tabs are a step away, not what the page opens on.
        QVERIFY(!app.item("home-grid")->isVisible());

        // The other unfinished videos, the channels' latest, and each lens, as shelves.
        QTRY_VERIFY(app.item("shelf-resume") && app.item("shelf-latest") && app.item("shelf-aaaa0001") && app.item("shelf-aaaa0002"));
        QTRY_COMPARE(App::count(app.item("shelf-resume"), "card"), 2);
        QTRY_VERIFY(App::count(app.item("shelf-latest"), "card") >= 4);
        QCOMPARE(App::under(app.item("shelf-aaaa0002"), "shelf-title")->property("text").toString(),
                 QString("systems talks over 30 min, this year"));
        QTRY_VERIFY(App::count(app.item("shelf-aaaa0002"), "card") >= 4);
        // A lens's shelf is what the engine already has for it: showing it searches for nothing.
        QCOMPARE(app.asked("GET api/lens?id=aaaa0001&refresh=0"), 1);
        QCOMPARE(app.asked("GET api/lens?id=aaaa0002&refresh=0"), 1);
        for (const QString &line : app.asked().filter("api/lens"))
            QVERIFY2(!line.contains("refresh=1") && !line.contains("force=1"), qPrintable(line));
        QCOMPARE(app.asked("GET api/search"), 0);
        // Its cards say why they are there.
        QVERIFY(App::under(app.item("shelf-aaaa0002"), "card-why"));
    }

    void theLeadResumesStartsOverAndForgets()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        app.click(app.item("lead-resume"));
        QQuickItem *surface = app.item("surface");
        QTRY_COMPARE(surface->property("startedAt").toInt(), 845);
        QCOMPARE(surface->property("info").toMap().value("id").toString(), QString("vid00000001"));

        // Starting over plays the video from its start. The place is not forgotten
        // until the video has begun (nothing plays here, so it stays: see below).
        // (Put away first: left while playing, it would go on in the small window.)
        QMetaObject::invokeMethod(app.item("watch"), "close");
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        QTRY_VERIFY(app.item("lead")->isVisible());
        // Coming back, the page is where it was left: the lead is still in view.
        QTest::qWait(100);
        QVERIFY2(app.item("lead")->mapToScene(QPointF(0, 0)).y() > 0, "the lead was scrolled away");
        app.click(app.item("lead-over"));
        QTRY_VERIFY(surface->property("active").toBool() && !surface->property("pending").toBool());
        QCOMPARE(surface->property("info").toMap().value("id").toString(), QString("vid00000001"));
        QCOMPARE(surface->property("startedAt").toInt(), 0);
        QCOMPARE(app.asked("POST api/resume/rm"), 0);
        QCOMPARE(app.backend->resume().first().toMap().value("id").toString(), QString("vid00000001"));

        // The next unfinished video leads when this one is forgotten, where it stands.
        app.backend->forgetResume("vid00000001");
        QMetaObject::invokeMethod(app.item("watch"), "close");
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        app.click(app.item("lead-forget"));
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 2);
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Glaze tests: 36 tiles, one kiln load"));
    }

    void forgettingTheLeadCanBeUndone()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *toast = app.item("toast"), *undo = app.item("toast-undo");
        QVERIFY(toast && undo);
        QVERIFY(!undo->isVisible());
        app.click(app.item("lead-forget"));
        // The place is gone, and the line that says so offers it back.
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 1);
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        QTRY_VERIFY(toast->isVisible() && undo->isVisible());
        QCOMPARE(app.asked().count("POST api/resume"), 0);

        app.click(undo);
        // Noted again where it was, and it leads again.
        QTRY_COMPARE(app.asked().count("POST api/resume"), 1);
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
        QCOMPARE(app.backend->resume().first().toMap().value("pos").toInt(), 845);
        QCOMPARE(app.backend->resume().first().toMap().value("dur").toInt(), 2463);
        QTRY_VERIFY(!undo->isVisible());
    }

    void theOfferToUndoGoesWhenAnotherLineTakesTheToast()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *undo = app.item("toast-undo");
        app.click(app.item("lead-forget"));
        QTRY_VERIFY(undo->isVisible());
        QMetaObject::invokeMethod(app.item("toast"), "show", Q_ARG(QVariant, QString("Link copied")));
        QVERIFY(!undo->isVisible());
        QCOMPARE(app.asked().count("POST api/resume"), 0);
    }

    void startingOverForgetsThePlaceOnceTheVideoHasBegun()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        app.click(app.item("lead-over"));
        QQuickItem *surface = app.item("surface");
        QObject *mpv = surface->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mpv && mpv->property("started").toBool(), 15000);
        QCOMPARE(surface->property("startedAt").toInt(), 0);
        QCOMPARE(mpv->property("position").toDouble() < 10, true);
        // It began, so the place is forgotten, once.
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 1);
        QTRY_VERIFY(!app.backend->resume().first().toMap().value("id").toString().isEmpty()
                    && app.backend->resume().first().toMap().value("id").toString() != "vid00000001");
        QTest::qWait(300);
        QCOMPARE(app.asked("POST api/resume/rm"), 1);
    }

    void aStartOverThatDoesNotOpenKeepsThePlace()
    {
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_PLAY_FAILS", "vid00000001"}});
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QSignalSpy failed(app.backend.get(), &Backend::taskFailed);
        app.click(app.item("lead-over"));
        QTRY_COMPARE(failed.count(), 1);
        QTRY_VERIFY(!app.item("surface")->property("active").toBool());
        QCOMPARE(app.asked("POST api/resume/rm"), 0);
        QCOMPARE(app.backend->resume().size(), 3);
        // Still the lead, with its place.
        QCOMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
    }

    void resumingPlaysWithTheUnfinishedVideosToFollow()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        app.click(app.item("lead-resume"));
        QTRY_VERIFY(app.item("surface")->property("active").toBool());
        // What comes next is the rest of what was left unfinished (not "nothing follows").
        QTRY_COMPARE(app.items("next-entry").size(), 2);
        QCOMPARE(app.items("next-entry").at(0)->property("title").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        QCOMPARE(app.items("next-entry").at(1)->property("title").toString(), QString("Glaze tests: 36 tiles, one kiln load"));
        // N goes on to it.
        QTest::keyClick(app.window, Qt::Key_N);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000005"));
        QTRY_COMPARE(app.items("next-entry").size(), 1);
    }

    void resumingTheLastUnfinishedVideoFollowsWithTheLatest()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        app.backend->forgetResume("vid00000005");
        app.backend->forgetResume("vid00000004");
        QTRY_COMPARE(app.backend->resume().size(), 1);
        app.click(app.item("lead-resume"));
        QTRY_VERIFY(app.item("surface")->property("active").toBool());
        // The feed, newest first, without the video that is playing.
        QTRY_VERIFY(app.items("next-entry").size() >= 2);
        QCOMPARE(app.items("next-entry").at(0)->property("title").toString(), QString("A dovetail box with four hand tools"));
        QCOMPARE(app.items("next-entry").at(1)->property("title").toString(), QString("The 1970s synth that refuses to stay in tune"));
    }

    void historyPlaysWithTheListItWasClickedIn()
    {
        {
            // On: the history, from the video clicked.
            App app;
            READY(app);
            app.backend->setConfig({{"history", true}});
            QTRY_VERIFY(app.backend->config().value("history").toBool());
            QTest::keyClick(app.window, Qt::Key_4);
            QTRY_COMPARE(app.items("history-row").size(), 4);
            app.click(app.items("history-row").at(1));
            QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000002"));
            QTRY_COMPARE(app.items("next-entry").size(), 2);
            QCOMPARE(app.items("next-entry").first()->property("title").toString(), QString("The 1970s synth that refuses to stay in tune"));
        }
        {
            // Off: what is kept unfinished, from the video clicked.
            App app;
            READY(app);
            QTest::keyClick(app.window, Qt::Key_4);
            QTRY_COMPARE(app.items("history-kept").size(), 3);
            app.click(app.items("history-kept").at(1));
            QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000005"));
            QTRY_COMPARE(app.items("next-entry").size(), 1);
            QCOMPARE(app.items("next-entry").first()->property("title").toString(), QString("Glaze tests: 36 tiles, one kiln load"));
        }
    }

    void aShelfOpensItsFeedInFullAndEscapeReturns()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *shelves = app.item("shelves"), *grid = app.item("home-grid");
        // (Once the lead is there, the shelves are where they will stay.)
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && app.item("shelf-aaaa0002"));
        app.click(App::under(app.item("shelf-latest"), "shelf-all"));
        QTRY_VERIFY(grid->isVisible());
        QVERIFY(!shelves->isVisible());
        QTRY_COMPARE(grid->property("count").toInt(), 12);
        QVERIFY(app.item("feed-tab-latest")->property("current").toBool());
        QTRY_VERIFY(grid->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(shelves->isVisible());
        QVERIFY(!grid->isVisible());
        QCOMPARE(app.page(), QString("home"));

        // A lens's shelf opens that lens.
        app.click(App::under(app.item("shelf-aaaa0002"), "shelf-title"));
        QTRY_VERIFY(grid->isVisible());
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0002"));
        QVERIFY(app.item("feed-tab-aaaa0002")->property("current").toBool());
        QTRY_COMPARE(grid->property("count").toInt(), 6);
        // Home, by its key or the bar, is the shelves again.
        QTest::keyClick(app.window, Qt::Key_1);
        QTRY_VERIFY(shelves->isVisible());
    }

    void arrowsMoveAlongAShelfAndBetweenShelves()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 4);
        QTRY_VERIFY(home->hasActiveFocus() || app.item("shelves")->hasActiveFocus());
        // (Off the lead, onto the first shelf.)
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!app.item("shelves-home")->property("onLead").toBool());
        QCOMPARE(home->property("row").toInt(), 0);
        // The first shelf is the latest; the next is the other unfinished videos.
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_COMPARE(home->property("row").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(home->property("column").toInt(), 1);
        // Each shelf remembers where the keyboard was on it.
        QTest::keyClick(app.window, Qt::Key_Up);
        QTRY_COMPARE(home->property("row").toInt(), 0);
        QCOMPARE(home->property("column").toInt(), 0);
        QTest::keyClick(app.window, Qt::Key_Right);
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(home->property("column").toInt(), 2);
        // What is selected is marked, and Enter plays it, with the shelf as what comes next.
        QQuickItem *selected = nullptr;
        for (QQuickItem *card : app.items("card"))
            if (card->property("selected").toBool())
                selected = card;
        QVERIFY(selected);
        QCOMPARE(selected->property("videoId").toString(), QString("vid00000004"));
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000004"));
        QTRY_VERIFY(!app.items("next-entry").isEmpty());
        // Up has an end, and so has Left.
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QCOMPARE(home->property("row").toInt(), 0);
        for (int i = 0; i < 4; ++i)
            QTest::keyClick(app.window, Qt::Key_Left);
        QCOMPARE(home->property("column").toInt(), 0);
    }

    void withNothingKeptHomeStartsWithTheLatest()
    {
        App app(true);
        READY(app);
        QTRY_VERIFY(app.item("shelf-latest"));
        QVERIFY(!app.item("lead")->isVisible());
        QVERIFY(!app.item("shelf-resume"));
        QTRY_VERIFY(App::count(app.item("shelf-latest"), "card") >= 4);
        QCOMPARE(app.item("shelves-home")->property("row").toInt(), 0);
    }

    void shelvesBelowTheFoldAskForNothing()
    {
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_LENSES", "12"}});
        READY(app);
        QTRY_VERIFY(app.item("shelf-resume"));
        QTest::qWait(400);
        // Twelve lenses are twelve shelves, but only the ones in view are made and filled.
        QVERIFY(!app.item("shelf-aaaa0012"));
        QCOMPARE(app.asked("GET api/lens?id=aaaa0012"), 0);
        QMetaObject::invokeMethod(app.item("shelves"), "positionViewAtEnd");
        QTRY_VERIFY(app.item("shelf-aaaa0012"));
        QTRY_COMPARE(app.asked("GET api/lens?id=aaaa0012&refresh=0"), 1);
    }

    void settingsAreRowsUnderCategoriesDownTheSide()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_5);
        QQuickItem *page = app.item("settings");
        QVERIFY(page);
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        // The categories, down the starting side; the first is showing.
        qreal lastY = -1;
        for (const char *name : {"settings-cat-privacy", "settings-cat-playback", "settings-cat-home", "settings-cat-about"}) {
            QQuickItem *cat = App::under(page, name);
            QVERIFY2(cat && cat->isVisible(), name);
            QVERIFY(box(cat).left() < 60 && box(cat).top() > lastY);
            lastY = box(cat).top();
        }
        QVERIFY(App::under(page, "settings-cat-privacy")->property("current").toBool());
        QCOMPARE(page->property("category").toString(), QString("privacy"));

        // Which rows a category has, and a control of each.
        const QList<std::tuple<QString, QString, QString>> rows{
            {"privacy", "set-route", "route-tor"},     {"privacy", "set-history", "history-on"},
            {"privacy", "set-resume", "resume-7"},     {"playback", "set-quality", "quality-1080"},
            {"home", "set-thumbs", "thumbs-clear"},    {"home", "set-shorts", "shorts-hidden"}};
        qreal left = -1, right = -1;
        for (const auto &[category, rowName, controlName] : rows) {
            app.click(App::under(page, qPrintable("settings-cat-" + category)));
            QCOMPARE(page->property("category").toString(), category);
            QQuickItem *row = App::under(page, rowName);
            QVERIFY2(row && row->isVisible(), qPrintable(rowName));
            QQuickItem *name = App::under(row, "setting-name"), *control = App::under(row, controlName);
            QVERIFY2(name && control && control->isVisible(), qPrintable(rowName));
            QVERIFY(!name->property("text").toString().isEmpty());
            const QRectF n = box(name), c = box(control), r = box(row);
            // The control is beside the name, on the row, towards its far end.
            QVERIFY2(c.left() > n.right(), qPrintable(rowName));
            QVERIFY2(r.contains(c) && r.contains(n), qPrintable(rowName));
            QVERIFY2(c.center().x() > r.center().x(), qPrintable(rowName));
            // The rows share their edges, beside the categories, and are no wider than reads well.
            if (left < 0) {
                left = r.left();
                right = r.right();
            }
            QCOMPARE(r.left(), left);
            QCOMPARE(r.right(), right);
            QVERIFY2(r.width() <= 780 && r.left() > box(App::under(page, "settings-cat-privacy")).right(), qPrintable(rowName));
            // Only the rows of the category that is showing are showing.
            for (const auto &[other, otherRow, unused] : rows)
                QCOMPARE(App::under(page, otherRow)->isVisible(), other == category);
        }
        // The route's row says what the engine says of the route chosen.
        app.click(App::under(page, "settings-cat-privacy"));
        QCOMPARE(App::under(App::under(page, "set-route"), "route-note")->property("text").toString(),
                 app.backend->privacy().value("routes").toMap().value("direct").toString());
        // About has the engine's own account, in full.
        QCOMPARE(App::count(page, "privacy-point"), 0);
        app.click(App::under(page, "settings-cat-about"));
        QTRY_COMPARE(App::count(page, "privacy-point"), int(app.backend->privacy().value("points").toList().size()));
        // The arrow keys step through the categories.
        QTest::keyClick(app.window, Qt::Key_Up);
        QTRY_COMPARE(page->property("category").toString(), QString("home"));
        QTest::keyClick(app.window, Qt::Key_Down);
        QTest::keyClick(app.window, Qt::Key_Down);
        QCOMPARE(page->property("category").toString(), QString("about"));
    }

    void typeIsTheSystemsAndPicturesAreClearUnlessAskedOtherwise()
    {
        App app;
        READY(app);
        const QString family = app.theme->property("uiFamily").toString();
        QVERIFY(!family.isEmpty());
        QTRY_VERIFY(app.item("lead")->isVisible());
        QCOMPARE(app.item("lead-title")->property("font").value<QFont>().family(), family);
        // Nothing is bold by smearing: titles use a weight the face has.
        QCOMPARE(app.item("lead-title")->property("font").value<QFont>().weight(), QFont::DemiBold);

        QCOMPARE(app.window->property("thumbs").toString(), QString("clear"));
        QTRY_VERIFY(app.item("shelf-resume") && App::under(app.item("shelf-resume"), "frost"));
        QVERIFY(!App::under(app.item("shelf-resume"), "frost")->property("veiled").toBool());
        // Frosted is still there for whoever wants it.
        QTest::keyClick(app.window, Qt::Key_5);
        app.click(app.item("settings-cat-home"));
        app.click(app.item("thumbs-frosted"));
        QCOMPARE(app.window->property("thumbs").toString(), QString("frosted"));
        QTest::keyClick(app.window, Qt::Key_1);
        QTRY_VERIFY(App::under(app.item("shelf-resume"), "frost")->property("veiled").toBool());
    }

    void upNextShowsPicturesAndLeadsTheSheetWhenThereIsAList()
    {
        App app;
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QCOMPARE(app.item("sheet")->property("current").toString(), QString("next"));
        QTRY_VERIFY(!app.items("next-entry").isEmpty());
        QVERIFY(App::under(app.items("next-entry").first(), "frost"));
        // Without a list there is nothing to come next, and the sheet opens on what the video is.
        app.play({{"id", "vid00000004"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QCOMPARE(app.item("sheet")->property("current").toString(), QString("next"));
    }

    // --- Fluid: it follows its tile, moves briefly, loads without jumps, and its shelves glide. ---

    void theSizeFollowsTheTileWithoutAJump()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        for (const auto &[width, zoom] : {std::pair{1280, 1.0}, std::pair{1440, 1.125}, std::pair{1600, 1.25}, std::pair{1920, 1.5}, std::pair{2560, 1.5}}) {
            App app(false, QSize(width, 900));
            READY(app);
            QCOMPARE(app.window->property("zoom").toReal(), zoom);
            QCOMPARE(box(app.item("bar")).height(), 56 * zoom);
            QCOMPARE(box(app.item("bar")).width(), qreal(width));
        }
    }

    void aShortTileHasTheLeadAsAStripAndAWholeShelfUnderIt()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QQuickItem *lead = app.item("lead");
        QTRY_VERIFY(lead->isVisible());
        QTRY_VERIFY2(box(lead).height() <= 120, qPrintable(QString::number(box(lead).height())));
        // It still says what it is and how much is left, and pressing it resumes.
        QCOMPARE(app.item("lead-title")->property("text").toString(), QString("Why the borrow checker says no"));
        QVERIFY(app.item("lead-title")->isVisible() && app.item("lead-meta")->isVisible());
        // Its three actions are words in the strip, between what it is and its picture.
        for (const char *name : {"lead-resume", "lead-over", "lead-forget"}) {
            QQuickItem *act = app.item(name);
            QVERIFY2(act->isVisible(), name);
            QVERIFY2(box(lead).contains(box(act)), qPrintable(QString("%1 %2,%3 %4x%5 in %6,%7 %8x%9").arg(name).arg(box(act).x()).arg(box(act).y()).arg(box(act).width()).arg(box(act).height()).arg(box(lead).x()).arg(box(lead).y()).arg(box(lead).width()).arg(box(lead).height())));
            QVERIFY2(box(act).left() >= box(app.item("lead-meta")).right() - 1, name);
            QVERIFY2(box(act).right() <= box(app.item("lead-picture")).left() + 1, name);
        }
        // Left and right go along them, and Enter does what is marked; the next
        // video to lead starts on Resume again.
        QTRY_VERIFY(app.item("shelves-home")->property("leadSelected").toBool());
        QVERIFY(app.item("lead-resume")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_VERIFY(app.item("lead-forget")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 1);
        QTRY_COMPARE(app.item("lead-title")->property("text").toString(), QString("Crossing the Pamir on a steel bike, part 3"));
        QCOMPARE(app.item("shelves-home")->property("leadAction").toInt(), 0);
        // A whole row of the latest fits under it.
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        QTRY_VERIFY(App::count(shelf, "card") >= 3);
        QQuickItem *card = App::under(shelf, "card");
        QTRY_VERIFY2(box(card).bottom() <= 508, qPrintable(QString::number(box(card).bottom())));
        app.click(app.item("lead-title"));
        QTRY_VERIFY(app.page() == QString("watch") || app.window->property("watching").toBool());

        // In a tile with room, the lead is the lead.
        App tall(false, QSize(941, 1030));
        READY(tall);
        QTRY_VERIFY(tall.item("lead")->isVisible());
        QTRY_VERIFY(box(tall.item("lead")).height() >= 200);
        QVERIFY(tall.item("lead-over")->isVisible());
    }

    void aNarrowColumnFoldsTheSearchFieldIntoItsKey()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(630, 1030));
        READY(app);
        QQuickItem *search = app.item("search");
        QVERIFY2(box(search).width() <= 60, qPrintable(QString::number(box(search).width())));
        // Nothing lies over anything else, and nothing is past the edge.
        QVERIFY(box(app.item("nav-history")).right() <= box(search).left());
        QVERIFY(box(search).right() <= box(app.item("nav-route")).left());
        QVERIFY(box(app.item("nav-settings")).right() <= 630);
        // Its key, or a press, opens it over the bar; Escape folds it again.
        QTest::keyClick(app.window, '/');
        QTRY_VERIFY(app.window->property("typing").toBool());
        QTRY_VERIFY2(box(search).width() >= 300, qPrintable(QString::number(box(search).width())));
        QVERIFY(box(search).left() >= 0 && box(search).right() <= 630);
        app.type("rust");
        QCOMPARE(search->property("text").toString(), QString("rust"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.window->property("typing").toBool());
        QTRY_VERIFY(box(search).width() <= 60);
        // The lead is its words: there is no room for its picture beside them.
        QTRY_VERIFY(app.item("lead")->isVisible());
        QVERIFY(!app.item("lead-picture")->isVisible());
        QVERIFY(box(app.item("lead-forget")).right() <= box(app.item("lead")).right());
        QVERIFY(app.item("lead-title")->width() >= 400);
        // Two whole cards across.
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        QCOMPARE(shelf->property("across").toInt(), 2);
    }

    void hjklMoveLikeTheArrows()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 4);
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        // (Off the lead, onto the first shelf.)
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!app.item("shelves-home")->property("onLead").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(home->property("column").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(home->property("row").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_K);
        QTRY_COMPARE(home->property("row").toInt(), 0);
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_COMPARE(home->property("column").toInt(), 0);
        // In a grid.
        app.seeAll();
        QQuickItem *grid = app.item("home-grid");
        QTRY_VERIFY(grid->hasActiveFocus());
        grid->setProperty("currentIndex", 0);
        const int columns = grid->property("columns").toInt();
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 1 + columns);
        QTest::keyClick(app.window, Qt::Key_K);
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 0);
        // In a list.
        QTest::keyClick(app.window, Qt::Key_3);
        QQuickItem *channels = nullptr;
        QTRY_VERIFY((channels = app.item("channel-list")) && channels->hasActiveFocus());
        QTRY_COMPARE(app.items("channel-row").size(), 5);
        channels->setProperty("currentIndex", 0);
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(channels->property("currentIndex").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_K);
        QTRY_COMPARE(channels->property("currentIndex").toInt(), 0);
        // While a video has the page, J and L are still ten seconds back and on.
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(app.window->property("watching").toBool());
        QTest::keyClick(app.window, Qt::Key_J);
        QCOMPARE(app.page(), QString("channels"));
    }

    void aShelfGlidesByARowWithItsArrows()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        QTRY_VERIFY(App::count(shelf, "card") >= 5);
        QQuickItem *row = App::under(shelf, "shelf-row"), *next = App::under(shelf, "shelf-next"), *prev = App::under(shelf, "shelf-prev");
        QVERIFY(row && next && prev);
        // The arrows are there when the shelf is pointed at, and only where there is more.
        QVERIFY(!next->isVisible() && !prev->isVisible());
        app.hover(row);
        QTRY_VERIFY(next->isVisible());
        QVERIFY(!prev->isVisible());
        const qreal cell = row->property("cellWidth").toReal();
        app.click(next);
        // It moves there, not jumps: part-way first, then a whole row on.
        QTRY_VERIFY(row->property("contentX").toReal() > 0);
        QVERIFY(row->property("contentX").toReal() < 4 * cell);
        QTRY_COMPARE(row->property("contentX").toReal(), 4 * cell);
        QTRY_VERIFY(prev->isVisible());
        // Whole cards still: the first one showing starts where the row starts.
        QList<QQuickItem *> cards;
        App::collect(shelf, "card", cards);
        qreal first = 1e9;
        for (QQuickItem *card : cards)
            if (box(card).right() > box(row).left() + 1)
                first = qMin(first, box(card).left());
        QVERIFY2(qAbs(first - box(row).left()) <= 1, qPrintable(QString::number(first - box(row).left())));
        app.click(next);
        // (To its end: eleven cards, the lead's video being above it.)
        QTRY_COMPARE(row->property("contentX").toReal(), 7 * cell);
        // At the end there is no more that way.
        QTRY_VERIFY(!next->isVisible());
        app.click(prev);
        QTRY_COMPARE(row->property("contentX").toReal(), 3 * cell);
        // A shelf whose videos all fit has no arrows.
        QQuickItem *few = app.item("shelf-resume");
        app.hover(App::under(few, "shelf-row"));
        QTest::qWait(250);
        QVERIFY(!App::under(few, "shelf-next")->isVisible());
    }

    void aShelfHoldsItsPlaceWhileItLoads()
    {
        App app(true, QSize(1280, 720), {{"QVEIL_STANDIN_SLOW_FEED", "1.5"}});
        READY(app);
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        // The places of what is coming are there, as many as fit across, and no words about nothing.
        QTRY_COMPARE(App::count(shelf, "card-placeholder"), shelf->property("across").toInt() * shelf->property("rows").toInt());
        QCOMPARE(App::count(shelf, "card"), 0);
        // (Once the page has arrived: it settles into place over a moment.)
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        const qreal height = shelf->height();
        QQuickItem *more = app.item("home-new-lens");
        const qreal below = more ? more->mapToScene(QPointF(0, 0)).y() : -1;
        // When they come, they take those places: nothing under them moves.
        QTRY_VERIFY_WITH_TIMEOUT(App::count(shelf, "card") >= 4, 6000);
        QTRY_COMPARE(App::count(shelf, "card-placeholder"), 0);
        QCOMPARE(shelf->height(), height);
        if (more)
            QCOMPARE(more->mapToScene(QPointF(0, 0)).y(), below);
    }

    void movingBetweenPagesIsBriefAndCanBeTurnedOff()
    {
        App app;
        READY(app);
        QQuickItem *stage = app.item("stage");
        QVERIFY(stage);
        QTRY_COMPARE(stage->opacity(), 1.0);
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("channels")));
        // The page that comes in fades in; the one that left is gone at once.
        QVERIFY(stage->opacity() < 1.0);
        QVERIFY(!app.item("shelves")->isVisible());
        QElapsedTimer timer;
        timer.start();
        QTRY_COMPARE(stage->opacity(), 1.0);
        QVERIFY2(timer.elapsed() < 400, qPrintable(QString::number(timer.elapsed())));

        // With the desktop's animations off, nothing moves.
        app.theme->setProperty("animations", false);
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("history")));
        QCOMPARE(stage->opacity(), 1.0);
    }

    // --- Sorting and filtering; topic feeds. ---

    void aShelfIsSortedAndFilteredWithoutAskingAnybody()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        QTRY_VERIFY(App::count(shelf, "card") >= 4);
        const auto first = [&](QQuickItem *in) {
            // The card furthest towards the start of the first row.
            QQuickItem *best = nullptr;
            QList<QQuickItem *> cards;
            App::collect(in, "card", cards);
            for (QQuickItem *card : cards) {
                const QPointF at = card->mapToScene(QPointF(0, 0));
                if (!best || at.y() < best->mapToScene(QPointF(0, 0)).y() - 1
                    || (qAbs(at.y() - best->mapToScene(QPointF(0, 0)).y()) <= 1 && at.x() < best->mapToScene(QPointF(0, 0)).x()))
                    best = card;
            }
            return best ? best->property("videoId").toString() : QString();
        };
        QCOMPARE(first(shelf), QString("vid00000002"));
        QQuickItem *sort = App::under(shelf, "shelf-sort");
        QVERIFY(sort);
        QCOMPARE(sort->property("text").toString(), QString("Newest"));
        // (Once what the page asks for as it comes up has been asked.)
        QTest::qWait(400);
        const QStringList before = app.asked();

        // The most viewed first.
        app.click(sort);
        QTRY_VERIFY(app.item("sort-views") && app.item("sort-views")->isVisible());
        app.click(app.item("sort-views"));
        QTRY_COMPARE(first(shelf), QString("vid00000002"));
        QVERIFY(sort->property("text").toString().contains("Most viewed"));
        // Only the long ones (over half an hour), still by views.
        app.click(app.item("length-long"));
        QTRY_COMPARE(first(shelf), QString("vid00000005"));
        QVERIFY(sort->property("text").toString().contains("30"));
        // Without what has a kept position (three of them have).
        app.click(app.item("watched-hide"));
        QTRY_COMPARE(first(shelf), QString("vid00000006"));
        QList<QQuickItem *> cards;
        App::collect(shelf, "card", cards);
        for (QQuickItem *card : cards) {
            QVERIFY(card->property("seconds").toInt() > 1800);
            QVERIFY(!QStringList({"vid00000001", "vid00000005", "vid00000004"}).contains(card->property("videoId").toString()));
        }
        // The longest, and the shortest of the long.
        app.click(app.item("sort-longest"));
        QTRY_COMPARE(first(shelf), QString("vid00000006"));
        app.click(app.item("sort-shortest"));
        QTRY_COMPARE(first(shelf), QString("vid00000010"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.item("sort-views") || !app.item("sort-views")->isVisible());
        // None of that asked the engine for anything.
        QCOMPARE(app.asked(), before);

        // The feed in full is the same feed, in the same order, and what
        // comes next after a video is what comes next in that order.
        app.seeAll();
        QQuickItem *grid = app.item("home-grid");
        QTRY_VERIFY(grid->isVisible());
        QTRY_COMPARE(grid->property("count").toInt(), 3);
        QCOMPARE(app.item("feed-sort")->property("text").toString(), sort->property("text").toString());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000010"));
        QTRY_COMPARE(app.items("next-entry").size(), 2);
        QCOMPARE(app.items("next-entry").first()->property("title").toString(), QString("The cheapest bench that works"));

        // Another shelf has a setting of its own.
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        QTRY_VERIFY(app.item("shelf-resume"));
        QCOMPARE(App::under(app.item("shelf-resume"), "shelf-sort")->property("text").toString(), QString("Last watched"));
    }

    void topicFeedsAreOffUntilTurnedOnAndThenAreShelves()
    {
        App app(false, QSize(1280, 2400));
        READY(app);
        QTRY_VERIFY(app.item("shelf-aaaa0002"));
        QTest::qWait(300);
        // Off: no shelves, and nothing is asked for them.
        QVERIFY(!app.item("shelf-topic-aa11bb22cc33"));
        QCOMPARE(app.asked("GET api/topics?refresh"), 0);

        QTest::keyClick(app.window, Qt::Key_5);
        app.click(app.item("settings-cat-home"));
        QQuickItem *row = app.item("set-topics");
        QVERIFY(row && row->isVisible());
        const auto said = [&] { return app.backend->privacy().value("pages").toMap().value("topics").toString(); };
        QVERIFY(said().startsWith("Off"));
        QCOMPARE(app.item("set-topics-note")->property("text").toString(), said());
        QVERIFY(app.item("topics-off")->property("current").toBool());
        app.click(app.item("topics-on"));
        QTRY_VERIFY(app.backend->config().value("topicFeeds").toBool());
        QTRY_VERIFY(said().startsWith("On"));
        QCOMPARE(app.item("set-topics-note")->property("text").toString(), said());
        // Turned on, they are searched for, once.
        QTRY_COMPARE(app.asked("GET api/topics?refresh"), 1);

        QTest::keyClick(app.window, Qt::Key_1);
        QQuickItem *wood = nullptr;
        QTRY_VERIFY((wood = app.item("shelf-topic-aa11bb22cc33")));
        QVERIFY(app.item("shelf-topic-dd44ee55ff66"));
        QCOMPARE(App::under(wood, "shelf-title")->property("text").toString(), QString("Hand plane, dovetail"));
        // Which of your channels it comes from.
        QStringList words;
        App::texts(wood, words);
        QVERIFY2(words.join("|").contains("Northlight Woodworks"), qPrintable(words.join("|")));
        QTRY_VERIFY(App::count(wood, "card") >= 4);
        QVERIFY(App::under(wood, "card-why"));
        // Going home again doesn't search again.
        QTest::keyClick(app.window, Qt::Key_2);
        QTest::keyClick(app.window, Qt::Key_1);
        QTest::qWait(200);
        QCOMPARE(app.asked("GET api/topics?refresh"), 1);

        // Hidden, a topic is gone.
        app.click(App::under(app.item("shelf-topic-dd44ee55ff66"), "topic-hide"));
        QTRY_COMPARE(app.asked("POST api/topics/hide?id=dd44ee55ff66"), 1);
        QTRY_VERIFY(!app.item("shelf-topic-dd44ee55ff66"));
        // Kept as a lens, it is one: its searches are the lens's words.
        // (The shelves were made again when one went: the other is looked for anew.)
        QTRY_VERIFY((wood = app.item("shelf-topic-aa11bb22cc33")));
        app.click(App::under(wood, "topic-keep"));
        QTRY_COMPARE(app.page(), QString("lenses"));
        QTRY_COMPARE(app.item("lens-text")->property("text").toString(), QString("hand plane, dovetail"));

        // Off again: the shelves go.
        QTest::keyClick(app.window, Qt::Key_5);
        app.click(app.item("settings-cat-home"));
        app.click(app.item("topics-off"));
        QTRY_VERIFY(!app.backend->config().value("topicFeeds").toBool());
        QTest::keyClick(app.window, Qt::Key_1);
        QTRY_VERIFY(!app.item("shelf-topic-aa11bb22cc33"));
    }

    // --- The owner's picks from the council's mockups. ---

    void theKeyboardsCursorIsThereFromTheStartOnTheLead()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *home = app.item("shelves-home"), *lead = app.item("lead");
        QTRY_VERIFY(lead->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 4);
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        const auto marked = [&] {
            QStringList ids;
            for (QQuickItem *card : app.items("card"))
                if (card->property("selected").toBool())
                    ids << card->property("videoId").toString();
            return ids;
        };
        // No key has been pressed: the cursor is on the lead, and on nothing else.
        QTRY_VERIFY(home->property("leadSelected").toBool());
        // It is one mark, on the action Enter does: no frame round the whole lead.
        QVERIFY(!app.item("lead-cursor") || !app.item("lead-cursor")->isVisible());
        QVERIFY(app.item("lead-resume")->property("marked").toBool());
        QVERIFY(marked().isEmpty());
        // Down is the first shelf's first card, framed on its picture; Up is the lead again.
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_COMPARE(marked(), QStringList{"vid00000002"});
        QVERIFY(!home->property("leadSelected").toBool());
        QQuickItem *cursor = nullptr;
        for (QQuickItem *card : app.items("card"))
            if (card->property("selected").toBool())
                cursor = App::under(card, "cursor");
        QVERIFY(cursor && cursor->isVisible());
        QCOMPARE(cursor->property("border").value<QObject *>()->property("width").toInt(), 2);
        QCOMPARE(cursor->size(), App::under(cursor->parentItem(), "frost")->size());
        // Under the pointer the cursor steps aside; a key brings it back where it was.
        app.hover(app.items("card").at(1));
        app.hover(app.items("card").at(2));
        QTRY_VERIFY(marked().isEmpty());
        QTest::keyClick(app.window, Qt::Key_K);
        QTRY_VERIFY(home->property("leadSelected").toBool());

        // On the lead, left and right choose what Enter does.
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(home->property("leadAction").toInt(), 1);
        QVERIFY(app.item("lead-over")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_Left);
        QTest::keyClick(app.window, Qt::Key_Left);
        QCOMPARE(home->property("leadAction").toInt(), 0);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("startedAt").toInt(), 845);
        QCOMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000001"));

        // A grid's cursor is there from the start too.
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        app.seeAll();
        QTRY_VERIFY(app.item("home-grid")->hasActiveFocus());
        QTRY_COMPARE(marked(), QStringList{"vid00000001"});
    }

    void inAShortTileEnterOnTheLeadResumes()
    {
        App app(false, QSize(941, 508));
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QTRY_VERIFY(app.item("shelves-home")->property("leadSelected").toBool());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("startedAt").toInt(), 845);
    }

    void inTheQuarterTileTheSheetStandsBesideTheVideo()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QQuickItem *sheet = app.item("sheet"), *surface = app.item("surface");
        QTRY_VERIFY(!surface->property("pending").toBool());
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        // Beside the video, in view, and the picture is no smaller for it.
        QTRY_VERIFY2(box(sheet).left() >= box(surface).right(), qPrintable(QString::number(box(sheet).left())));
        QVERIFY2(box(sheet).right() <= 941 && box(sheet).top() < 120 && box(sheet).bottom() <= 508,
                 qPrintable(QString("%1..%2").arg(box(sheet).top()).arg(box(sheet).bottom())));
        QVERIFY2(box(surface).height() >= 250, qPrintable(QString::number(box(surface).height())));
        // What comes next is what it opens on, with rows in view.
        QCOMPARE(sheet->property("current").toString(), QString("next"));
        QTRY_VERIFY(app.items("next-entry").size() >= 3);
        QVERIFY(box(app.items("next-entry").at(2)).bottom() <= 508);
        // The description is under the video and nowhere else; the sheet has the chapters.
        QVERIFY(app.item("watch-about")->isVisible());
        QVERIFY(!app.item("tab-about"));
        QVERIFY(app.item("tab-chapters")->isVisible());
    }

    void inTheQuarterTileThePictureTakesWhatTheTileHas()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QQuickItem *surface = app.item("surface"), *watch = app.item("watch");
        QTRY_VERIFY(!surface->property("pending").toBool());
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        // Beside the sheet, the picture is as wide as its place, and only the
        // title and the channel's row are kept under it.
        QVERIFY2(box(surface).height() >= 300, qPrintable(QString::number(box(surface).height())));
        QVERIFY(box(app.item("sheet")).left() >= box(surface).right());
        QVERIFY2(box(app.item("watch-stats")).bottom() <= 508, qPrintable(QString::number(box(app.item("watch-stats")).bottom())));
        // In theatre it takes the width as well, and the title stays in view.
        watch->setProperty("theatre", true);
        QTRY_VERIFY2(box(surface).height() >= 360, qPrintable(QString::number(box(surface).height())));
        QVERIFY2(box(app.item("watch-title")).top() < 508 - 20, qPrintable(QString::number(box(app.item("watch-title")).top())));
    }

    void theSheetCanBeUsedFromTheKeyboard()
    {
        App app;
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QQuickItem *sheet = app.item("sheet"), *surface = app.item("surface"), *watch = app.item("watch");
        QTRY_VERIFY(!surface->property("pending").toBool());
        QVERIFY(!watch->property("inSheet").toBool());
        QVERIFY(app.window->property("steering").toBool());

        // C gives the keyboard to the sheet, on the comments (which are fetched now).
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_VERIFY(watch->property("inSheet").toBool());
        QCOMPARE(sheet->property("current").toString(), QString("comments"));
        QTRY_COMPARE(app.items("comment").size(), 5);
        // The keys are the sheet's now, not the video's.
        QVERIFY(!app.window->property("steering").toBool());
        // Left and right (and h and l) change the tab.
        QTest::keyClick(app.window, Qt::Key_H);
        QCOMPARE(sheet->property("current").toString(), QString("next"));
        QTest::keyClick(app.window, Qt::Key_Right);
        QTest::keyClick(app.window, Qt::Key_L);
        QCOMPARE(sheet->property("current").toString(), QString("chapters"));
        // Down and Enter on a chapter go to it.
        QTest::keyClick(app.window, Qt::Key_J);
        QTest::keyClick(app.window, Qt::Key_Down);
        QTest::keyClick(app.window, Qt::Key_Return);
        QCOMPARE(surface->property("lastSeek").toInt(), 760);
        // In what comes next, the row the keyboard is on is marked, and Enter plays it.
        QTest::keyClick(app.window, Qt::Key_H);
        QTest::keyClick(app.window, Qt::Key_H);
        QCOMPARE(sheet->property("current").toString(), QString("next"));
        QTRY_VERIFY(!app.items("next-entry").isEmpty());
        QVERIFY(app.items("next-entry").first()->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_VERIFY(app.items("next-entry").at(1)->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(surface->property("info").toMap().value("id").toString(), QString("vid00000003"));

        // Escape gives the keys back to the video first, and only then leaves it.
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_VERIFY(watch->property("inSheet").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!watch->property("inSheet").toBool());
        QVERIFY(app.window->property("watching").toBool());
        QVERIFY(app.window->property("steering").toBool());
        // N is the next video in the list.
        QTest::keyClick(app.window, Qt::Key_N);
        QTRY_COMPARE(surface->property("info").toMap().value("id").toString(), QString("vid00000004"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.window->property("watching").toBool());
    }

    void theLensesPageIsAStripASentenceAndAGrid()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.items("lens-row").size(), 2);
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        // The lenses are a strip along the top, the open one marked, with the way to a new one.
        const QList<QQuickItem *> lenses = app.items("lens-row");
        QVERIFY(qAbs(box(lenses.at(0)).top() - box(lenses.at(1)).top()) < 1);
        QVERIFY(box(lenses.at(1)).left() >= box(lenses.at(0)).right() - 1);
        QVERIFY(lenses.at(0)->property("current").toBool());
        QVERIFY(app.item("lens-new")->isVisible() && box(app.item("lens-new")).right() <= 941);
        // Under it the lens's own words, and then its videos across the whole width:
        // a whole row of them fits in the tile.
        QQuickItem *grid = app.item("lens-grid");
        QTRY_VERIFY(grid->property("count").toInt() >= 3);
        QVERIFY2(box(grid).left() <= 24 && box(grid).right() >= 917, qPrintable(QString::number(box(grid).left())));
        QQuickItem *card = App::under(grid, "card");
        QTRY_VERIFY(card);
        QTRY_VERIFY2(box(card).bottom() <= 508, qPrintable(QString::number(box(card).bottom())));
        QVERIFY(box(app.item("lens-text")).top() >= box(lenses.at(0)).bottom());
        // The other lens is a press on the strip away.
        app.click(lenses.at(1));
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0002"));
        QTRY_VERIFY(app.items("lens-row").at(1)->property("current").toBool());
        // It has the sort control every list has.
        QVERIFY(app.item("lens-sort") && app.item("lens-sort")->isVisible());
    }

    void channelsAreTwoPanesAndThePaneAsksNobody()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QTest::keyClick(app.window, Qt::Key_3);
        QQuickItem *list = nullptr;
        QTRY_VERIFY((list = app.item("channel-list")) && list->hasActiveFocus());
        QTRY_COMPARE(app.items("channel-row").size(), 5);
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        // Each channel has a picture: its latest video's, which is already here.
        QVERIFY(App::under(app.items("channel-row").first(), "channel-picture"));
        // Beside the list, the latest of the channel the keyboard is on.
        QQuickItem *pane = app.item("channel-pane");
        QVERIFY(pane && box(pane).left() >= box(list).right());
        QCOMPARE(app.item("channel-pane-name")->property("text").toString(), QString("Low Level Hours"));
        QTRY_COMPARE(pane->property("count").toInt(), 3);
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(app.item("channel-pane-name")->property("text").toString(), QString("Northlight Woodworks"));
        QTRY_COMPARE(pane->property("count").toInt(), 3);
        // None of that asked for anything.
        QCOMPARE(app.asked("GET api/channel"), 0);

        // Right goes into the pane, where Enter plays; left at its edge comes back.
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_VERIFY(pane->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.item("surface")->property("info").toMap().value("id").toString(), QString("vid00000002"));
        QTRY_COMPARE(app.items("next-entry").size(), 2);
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.window->property("watching").toBool());
        QTRY_VERIFY(pane->hasActiveFocus() || list->hasActiveFocus());
        pane->forceActiveFocus();
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_VERIFY(list->hasActiveFocus());

        // U asks once, in place, and then lets the channel go.
        QTest::keyClick(app.window, Qt::Key_U);
        QCOMPARE(app.asked("POST api/subs/rm"), 0);
        QQuickItem *current = nullptr;
        for (QQuickItem *row : app.items("channel-row"))
            if (row->property("current").toBool())
                current = row;
        QVERIFY(current);
        QCOMPARE(App::under(current, "unsubscribe")->property("text").toString(), QString("Unsubscribe?"));
        QTest::keyClick(app.window, Qt::Key_U);
        QTRY_COMPARE(app.asked("POST api/subs/rm"), 1);
        QTRY_COMPARE(app.items("channel-row").size(), 4);
        // Enter opens the channel's own page (which does ask).
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.page(), QString("channel"));
    }

    void theSmallCorrectionsFromTheCouncil()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(1280, 1900));
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelf-aaaa0001") && app.item("shelf-aaaa0002"));
        QTRY_VERIFY(App::count(app.item("shelf-aaaa0002"), "card") >= 4);
        QTRY_VERIFY(App::count(app.item("shelf-aaaa0001"), "card") >= 4);
        const auto reasons = [&](const char *shelf) {
            QList<QQuickItem *> found;
            App::collect(app.item(shelf), "card-why", found);
            return found;
        };
        // Why a video is there is said only where it differs from card to card, and quietly;
        // how much of a video is left keeps the accent.
        QVERIFY(reasons("shelf-aaaa0001").isEmpty());
        QVERIFY(!reasons("shelf-aaaa0002").isEmpty());
        const QColor accent(app.theme->accent());
        QVERIFY(reasons("shelf-aaaa0002").first()->property("color").value<QColor>() != accent);
        QVERIFY(!reasons("shelf-resume").isEmpty());
        QCOMPARE(reasons("shelf-resume").first()->property("color").value<QColor>(), accent);
        // The sort control is a quiet word with a mark that it opens.
        QQuickItem *sort = App::under(app.item("shelf-latest"), "shelf-sort");
        QVERIFY(!sort->property("filled").toBool());
        QCOMPARE(sort->property("trailing").toString(), QString("\u25be"));

        // On search results no place is marked in the bar; nor on a channel opened from them.
        QMetaObject::invokeMethod(app.item("search"), "edit");
        app.type("rust");
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.page(), QString("search"));
        for (const char *name : {"nav-home", "nav-lenses", "nav-channels", "nav-history"})
            QVERIFY2(!app.item(name)->property("current").toBool(), name);
        QMetaObject::invokeMethod(app.window, "openChannel", Q_ARG(QVariant, QString("UCaaaaaaaaaaaaaaaaaaaaaa")));
        QVERIFY(!app.item("nav-home")->property("current").toBool());
        // A channel opened from home is still under Home.
        QTest::keyClick(app.window, Qt::Key_1);
        QMetaObject::invokeMethod(app.window, "openChannel", Q_ARG(QVariant, QString("UCaaaaaaaaaaaaaaaaaaaaaa")));
        QVERIFY(app.item("nav-home")->property("current").toBool());

        // With the history off, its page shows what is kept all the same: the unfinished videos.
        QTest::keyClick(app.window, Qt::Key_4);
        QTRY_COMPARE(app.items("history-kept").size(), 3);
        QCOMPARE(app.item("history-kept-note")->property("text").toString(),
                 app.backend->privacy().value("pages").toMap().value("resume").toString());
        app.click(App::under(app.items("history-kept").first(), "history-forget"));
        QTRY_COMPARE(app.asked("POST api/resume/rm"), 1);
        QTRY_COMPARE(app.items("history-kept").size(), 2);

        // Words and places.
        QTest::keyClick(app.window, Qt::Key_1);
        app.seeAll();
        QStringList said;
        App::texts(app.item("feed-tabs"), said);
        QVERIFY2(said.contains("Shorts: off"), qPrintable(said.join("|")));
        QTest::keyClick(app.window, Qt::Key_5);
        QQuickItem *forget = app.item("forget-positions"), *note = app.item("set-resume-note");
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        QVERIFY(forget->isVisible());
        QVERIFY2(box(forget).top() >= box(note).bottom() - 1, qPrintable(QString::number(box(forget).top())));
        QCOMPARE(box(app.item("settings-title")).left(), 24.0);
    }

    void theSortMenuTheRouteAndTheSettingsHaveKeys()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 4);
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!home->property("onLead").toBool());

        // S opens the sort menu of the shelf the keyboard is on; j, k and Enter work in it.
        QQuickItem *sort = App::under(app.item("shelf-latest"), "shelf-sort");
        QTest::keyClick(app.window, Qt::Key_S);
        QTRY_VERIFY(sort->property("opened").toBool());
        QQuickItem *views = nullptr;
        QTRY_VERIFY((views = app.item("sort-views")) && views->isVisible());
        QVERIFY(app.item("sort-default")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_VERIFY(views->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_VERIFY(sort->property("text").toString().contains("Most viewed"));
        QTest::keyClick(app.window, Qt::Key_K);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(sort->property("text").toString(), QString("Newest"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!sort->property("opened").toBool());
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());

        // O opens that shelf in full, where S is the grid's own sort menu.
        QTest::keyClick(app.window, Qt::Key_O);
        QQuickItem *grid = app.item("home-grid");
        QTRY_VERIFY(grid->isVisible());
        QVERIFY(app.item("feed-tab-latest")->property("current").toBool());
        QTest::keyClick(app.window, Qt::Key_S);
        QTRY_VERIFY(app.item("feed-sort")->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.item("feed-sort")->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(app.item("shelves")->isVisible());

        // P opens the route with the keyboard in it: left and right choose.
        QObject *popup = app.object("routePopup");
        QTest::keyClick(app.window, Qt::Key_P);
        QTRY_VERIFY(popup->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("tor"));
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("direct"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!popup->property("opened").toBool());

        // Settings: Enter goes from the categories into their rows, where up and down
        // move and left and right change the choice; Escape comes back out.
        QTest::keyClick(app.window, Qt::Key_5);
        QQuickItem *settings = app.item("settings");
        QTRY_COMPARE(app.page(), QString("settings"));
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(settings->property("category").toString(), QString("playback"));
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_VERIFY(settings->property("inRows").toBool());
        QVERIFY(app.item("set-quality")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_COMPARE(app.backend->config().value("maxHeight").toInt(), 720);
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(app.backend->config().value("maxHeight").toInt(), 1080);
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!settings->property("inRows").toBool());
        QCOMPARE(app.page(), QString("settings"));
        // In a category with several rows, down goes to the next.
        QTest::keyClick(app.window, Qt::Key_K);
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_VERIFY(app.item("set-route")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_VERIFY(app.item("set-history")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_VERIFY(app.backend->config().value("history").toBool());
    }

    void theHelpFitsAQuarterTile()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        {
            App app(false, QSize(941, 508));
            READY(app);
            QObject *help = app.object("help");
            QTest::keyClick(app.window, '?');
            QTRY_VERIFY(help->property("opened").toBool());
            QTest::qWait(250);
            const QRectF sheet = box(help->property("background").value<QQuickItem *>());
            QVERIFY2(QRectF(0, 0, 941, 508).contains(sheet),
                     qPrintable(QString("%1,%2 %3x%4").arg(sheet.x()).arg(sheet.y()).arg(sheet.width()).arg(sheet.height())));
            // Its two groups stand side by side, and all of it is in view.
            QQuickItem *first = app.item("help-group-0"), *second = app.item("help-group-1");
            QVERIFY(first && second);
            QVERIFY(qAbs(box(first).top() - box(second).top()) < 1);
            QVERIFY(sheet.contains(box(first)) && sheet.contains(box(second)));
            const QStringList listed = help->property("listed").toStringList();
            for (const char *key : {"S", "O", "N", "U", "H"})
                QVERIFY2(listed.contains(key), key);
        }
        // In a narrow column they are one under the other, and the sheet scrolls if it has to.
        App app(false, QSize(630, 600));
        READY(app);
        QObject *help = app.object("help");
        QTest::keyClick(app.window, '?');
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::qWait(250);
        const QRectF sheet = box(help->property("background").value<QQuickItem *>());
        QVERIFY2(QRectF(0, 0, 630, 600).contains(sheet), qPrintable(QString("%1x%2").arg(sheet.width()).arg(sheet.height())));
        QVERIFY(box(app.item("help-group-1")).top() > box(app.item("help-group-0")).bottom() - 1);
    }

    // --- What the second council found in the code. ---

    void editingALensChangesItAndAddsNone()
    {
        App app(true);
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QQuickItem *text = app.item("lens-text");
        QTRY_VERIFY(text->property("editing").toBool());
        app.type("bread, sourdough over 30 min");
        QTRY_VERIFY(app.item("lens-save")->isEnabled());
        app.click(app.item("lens-save"));
        QTRY_COMPARE(app.items("lens-row").size(), 1);
        QCOMPARE(app.asked("POST api/lenses"), 1);
        // Now change its words: the same lens is saved again.
        QMetaObject::invokeMethod(text, "edit");
        QTRY_VERIFY(text->property("editing").toBool());
        QTest::keyClick(app.window, Qt::Key_A, Qt::ControlModifier);
        app.type("bread, rye over 30 min");
        QTRY_VERIFY(app.item("lens-save")->isEnabled());
        app.click(app.item("lens-save"));
        QTRY_COMPARE(app.asked("POST api/lenses/bbbb"), 1);
        QCOMPARE(app.asked("POST api/lenses"), 2);
        QCOMPARE(app.items("lens-row").size(), 1);
    }

    void theKeyboardGoesBackToTheVideo()
    {
        App app;
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        const auto play = [&](int i) {
            QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(i)),
                                      Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, i));
            QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        };
        QQuickItem *watch = app.item("watch");
        play(0);
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_VERIFY(watch->property("inSheet").toBool());
        // Left by a page key with the keyboard in the sheet: the next video has it in the video.
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.page(), QString("lenses"));
        // (It went on in the small window; that is closed, so the next plays here.)
        QTRY_VERIFY(app.pip()->isVisible());
        app.pip()->close();
        QTRY_VERIFY(!app.pip()->isVisible());
        play(1);
        QTRY_VERIFY(app.window->property("watching").toBool());
        QVERIFY(!watch->property("inSheet").toBool());
        QVERIFY(app.window->property("steering").toBool());
        // Theatre hides the sheet, and takes the keyboard out of it.
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_VERIFY(watch->property("inSheet").toBool());
        watch->setProperty("theatre", true);
        QTRY_VERIFY(!watch->property("inSheet").toBool());
        QVERIFY(app.item("surface")->hasActiveFocus() || app.item("surface")->property("activeFocus").toBool()
                || app.window->property("steering").toBool());
        watch->setProperty("theatre", false);

        // The route's popup, opened while watching, has the arrows and h and l.
        QObject *popup = app.object("routePopup");
        QTest::keyClick(app.window, Qt::Key_P);
        QTRY_VERIFY(popup->property("opened").toBool());
        QVERIFY(!app.window->property("steering").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("tor"));
        // (A new route ends what was playing over the old one; the popup keeps the keyboard.)
        QTest::keyClick(app.window, Qt::Key_Left);
        QTRY_COMPARE(app.backend->config().value("route").toString(), QString("direct"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!popup->property("opened").toBool());
        QTRY_VERIFY(app.window->activeFocusItem() != nullptr);
    }

    void settingsKeysChangeOnlyTheMarkedRow()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_5);
        QQuickItem *settings = app.item("settings");
        QTRY_COMPARE(app.page(), QString("settings"));
        QTest::keyClick(app.window, Qt::Key_J);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_VERIFY(settings->property("inRows").toBool());
        // A category chosen with the pointer: nothing is marked, and keys change nothing.
        app.click(app.item("settings-cat-home"));
        QTRY_VERIFY(!settings->property("inRows").toBool());
        QTest::keyClick(app.window, Qt::Key_L);
        QTest::keyClick(app.window, Qt::Key_J);
        QTest::keyClick(app.window, Qt::Key_L);
        QCOMPARE(app.window->property("thumbs").toString(), QString("clear"));
        QVERIFY(!app.backend->config().value("topicFeeds").toBool());
        // About has no rows: keys there are harmless.
        app.click(app.item("settings-cat-about"));
        QTest::keyClick(app.window, Qt::Key_Return);
        QTest::keyClick(app.window, Qt::Key_J);
        QTest::keyClick(app.window, Qt::Key_H);
        QTest::keyClick(app.window, Qt::Key_L);
        QVERIFY(!settings->property("inRows").toBool());
        // Leaving with the keyboard in the rows and coming back starts in the categories.
        app.click(app.item("settings-cat-playback"));
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_VERIFY(settings->property("inRows").toBool());
        QTest::keyClick(app.window, Qt::Key_1);
        QTest::keyClick(app.window, Qt::Key_5);
        QTRY_COMPARE(app.page(), QString("settings"));
        QVERIFY(!settings->property("inRows").toBool());
    }

    void whatComesNextIsTheListAsItWasWhenPlayed()
    {
        App app(false, QSize(1280, 720));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QTest::keyClick(app.window, Qt::Key_3);
        QQuickItem *list = nullptr;
        QTRY_VERIFY((list = app.item("channel-list")) && list->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(app.item("channel-pane-name")->property("text").toString(), QString("Northlight Woodworks"));
        QTest::keyClick(app.window, Qt::Key_L);
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_VERIFY(app.window->property("watching").toBool());
        QObject *next = app.item("watch")->property("list").value<QObject *>();
        QVERIFY(next);
        const auto channels = [&] {
            QStringList found;
            QObject *l = app.item("watch")->property("list").value<QObject *>();
            for (int i = 0; i < l->property("count").toInt(); ++i) {
                QVariantMap v;
                QMetaObject::invokeMethod(l, "get", Q_RETURN_ARG(QVariantMap, v), Q_ARG(int, i));
                found << v.value("channel").toString();
            }
            return found;
        };
        QTRY_VERIFY(!channels().isEmpty());
        const QStringList before = channels();
        QVERIFY(before.contains("Northlight Woodworks"));
        // The channels change while it plays (one is let go): what comes next doesn't.
        app.backend->unsubscribe("UCeeeeeeeeeeeeeeeeeeeeee");
        QTRY_COMPARE(app.backend->property("subs").toList().size(), 4);
        QTest::qWait(100);
        QCOMPARE(channels(), before);
    }

    void bracketsChangeTheTabAndNothingActsUnderAMenu()
    {
        App app;
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        // Lenses: ] and [ go along the strip of lenses.
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.items("lens-row").size(), 2);
        QTRY_VERIFY(app.items("lens-row").at(0)->property("current").toBool());
        QTest::keyClick(app.window, Qt::Key_BracketRight);
        QTRY_VERIFY(app.items("lens-row").at(1)->property("current").toBool());
        QTest::keyClick(app.window, Qt::Key_BracketLeft);
        QTRY_VERIFY(app.items("lens-row").at(0)->property("current").toBool());
        // The full feed: along its tabs.
        QTest::keyClick(app.window, Qt::Key_1);
        app.seeAll();
        QTRY_VERIFY(app.item("feed-tab-latest")->property("current").toBool());
        QTest::keyClick(app.window, Qt::Key_BracketRight);
        QTRY_VERIFY(!app.item("feed-tab-latest")->property("current").toBool());
        QTest::keyClick(app.window, Qt::Key_BracketLeft);
        QTRY_VERIFY(app.item("feed-tab-latest")->property("current").toBool());

        // Under the help, the page keys do nothing; under a menu, Space picks.
        QTest::keyClick(app.window, '?');
        QObject *help = app.object("help");
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_2);
        QCOMPARE(app.page(), QString("home"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!help->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_S);
        QQuickItem *sort = app.item("feed-sort");
        QTRY_VERIFY(sort->property("opened").toBool());
        // (To the top first: the menu opens on the order that holds.)
        for (int i = 0; i < 5; ++i)
            QTest::keyClick(app.window, Qt::Key_K);
        // (Down to Most viewed: an option the list's own order makes the same as another isn't shown.)
        for (int i = 0; i < 5 && !app.item("sort-views")->property("marked").toBool(); ++i)
            QTest::keyClick(app.window, Qt::Key_J);
        QTRY_VERIFY(app.item("sort-views")->property("marked").toBool());
        QTest::keyClick(app.window, Qt::Key_Space);
        QTRY_VERIFY(sort->property("text").toString().contains("Most viewed"));
        QVERIFY(!app.window->property("watching").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!sort->property("opened").toBool());

        // Watching, with the keyboard in the sheet: [ and ] change its tab.
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_C);
        QQuickItem *sheet = app.item("sheet");
        QTRY_COMPARE(sheet->property("current").toString(), QString("comments"));
        QTest::keyClick(app.window, Qt::Key_BracketLeft);
        QTRY_COMPARE(sheet->property("current").toString(), QString("next"));
    }

    void nothingIsCutOrOverlappedInTheQuarterTile()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        // The help: every line whole, and it fits.
        QTest::keyClick(app.window, '?');
        QObject *help = app.object("help");
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::qWait(250);
        for (QQuickItem *said : app.items("help-said"))
            QVERIFY2(!said->property("truncated").toBool() && said->property("lineCount").toInt() <= 2,
                     qPrintable(said->property("text").toString()));
        const QRectF sheet = box(help->property("background").value<QQuickItem *>());
        QVERIFY2(sheet.bottom() <= 508 && sheet.top() >= 0, qPrintable(QString::number(sheet.height())));
        const QStringList listed = help->property("listed").toStringList();
        for (const char *key : {"Enter", "[", "]", "K", "S", "O", "U", "N", "C"})
            QVERIFY2(listed.contains(key), key);
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!help->property("opened").toBool());

        // Watching: the chapter's name stops before the speed; Up next's lines are whole.
        QMetaObject::invokeMethod(app.window, "play", Q_ARG(QVariant, app.backend->feed()->get(0)),
                                  Q_ARG(QVariant, QVariant::fromValue(app.backend->feed())), Q_ARG(QVariant, 0));
        QQuickItem *surface = app.item("surface");
        QTRY_VERIFY(!surface->property("pending").toBool());
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        QQuickItem *chapter = App::under(surface, "seek-chapter"), *speed = App::under(surface, "seek-speed");
        QVERIFY(chapter && speed);
        QTRY_VERIFY(!chapter->property("text").toString().isEmpty());
        if (chapter->isVisible())
            QVERIFY2(box(chapter).right() <= box(speed).left(), qPrintable(QString("%1 > %2").arg(box(chapter).right()).arg(box(speed).left())));
        QTRY_VERIFY(app.items("next-meta").size() >= 3);
        for (QQuickItem *meta : app.items("next-meta"))
            QVERIFY2(!meta->property("truncated").toBool(), qPrintable(meta->property("text").toString()));
    }

    void popupsKeepTheKeyboardWhereItBelongs()
    {
        App app;
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QTRY_VERIFY(app.item("shelves-home")->property("leadSelected").toBool());
        // The help has the keyboard: Enter under it resumes nothing.
        QObject *help = app.object("help");
        QTest::keyClick(app.window, '?');
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::keyClick(app.window, Qt::Key_Return);
        QTest::keyClick(app.window, Qt::Key_Space);
        QTest::qWait(200);
        QVERIFY(!app.window->property("watching").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!help->property("opened").toBool());
        // ...and gives it back to the page when it closes.
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());

        // The route's popup, closed by a click in the search field, leaves the keyboard there.
        QObject *route = app.object("routePopup");
        QTest::keyClick(app.window, Qt::Key_P);
        QTRY_VERIFY(route->property("opened").toBool());
        app.click(app.item("search"));
        QTRY_VERIFY(!route->property("opened").toBool());
        QTest::qWait(300);
        QVERIFY2(app.window->property("typing").toBool(), qPrintable(QDebug::toString(app.window->activeFocusItem())));
    }

    void switchingTheHistoryKeepsTheKeyboardOnTheList()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_4);
        QTRY_VERIFY(app.item("history-kept-list")->hasActiveFocus());
        app.click(app.item("history-turn-on"));
        QQuickItem *list = nullptr;
        QTRY_VERIFY((list = app.item("history-list")) && list->isVisible());
        QTRY_COMPARE(app.items("history-row").size(), 4);
        QTRY_VERIFY(list->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(list->property("currentIndex").toInt(), 1);
    }

    void inARightToLeftShortTileTheLeadsActionsAreOnIt()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(941, 508));
        READY(app);
        QGuiApplication::setLayoutDirection(Qt::RightToLeft);
        QQuickItem *lead = app.item("lead");
        QTRY_VERIFY(lead->isVisible());
        QTest::qWait(250);
        for (const char *name : {"lead-resume", "lead-over", "lead-forget"}) {
            QQuickItem *act = app.item(name);
            QVERIFY2(box(lead).contains(box(act)), name);
            QVERIFY2(!box(act).intersects(box(app.item("lead-title"))), name);
            QVERIFY2(!box(act).intersects(box(app.item("lead-picture"))), name);
        }
        QGuiApplication::setLayoutDirection(Qt::LeftToRight);
    }

    void leftLeavesTheChannelPaneOnlyFromItsEdge()
    {
        App app(false, QSize(1280, 720));
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QTest::keyClick(app.window, Qt::Key_3);
        QQuickItem *list = nullptr, *pane = nullptr;
        QTRY_VERIFY((list = app.item("channel-list")) && list->hasActiveFocus());
        pane = app.item("channel-pane");
        QTRY_VERIFY(pane->property("count").toInt() >= 2);
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_VERIFY(pane->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(pane->property("currentIndex").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_COMPARE(pane->property("currentIndex").toInt(), 0);
        QVERIFY(pane->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_H);
        QTRY_VERIFY(list->hasActiveFocus());
    }

    // --- What the council found in the code. ---

    void turningTopicFeedsOffDuringARefreshKeepsThemOff()
    {
        App app(false, QSize(1280, 2400), {{"QVEIL_STANDIN_SLOW_TOPICS", "1.2"}});
        READY(app);
        app.backend->setConfig({{"topicFeeds", true}});
        QTRY_VERIFY(app.backend->busy().contains("topics"));
        app.backend->setConfig({{"topicFeeds", false}});
        QTRY_VERIFY(!app.backend->config().value("topicFeeds").toBool());
        // The search that was running is let go of, and what it finds is not shown.
        QTRY_VERIFY(!app.backend->busy().contains("topics"));
        QTest::qWait(1800);
        QVERIFY(!app.backend->property("topics").toMap().value("enabled").toBool());
        QVERIFY(!app.item("shelf-topic-aa11bb22cc33"));
    }

    void topicShelvesShowWhatIsKeptWhileARefreshRunsAndWhenItFails()
    {
        App app(false, QSize(1280, 2400),
                {{"QVEIL_STANDIN_TOPICS", "fetched"}, {"QVEIL_STANDIN_SLOW_TOPICS", "1.5"}, {"QVEIL_STANDIN_TOPICS_FAIL", "1"}});
        READY(app);
        // What was found before is there at once, while the refresh is still running...
        QQuickItem *wood = nullptr;
        QTRY_VERIFY_WITH_TIMEOUT((wood = app.item("shelf-topic-aa11bb22cc33")), 1200);
        QTRY_VERIFY_WITH_TIMEOUT(App::count(wood, "card") >= 4, 1000);
        QVERIFY(app.backend->busy().contains("topics"));
        // ...and still there when it has failed, with the failure said.
        QTRY_VERIFY_WITH_TIMEOUT(!app.backend->busy().contains("topics"), 4000);
        QTRY_VERIFY(!app.backend->error().isEmpty());
        QVERIFY((wood = app.item("shelf-topic-aa11bb22cc33")));
        QVERIFY(App::count(wood, "card") >= 4);
    }

    void aTopicWhoseSearchFailedIsSaid()
    {
        App app(false, QSize(1280, 2400), {{"QVEIL_STANDIN_TOPICS", "fetched"}, {"QVEIL_STANDIN_TOPICS_FAIL", "group"}});
        READY(app);
        QTRY_VERIFY(app.item("shelf-topic-aa11bb22cc33"));
        QTRY_VERIFY_WITH_TIMEOUT(!app.backend->busy().contains("topics"), 4000);
        // The engine names the group whose search failed; the window shows its words.
        QTRY_VERIFY2(app.backend->error().contains("borrow checker, allocators: YouTube didn't answer"),
                     qPrintable(app.backend->error()));
        QVERIFY(app.item("shelf-topic-aa11bb22cc33"));
    }

    void hidingATopicNeverLeavesAShelfWaitingForNothing()
    {
        App app(false, QSize(1280, 3200), {{"QVEIL_STANDIN_TOPICS", "fetched"}, {"QVEIL_STANDIN_TOPIC_GROUPS", "5"}});
        READY(app);
        QQuickItem *first = nullptr;
        QTRY_VERIFY((first = app.item("shelf-topic-aa11bb22cc33")));
        QTRY_VERIFY(App::count(first, "card") >= 4);
        const int searches = app.asked("GET api/topics?refresh");
        app.click(App::under(first, "topic-hide"));
        QTRY_VERIFY(!app.item("shelf-topic-aa11bb22cc33"));
        // The one that comes in in its place is searched for, and then has its videos:
        // nothing sits there breathing with no search behind it.
        QTRY_VERIFY(app.asked("GET api/topics?refresh") > searches);
        QTRY_VERIFY(!app.backend->busy().contains("topics"));
        QQuickItem *fifth = nullptr;
        QTRY_VERIFY((fifth = app.item("shelf-topic-000000000005")));
        QTRY_VERIFY(App::count(fifth, "card") >= 1);
        QCOMPARE(App::count(app.item("shelves"), "card-placeholder"), 0);
    }

    void upOnTheFirstShelfKeepsThePageAtItsTop()
    {
        App app;
        READY(app);
        QQuickItem *lead = app.item("lead");
        QTRY_VERIFY(lead->isVisible());
        QTRY_VERIFY(app.item("shelf-latest") && App::count(app.item("shelf-latest"), "card") >= 4);
        QTRY_COMPARE(app.item("stage")->opacity(), 1.0);
        const qreal top = lead->mapToScene(QPointF(0, 0)).y();
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        for (const int key : {Qt::Key_Right, Qt::Key_K, Qt::Key_Down, Qt::Key_Up, Qt::Key_Up}) {
            QTest::keyClick(app.window, Qt::Key(key));
            QTest::qWait(350);
        }
        QCOMPARE(app.item("shelves-home")->property("row").toInt(), 0);
        QCOMPARE(lead->mapToScene(QPointF(0, 0)).y(), top);
    }

    void liveStreamsAreNotOnShelves()
    {
        App app(false, QSize(1280, 2400), {{"QVEIL_STANDIN_LIVE", "1"}, {"QVEIL_STANDIN_TOPICS", "fetched"}});
        READY(app);
        app.backend->setConfig({{"topicFeeds", true}});
        QQuickItem *lens = nullptr, *topic = nullptr;
        // (The topics first: the shelves are made again when they arrive.)
        QTRY_VERIFY((topic = app.item("shelf-topic-aa11bb22cc33")));
        QTRY_VERIFY(App::count(topic, "card") >= 4);
        QTRY_VERIFY((lens = app.item("shelf-aaaa0002")));
        QTRY_VERIFY(App::count(lens, "card") >= 4);
        // One of each shelf's videos is a live stream, which qveil can't play: it isn't offered.
        for (QQuickItem *shelf : {lens, topic}) {
            QList<QQuickItem *> cards;
            App::collect(shelf, "card", cards);
            for (QQuickItem *card : cards) {
                QVERIFY2(card->property("videoId").toString() != "len0000200" && card->property("videoId").toString() != "tpw0000000",
                         qPrintable(card->property("videoId").toString()));
            }
        }
    }

    void aSortedGridKeepsItsPlaceWhenItsListChanges()
    {
        App app;
        READY(app);
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QMetaObject::invokeMethod(app.window, "arrange", Q_ARG(QVariant, QString("latest")),
                                  Q_ARG(QVariant, QVariantMap({{"sort", "views"}, {"length", "any"}, {"watched", "hide"}})));
        app.seeAll();
        QQuickItem *grid = app.item("home-grid");
        // Twelve, less the three that have a kept position.
        QTRY_COMPARE(grid->property("count").toInt(), 9);
        grid->setProperty("currentIndex", 6);
        QObject *list = grid->property("list").value<QObject *>();
        QVERIFY(list);
        QVariantMap at;
        QMetaObject::invokeMethod(list, "get", Q_RETURN_ARG(QVariantMap, at), Q_ARG(int, 6));
        const QString id = at.value("id").toString();
        QVERIFY(!id.isEmpty());
        // A position is forgotten, so one more video belongs in the list.
        app.backend->forgetResume("vid00000004");
        QTRY_COMPARE(grid->property("count").toInt(), 10);
        // The keyboard is still on the video it was on, not thrown back to the first.
        const int now = grid->property("currentIndex").toInt();
        QVERIFY2(now > 0, "the grid was reset to its start");
        QMetaObject::invokeMethod(list, "get", Q_RETURN_ARG(QVariantMap, at), Q_ARG(int, now));
        QCOMPARE(at.value("id").toString(), id);
    }

    void theSortMenuStaysInsideAWideWindow()
    {
        App app(false, QSize(1920, 1080));
        READY(app);
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        app.click(App::under(shelf, "shelf-sort"));
        QQuickItem *last = nullptr;
        QTRY_VERIFY((last = app.item("watched-hide")) && last->isVisible());
        QTest::qWait(250);
        const QRectF at = last->mapRectToScene(QRectF(0, 0, last->width(), last->height()));
        QVERIFY2(QRectF(0, 0, 1920, 1080).contains(at),
                 qPrintable(QString("%1,%2 %3x%4").arg(at.x()).arg(at.y()).arg(at.width()).arg(at.height())));
        app.click(last);
        QTRY_VERIFY(App::under(shelf, "shelf-sort")->property("text").toString().contains("unwatched"));
    }

    void whileAVideoIsInTheSmallWindowTheMainWindowsKeysAreItsOwn()
    {
        App app(false, QSize(1280, 1600));
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        app.window->requestActivate();
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QTRY_VERIFY(!app.window->property("steering").toBool());
        // (Off the lead, onto the first shelf.)
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!app.item("shelves-home")->property("onLead").toBool());
        // J and L move, as H and K do.
        QTest::keyClick(app.window, Qt::Key_L);
        QTRY_COMPARE(home->property("column").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_J);
        QTRY_COMPARE(home->property("row").toInt(), 1);
        // Escape goes back in this window; it doesn't pull the video out of the small one.
        QMetaObject::invokeMethod(app.window, "openChannel", Q_ARG(QVariant, QString("UCaaaaaaaaaaaaaaaaaaaaaa")));
        QCOMPARE(app.page(), QString("channel"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QCOMPARE(app.page(), QString("home"));
        QVERIFY(app.window->property("pip").toBool());
        QVERIFY(app.pip()->isVisible());
    }

    void whatTurningTopicFeedsOnDoesIsSaidBeforeItIsDone()
    {
        App app;
        READY(app);
        QTest::keyClick(app.window, Qt::Key_5);
        app.click(app.item("settings-cat-home"));
        const QVariantMap pages = app.backend->privacy().value("pages").toMap();
        QVERIFY(pages.value("topics_on").toString().startsWith("On"));
        // While it is off, the row says what off is, and what on would do, in the engine's words.
        QCOMPARE(app.item("set-topics-note")->property("text").toString(), pages.value("topics_off").toString());
        QQuickItem *would = app.item("set-topics-would");
        QVERIFY(would && would->isVisible());
        QCOMPARE(would->property("text").toString(), pages.value("topics_on").toString());
        QCOMPARE(app.asked("GET api/topics?refresh"), 0);
        app.click(app.item("topics-on"));
        QTRY_VERIFY(app.backend->config().value("topicFeeds").toBool());
        QTRY_VERIFY(!would->isVisible());
    }

    // --- The window adapts to its size. ---

    void aShelfShowsWholeCardsAndTheLatestFillTheWindow()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        // Nothing unfinished and no lenses, in a small tile: the latest are the page.
        {
            App app(true, QSize(941, 508));
            READY(app);
            QQuickItem *shelf = nullptr;
            QTRY_VERIFY((shelf = app.item("shelf-latest")));
            QTRY_VERIFY(App::count(shelf, "card") >= 6);
            QCOMPARE(shelf->property("across").toInt(), 3);
            QVERIFY(shelf->property("rows").toInt() >= 2);
            // Whole cards: none reaches past the window's edge, and the first row has three.
            QList<QQuickItem *> cards;
            App::collect(shelf, "card", cards);
            int top = 0;
            for (QQuickItem *card : cards) {
                QVERIFY2(box(card).right() <= 941 && box(card).left() >= 0, qPrintable(QString::number(box(card).right())));
                if (qAbs(box(card).top() - box(cards.first()).top()) < 1)
                    ++top;
            }
            QCOMPARE(top, 3);
            // The arrows move through the rows of the one shelf.
            QQuickItem *home = app.item("shelves-home");
            QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
            QTest::keyClick(app.window, Qt::Key_Right);
            QTRY_COMPARE(home->property("column").toInt(), 1);
            QTest::keyClick(app.window, Qt::Key_Down);
            QTRY_COMPARE(home->property("column").toInt(), 4);
            QCOMPARE(home->property("row").toInt(), 0);
            QTest::keyClick(app.window, Qt::Key_Up);
            QTRY_COMPARE(home->property("column").toInt(), 1);
            // What the keyboard is on is in view.
            QTest::keyClick(app.window, Qt::Key_Down);
            QTest::keyClick(app.window, Qt::Key_Down);
            QQuickItem *selected = nullptr;
            QTRY_VERIFY2(([&] {
                for (QQuickItem *card : app.items("card"))
                    if (card->property("selected").toBool())
                        selected = card;
                return selected && box(selected).bottom() <= 508 && box(selected).top() >= 56;
            })(), "the selected card is out of view");
        }
        // With a lead and other shelves there is a row of the latest, of whole cards, that scrolls sideways.
        {
            App app;
            READY(app);
            QTRY_VERIFY(app.item("lead")->isVisible());
            QQuickItem *shelf = nullptr;
            QTRY_VERIFY((shelf = app.item("shelf-latest")));
            QTRY_VERIFY(App::count(shelf, "card") >= 5);
            QCOMPARE(shelf->property("across").toInt(), 4);
            QCOMPARE(shelf->property("rows").toInt(), 1);
            QList<QQuickItem *> cards;
            App::collect(shelf, "card", cards);
            std::sort(cards.begin(), cards.end(), [&](QQuickItem *a, QQuickItem *b) { return box(a).left() < box(b).left(); });
            // The fourth ends at the page's edge, where the fifth begins.
            QVERIFY2(qAbs(box(cards.at(3)).right() - (1280 - 16)) <= 12, qPrintable(QString::number(box(cards.at(3)).right())));
            QVERIFY(box(cards.at(4)).left() >= 1280 - 28);
        }
    }

    void aWideWindowIsDrawnHalfAgainAsLarge()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(1920, 1080));
        READY(app);
        QCOMPARE(app.window->property("zoom").toReal(), 1.5);
        // The bar, its type and the cards are all half again as large, and span the window.
        QQuickItem *bar = app.item("bar");
        QCOMPARE(box(bar), QRectF(0, 0, 1920, 84));
        QTRY_VERIFY(app.item("lead")->isVisible());
        QQuickItem *shelf = nullptr;
        QTRY_VERIFY((shelf = app.item("shelf-latest")));
        QTRY_VERIFY(App::count(shelf, "card") >= 4);
        QCOMPARE(shelf->property("across").toInt(), 4);
        QQuickItem *card = App::under(shelf, "card");
        QVERIFY2(box(card).width() > 440 && box(card).width() < 480, qPrintable(QString::number(box(card).width())));
        // Pressing things still presses them.
        app.click(app.item("nav-channels"));
        QCOMPARE(app.page(), QString("channels"));
        // What opens over the window is as large, and inside it.
        app.click(app.item("nav-route"));
        QObject *popup = app.object("routePopup");
        QTRY_VERIFY(popup->property("opened").toBool());
        QTest::qWait(200);
        QQuickItem *background = popup->property("background").value<QQuickItem *>();
        QVERIFY2(qAbs(box(background).width() - 660) < 2, qPrintable(QString::number(box(background).width())));
        QVERIFY2(QRectF(0, 0, 1920, 1080).contains(box(background)) && box(background).top() >= 83,
                 qPrintable(QString("%1,%2 %3x%4").arg(box(background).x()).arg(box(background).y()).arg(box(background).width()).arg(box(background).height())));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!popup->property("opened").toBool());
        QObject *help = app.object("help");
        QTest::keyClick(app.window, '?');
        QTRY_VERIFY(help->property("opened").toBool());
        QTest::qWait(200);
        QQuickItem *sheet = help->property("background").value<QQuickItem *>();
        QVERIFY2(qAbs(box(sheet).center().x() - 960) < 3 && qAbs(box(sheet).center().y() - 540) < 3 && box(sheet).top() >= 0,
                 qPrintable(QString("%1,%2").arg(box(sheet).center().x()).arg(box(sheet).center().y())));
        QTest::keyClick(app.window, Qt::Key_Escape);
        // The video is drawn for the pixels it covers, not drawn small and stretched.
        app.play({{"id", "vid00000002"}});
        QObject *mpv = app.item("surface")->property("mpv").value<QObject *>();
        QTRY_COMPARE(mpv->property("renderScale").toReal(), 1.5);

        // A window of the usual size is drawn as it was.
        App usual;
        READY(usual);
        QCOMPARE(usual.window->property("zoom").toReal(), 1.0);
        QCOMPARE(box(usual.item("bar")).height(), 56.0);
        QCOMPARE(usual.item("surface")->property("mpv").value<QObject *>()->property("renderScale").toReal(), 1.0);
    }

    // --- What the review of the window found. ---

    void homeShowsTheLensItsTabNames()
    {
        App app;
        READY(app);
        app.seeAll();
        QTRY_VERIFY(app.item("feed-tab-aaaa0002"));
        app.click(app.item("feed-tab-aaaa0002"));
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0002"));
        // Another lens is opened on the lenses page...
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.items("lens-row").size(), 2);
        app.click(app.items("lens-row").first());
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0001"));
        // ...and home still shows the one its tab says.
        QTest::keyClick(app.window, Qt::Key_1);
        app.seeAll("aaaa0002");
        QVERIFY(app.item("feed-tab-aaaa0002")->property("current").toBool());
        QTRY_COMPARE(app.backend->lensInfo().value("id").toString(), QString("aaaa0002"));
        QTRY_VERIFY(!app.items("card-why").isEmpty());
        QVERIFY(app.items("card-why").first()->property("text").toString().contains("over 30:00"));
        // Putting that right is no reason to search again.
        QCOMPARE(app.asked("GET api/lens?id=aaaa0002&stream=1&refresh=2"), 0);
    }

    void arrowsMoveThroughTheGridWhileAVideoIsInTheSmallWindow()
    {
        App app;
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_I);
        QTRY_VERIFY(app.pip()->isVisible());
        QMetaObject::invokeMethod(app.window, "go", Q_ARG(QVariant, QString("home")));
        app.window->requestActivate();
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QTRY_VERIFY2(!app.window->property("steering").toBool(),
                     qPrintable(QString("main active %1, small active %2").arg(app.window->isActive()).arg(app.pip()->isActive())));
        // (Off the lead, onto the first shelf.)
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!app.item("shelves-home")->property("onLead").toBool());
        // On the shelves...
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(home->property("column").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_COMPARE(home->property("row").toInt(), 1);
        // ...and in a grid.
        app.seeAll();
        QQuickItem *grid = app.item("home-grid");
        QTRY_VERIFY(grid->hasActiveFocus());
        const int columns = grid->property("columns").toInt();
        QVERIFY(columns > 1);
        grid->setProperty("currentIndex", 0);
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 1);
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 1 + columns);
        // The keys that move nothing in a grid still reach the video.
        QObject *mpv = app.inPip("pip-surface")->property("mpv").value<QObject *>();
        QVERIFY(mpv);
        const bool muted = mpv->property("muted").toBool();
        QTest::keyClick(app.window, Qt::Key_M);
        QTRY_COMPARE(mpv->property("muted").toBool(), !muted);
    }

    void theRoutePopupStaysInsideTheWindow()
    {
        for (const Qt::LayoutDirection direction : {Qt::LeftToRight, Qt::RightToLeft}) {
            App app(false, QSize(950, 520));
            READY(app);
            QGuiApplication::setLayoutDirection(direction);
            QTest::qWait(100);
            QMetaObject::invokeMethod(app.window, "openRoute");
            QObject *popup = app.object("routePopup");
            QTRY_VERIFY(popup->property("opened").toBool());
            QTest::qWait(300);
            const auto inside = [&](const char *name) {
                QQuickItem *found = nullptr;
                for (QQuickItem *item : app.items(name))
                    found = item;
                if (!found)
                    return QString("%1 isn't showing").arg(name);
                const QRectF at = found->mapRectToScene(QRectF(0, 0, found->width(), found->height()));
                return QRectF(0, 0, 950, 520).contains(at) && at.width() > 20 ? QString()
                    : QString("%1 is at %2,%3 %4x%5").arg(name).arg(at.x()).arg(at.y()).arg(at.width()).arg(at.height());
            };
            QVERIFY2(inside("route-direct").isEmpty(), qPrintable(inside("route-direct")));
            QVERIFY2(inside("route-proxy").isEmpty(), qPrintable(inside("route-proxy")));
            QVERIFY2(inside("all-settings").isEmpty(), qPrintable(inside("all-settings")));
            QQuickItem *background = popup->property("background").value<QQuickItem *>();
            QVERIFY(background && background->width() >= 300);
            QGuiApplication::setLayoutDirection(Qt::LeftToRight);
        }
    }

    void twelveLensesCanAllBeReachedInASmallWindow()
    {
        const auto box = [](QQuickItem *item) { return item->mapRectToScene(QRectF(0, 0, item->width(), item->height())); };
        App app(false, QSize(950, 520), {{"QVEIL_STANDIN_LENSES", "12"}});
        READY(app);
        QTest::keyClick(app.window, Qt::Key_2);
        QTRY_COMPARE(app.backend->lenses().size(), 12);
        QQuickItem *list = app.item("lens-list");
        QVERIFY2(list, "the lenses are not in a strip that scrolls");
        QTRY_VERIFY(list->property("contentWidth").toReal() > list->width());
        QVERIFY(box(list).right() <= 950);
        // The way to a new one is never scrolled out of reach, and the results keep room.
        QQuickItem *fresh = app.item("lens-new");
        QVERIFY(fresh->isVisible() && box(fresh).right() <= 950 && box(fresh).left() >= box(list).right() - 1);
        QQuickItem *grid = app.item("lens-grid");
        QTRY_VERIFY2(grid->height() >= 200, qPrintable(QString::number(grid->height())));
        // The last lens can be brought into view and opened.
        // (Once the strip has its tabs' widths.)
        QTest::qWait(200);
        list->setProperty("contentX", list->property("contentWidth").toReal() - list->width());
        QTest::qWait(100);
        QQuickItem *last = nullptr;
        for (QQuickItem *row : app.items("lens-row"))
            if (box(row).right() <= box(list).right() + 3 && (!last || box(row).right() > box(last).right()))
                last = row;
        QVERIFY(last);
        QStringList where;
        for (QQuickItem *row : app.items("lens-row"))
            where << QString::number(box(row).right());
        app.click(last);
        QTRY_VERIFY2(app.backend->lensInfo().value("id").toString() == "aaaa0012",
                     qPrintable(QString("list ends at %1 (contentX %2 of %3, width %4); rows end at %5")
                                    .arg(box(list).right()).arg(list->property("contentX").toReal())
                                    .arg(list->property("contentWidth").toReal()).arg(list->width()).arg(where.join(" "))));
    }

    void commentsLoadForTheVideoThatIsShowing()
    {
        App app;
        READY(app);
        // A video whose comments take long to come...
        app.play({{"id", "slowslowslo"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_VERIFY(app.backend->busy().contains("comments"));
        // ...is left for another, whose comments are asked for at once.
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QTRY_VERIFY_WITH_TIMEOUT(!app.backend->busy().contains("comments"), 1000);
        QTest::keyClick(app.window, Qt::Key_C);
        QTRY_COMPARE_WITH_TIMEOUT(app.items("comment").size(), 5, 3000);
    }

    void aPositionIsReportedAndTheFirstTimeTheWindowSaysSo()
    {
        // Something to play: the stand-in engine serves it as the video.
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QQuickItem *surface = app.item("surface");
        QObject *mpv = surface->property("mpv").value<QObject *>();
        QVERIFY(mpv);
        QTRY_VERIFY_WITH_TIMEOUT(mpv->property("duration").toDouble() > 0, 15000);
        QCOMPARE(app.asked("POST api/resume"), 0);
        QQuickItem *note = app.item("resume-notice");
        QVERIFY(note && !note->isVisible());

        // Going to another place in the video says where it is...
        QMetaObject::invokeMethod(surface, "seekTo", Q_ARG(QVariant, 40));
        QTRY_COMPARE(app.asked("POST api/resume"), 1);
        // ...and the first time a position is kept, the window says that it
        // is, in the engine's words, with the way to the setting.
        QTRY_VERIFY(note->isVisible());
        QCOMPARE(app.item("resume-notice-text")->property("text").toString(),
                 app.backend->privacy().value("pages").toMap().value("resume").toString());
        QTRY_COMPARE(app.backend->resume().first().toMap().value("id").toString(), QString("vid00000002"));
        // So does pausing...
        QTest::keyClick(app.window, Qt::Key_Space);
        QTRY_VERIFY(mpv->property("paused").toBool());
        QTRY_COMPARE(app.asked("POST api/resume"), 2);
        // ...a step with the keyboard...
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(app.asked("POST api/resume"), 3);
        // ...and leaving.
        app.click(app.item("resume-notice-settings"));
        QTRY_COMPARE(app.page(), QString("settings"));
        QTRY_COMPARE(app.asked("POST api/resume"), 4);
        QVERIFY(!note->isVisible());

        // It is said once.
        app.play({{"id", "vid00000003"}});
        QTRY_VERIFY(!surface->property("pending").toBool());
        QTRY_VERIFY_WITH_TIMEOUT(mpv->property("duration").toDouble() > 0, 15000);
        QMetaObject::invokeMethod(surface, "seekTo", Q_ARG(QVariant, 50));
        QTRY_COMPARE(app.asked("POST api/resume"), 5);
        QTRY_COMPARE(app.backend->resume().first().toMap().value("id").toString(), QString("vid00000003"));
        QVERIFY(!note->isVisible());
    }

    void qQuitsAtOnceWhenNothingIsPlaying()
    {
        App app;
        READY(app);
        QObject::disconnect(app.qml.get(), &QQmlEngine::quit, nullptr, nullptr);
        QSignalSpy quit(app.qml.get(), &QQmlApplicationEngine::quit);
        QTest::keyClick(app.window, Qt::Key_Q);
        QCOMPARE(quit.count(), 1);
    }

    void qReportsWhereAVideoIsBeforeItQuits()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}});
        READY(app);
        QObject::disconnect(app.qml.get(), &QQmlEngine::quit, nullptr, nullptr);
        QSignalSpy quit(app.qml.get(), &QQmlApplicationEngine::quit);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QQuickItem *surface = app.item("surface");
        QObject *mpv = surface->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mpv && mpv->property("duration").toDouble() > 0, 15000);
        QMetaObject::invokeMethod(surface, "seekTo", Q_ARG(QVariant, 40));
        QTRY_COMPARE(app.asked("POST api/resume"), 1);

        // Up to fifteen seconds of it would be lost without this: the position
        // goes to the engine, and the window quits once it has been heard.
        QTest::keyClick(app.window, Qt::Key_Q);
        QTRY_COMPARE(quit.count(), 1);
        QCOMPARE(app.asked("POST api/resume"), 2);
    }

    void qDoesNotWaitLongForAnEngineThatDoesNotAnswer()
    {
        QTemporaryDir dir;
        const QString clip = makeClip(dir);
        if (clip.isEmpty())
            QSKIP("ffmpeg couldn't make a clip to play");
        App app(false, QSize(1280, 720), {{"QVEIL_STANDIN_CLIP", clip}, {"QVEIL_STANDIN_SLOW_RESUME", "8"}});
        READY(app);
        QObject::disconnect(app.qml.get(), &QQmlEngine::quit, nullptr, nullptr);
        QSignalSpy quit(app.qml.get(), &QQmlApplicationEngine::quit);
        app.play({{"id", "vid00000002"}, {"title", "A dovetail box with four hand tools"}});
        QObject *mpv = app.item("surface")->property("mpv").value<QObject *>();
        QTRY_VERIFY_WITH_TIMEOUT(mpv && mpv->property("duration").toDouble() > 0, 15000);

        QElapsedTimer waited;
        waited.start();
        QTest::keyClick(app.window, Qt::Key_Q);
        QTRY_COMPARE(app.asked("POST api/resume"), 1);
        QTest::qWait(300);
        QCOMPARE(quit.count(), 0);
        QTRY_COMPARE_WITH_TIMEOUT(quit.count(), 1, 4000);
        QVERIFY2(waited.elapsed() < 5000, qPrintable(QString::number(waited.elapsed())));
    }

    void escapeFromTheSearchFieldGivesTheKeysBackToTheGrid()
    {
        App app;
        READY(app);
        QQuickItem *home = app.item("shelves-home");
        QTRY_COMPARE(app.backend->feed()->count(), 12);
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        // (Off the lead, onto the first shelf.)
        QTest::keyClick(app.window, Qt::Key_Down);
        QTRY_VERIFY(!app.item("shelves-home")->property("onLead").toBool());
        QTest::keyClick(app.window, '/');
        QTRY_VERIFY(app.window->property("typing").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!app.window->property("typing").toBool());
        QTRY_VERIFY(app.item("shelves")->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(home->property("column").toInt(), 1);
        // The same in the feed in full.
        app.seeAll();
        QQuickItem *grid = app.item("home-grid");
        QTRY_VERIFY(grid->hasActiveFocus());
        grid->setProperty("currentIndex", 0);
        QTest::keyClick(app.window, '/');
        QTRY_VERIFY(app.window->property("typing").toBool());
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(grid->hasActiveFocus());
        QTest::keyClick(app.window, Qt::Key_Right);
        QTRY_COMPARE(grid->property("currentIndex").toInt(), 1);
    }

    void aChannelOpenedFromSearchGoesBackToTheResults()
    {
        App app;
        READY(app);
        QMetaObject::invokeMethod(app.item("search"), "edit");
        app.type("rust");
        QTest::keyClick(app.window, Qt::Key_Return);
        QTRY_COMPARE(app.page(), QString("search"));
        QTRY_VERIFY(app.asked("GET api/search") >= 1);
        const int searches = app.asked("GET api/search");
        QMetaObject::invokeMethod(app.window, "openChannel", Q_ARG(QVariant, QString("UCaaaaaaaaaaaaaaaaaaaaaa")));
        QCOMPARE(app.page(), QString("channel"));
        QTest::keyClick(app.window, Qt::Key_Escape);
        // Back at the results, with what was searched for, and without asking again.
        QCOMPARE(app.page(), QString("search"));
        QCOMPARE(app.item("search")->property("text").toString(), QString("rust"));
        QCOMPARE(app.asked("GET api/search"), searches);
        // And from there, back to where the search started.
        QTest::keyClick(app.window, Qt::Key_Escape);
        QCOMPARE(app.page(), QString("home"));
    }

    void aLinkToAChannelsHandleOpensThatChannel()
    {
        App app;
        READY(app);
        app.play({{"id", "vid00000002"}});
        QTRY_VERIFY(!app.item("surface")->property("pending").toBool());
        QMetaObject::invokeMethod(app.item("watch"), "follow", Q_ARG(QVariant, QString("qveil:channel/%40lowlevelhours")));
        QTRY_COMPARE(app.page(), QString("channel"));
        // The page is the channel's, under its ID, so that it can be subscribed to and refreshed.
        QTRY_COMPARE(app.backend->channel().value("id").toString(), QString("UCaaaaaaaaaaaaaaaaaaaaaa"));
        QVERIFY(!app.item("errorBar")->isVisible());
    }
};

QVEIL_SUITE(WindowTests)
#include "window_tests.moc"
