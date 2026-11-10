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
};

QVEIL_SUITE(WindowTests)
#include "window_tests.moc"
