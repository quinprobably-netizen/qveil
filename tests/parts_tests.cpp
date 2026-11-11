#include <QFontDatabase>
#include <QFontInfo>
#include <QQmlComponent>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQuickItem>
#include <QQuickWindow>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QtTest>

#include <memory>

#include "suite.h"
#include "theme.h"

// The parts the window is built from, each on its own: a word you can press,
// a tab, a field, a bar. They are all the window has for controls, so what
// they do under the pointer and under the keyboard is tested here once.
class PartsTests : public QObject {
    Q_OBJECT

    // One part in a window of its own, with the theme it reads its looks from.
    struct Stage {
        QTemporaryDir themeDir;
        std::unique_ptr<Theme> theme;
        QQmlEngine engine;
        std::unique_ptr<QQuickWindow> window;
        QQuickItem *item = nullptr;
        QString error;

        // `qml` is the body of a QML document; it is loaded as a sibling of
        // the app's own files, so it names the parts as they do.
        explicit Stage(const QByteArray &qml)
        {
            theme = std::make_unique<Theme>(themeDir.path());
            engine.rootContext()->setContextProperty("theme", theme.get());
            QQmlComponent component(&engine);
            component.setData("import QtQuick\nimport Qveil\n" + qml, QUrl("qrc:/PartsStage.qml"));
            QObject *made = component.create();
            item = qobject_cast<QQuickItem *>(made);
            if (!item) {
                error = component.errorString();
                delete made;
                return;
            }
            window = std::make_unique<QQuickWindow>();
            window->resize(400, 300);
            item->setParentItem(window->contentItem());
            window->show();
            window->requestActivate();
        }

        QVariant style(const char *name)
        {
            QQmlComponent component(&engine);
            component.setData("import QtQuick\nimport Qveil\nQtObject { property var v: Style." + QByteArray(name) + " }",
                              QUrl("qrc:/PartsStyle.qml"));
            std::unique_ptr<QObject> probe(component.create());
            return probe ? probe->property("v") : QVariant();
        }
    };

    static void type(QWindow *window, const char *text)
    {
        for (const char *c = text; *c; ++c)
            QTest::keyClick(window, *c);
    }

    // How many items under `item` are called `name`.
    static int count(QQuickItem *item, const QString &name)
    {
        int found = item->objectName() == name ? 1 : 0;
        for (QQuickItem *child : item->childItems())
            found += count(child, name);
        return found;
    }

