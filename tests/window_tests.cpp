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
        // No test may leave a QML warning behind.
        QTest::failOnWarning(QRegularExpression("qrc:/|\\.qml|QML|TypeError|ReferenceError"));
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
};

QVEIL_SUITE(WindowTests)
#include "window_tests.moc"