    static QPoint centre(QQuickItem *item) { return item->mapToScene(QPointF(item->width() / 2, item->height() / 2)).toPoint(); }

private slots:
    void anActIsAWordThatClicksByMouseAndKeyboard()
    {
        Stage stage("Act { objectName: \"act\"; text: \"Refresh\" }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        QVERIFY(QTest::qWaitForWindowExposed(stage.window.get()));
        QSignalSpy clicked(stage.item, SIGNAL(clicked()));
        QVERIFY(stage.item->width() > 0 && stage.item->height() == stage.style("control").toInt());

        QTest::mouseClick(stage.window.get(), Qt::LeftButton, {}, centre(stage.item));
        QCOMPARE(clicked.count(), 1);
        stage.item->forceActiveFocus();
        QTest::keyClick(stage.window.get(), Qt::Key_Return);
        QCOMPARE(clicked.count(), 2);
        QTest::keyClick(stage.window.get(), Qt::Key_Space);
        QCOMPARE(clicked.count(), 3);

        // One that is switched off does nothing, however it is pressed.
        stage.item->setEnabled(false);
        QTest::mouseClick(stage.window.get(), Qt::LeftButton, {}, centre(stage.item));
        QTest::keyClick(stage.window.get(), Qt::Key_Return);
        QCOMPARE(clicked.count(), 3);
    }

    void anActTakesTheHoverFillAndNothingRounder()
    {
        Stage stage("Act { text: \"Refresh\" }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        QVERIFY(QTest::qWaitForWindowExposed(stage.window.get()));
        // At rest it is only a word.
        QCOMPARE(stage.item->property("color").value<QColor>().alpha(), 0);
        QTest::mouseMove(stage.window.get(), centre(stage.item));
        QTRY_COMPARE(stage.item->property("color").value<QColor>(), stage.style("hover").value<QColor>());
        QCOMPARE(stage.item->property("radius").toReal(), stage.style("radius").toReal());

        Stage filled("Act { text: \"Subscribed\"; filled: true }");
        QVERIFY2(filled.item, qPrintable(filled.error));
        QCOMPARE(filled.item->property("color").value<QColor>(), filled.style("rest").value<QColor>());
    }

    void aTabTakesTheSelectedFillWhenItIsCurrent()
    {
        Stage stage("TabWord { text: \"Latest\"; current: true }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        QCOMPARE(stage.item->property("color").value<QColor>(), stage.style("sel").value<QColor>());
        QSignalSpy clicked(stage.item, SIGNAL(clicked()));
        QVERIFY(QTest::qWaitForWindowExposed(stage.window.get()));
        QTest::mouseClick(stage.window.get(), Qt::LeftButton, {}, centre(stage.item));
        QCOMPARE(clicked.count(), 1);
        // One that isn't current, and isn't pointed at, is only a word.
        stage.item->setProperty("current", false);
        QTest::mouseMove(stage.window.get(), QPoint(390, 290));
        QTRY_COMPARE(stage.item->property("color").value<QColor>().alpha(), 0);
    }

    void aFieldSubmitsOnEnterAndEscapeClearsThenLeaves()
    {
        Stage stage("Field { width: 300; placeholder: \"Search\" }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        QVERIFY(QTest::qWaitForWindowExposed(stage.window.get()));
        QSignalSpy submitted(stage.item, SIGNAL(submitted(QString)));
        QSignalSpy edited(stage.item, SIGNAL(edited(QString)));

        QTest::mouseClick(stage.window.get(), Qt::LeftButton, {}, centre(stage.item));
        QTRY_VERIFY(stage.item->property("editing").toBool());
        type(stage.window.get(), "abc");
        QCOMPARE(stage.item->property("text").toString(), QString("abc"));
        QCOMPARE(edited.count(), 3);
        QTest::keyClick(stage.window.get(), Qt::Key_Return);
        QCOMPARE(submitted.count(), 1);
        QCOMPARE(submitted.first().first().toString(), QString("abc"));

        QTest::keyClick(stage.window.get(), Qt::Key_Escape);
        QCOMPARE(stage.item->property("text").toString(), QString());
        QVERIFY(stage.item->property("editing").toBool());
        QTest::keyClick(stage.window.get(), Qt::Key_Escape);
        QVERIFY(!stage.item->property("editing").toBool());
        // An empty field submits nothing.
        QTest::mouseClick(stage.window.get(), Qt::LeftButton, {}, centre(stage.item));
        QTest::keyClick(stage.window.get(), Qt::Key_Return);
        QCOMPARE(submitted.count(), 1);
    }

    void aBarReportsWhereItWasPressedAndDrawsOneSegmentPerMark()
    {
        Stage stage("Bar { width: 300; from: 0; to: 200; value: 50; marks: [0, 0.25, 0.5] }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        QVERIFY(QTest::qWaitForWindowExposed(stage.window.get()));
        QCOMPARE(count(stage.item, "segment"), 3);

        QSignalSpy moved(stage.item, SIGNAL(moved(double)));
        const QPoint at = stage.item->mapToScene(QPointF(225, stage.item->height() / 2)).toPoint();
        QTest::mousePress(stage.window.get(), Qt::LeftButton, {}, at);
        QVERIFY(stage.item->property("pressed").toBool());
        QTest::mouseRelease(stage.window.get(), Qt::LeftButton, {}, at);
        QVERIFY(moved.count() >= 1);
        QVERIFY(qAbs(moved.last().first().toDouble() - 150.0) < 2.0);
        QVERIFY(!stage.item->property("pressed").toBool());

        // No marks at all is one plain bar.
        Stage plain("Bar { width: 300; to: 10 }");
        QVERIFY2(plain.item, qPrintable(plain.error));
        QCOMPARE(count(plain.item, "segment"), 1);
    }

    void everyPartUsesTheOneTypeface()
    {
        Stage stage("Column { Act { text: \"a\" }  TabWord { text: \"b\" }  Field { width: 200; placeholder: \"c\"; hint: \"/\" }"
                    "  SectionHead { width: 300; text: \"d\"; Act { text: \"e\" } }  Tip { text: \"f\"; shown: true }  Rule { width: 100 } }");
        QVERIFY2(stage.item, qPrintable(stage.error));
        const QString family = stage.style("font").toString();
        // JetBrains Mono where it is installed, else the desktop's interface typeface.
        QVERIFY(!family.isEmpty());
        QCOMPARE(family, QFontDatabase::hasFamily(jetBrains) ? jetBrains : stage.theme->uiFamily());
        int texts = 0;
        for (QQuickItem *child : stage.item->findChildren<QQuickItem *>()) {
            if (!child->property("font").isValid())
                continue;
            ++texts;
            QCOMPARE(child->property("font").value<QFont>().family(), family);
        }
        QVERIFY2(texts >= 7, qPrintable(QString::number(texts)));
    }

    void theTypefaceIsJetBrainsMonoWhereverItIsInstalled()
    {
        if (!QFontDatabase::hasFamily(jetBrains))
            QSKIP("JetBrainsMono Nerd Font isn't installed here");
        Stage stage("Item {}");
        QVERIFY2(stage.item, qPrintable(stage.error));
        // Whatever the desktop's interface font is.
        QCOMPARE(stage.style("font").toString(), jetBrains);
        // Bold is a face it has (SemiBold, or Bold where only Regular and Bold
        // are installed), never Regular smeared.
        QFont strong(jetBrains);
        strong.setPixelSize(stage.style("body").toInt());
        strong.setWeight(QFont::Weight(stage.style("strong").toInt()));
        const QString face = QFontInfo(strong).styleName();
        QVERIFY2(face == "SemiBold" || face == "Bold", qPrintable(face));
    }

private:
    inline static const QString jetBrains = QStringLiteral("JetBrainsMono Nerd Font");
};

QVEIL_SUITE(PartsTests)
#include "parts_tests.moc"
