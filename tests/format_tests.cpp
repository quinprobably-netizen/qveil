#include <QCoreApplication>
#include <QDateTime>
#include <QLocale>
#include <QQmlComponent>
#include <QQmlEngine>
#include <QtTest>

#include <memory>

#include "suite.h"
#include "translator.h"

// What a card says under its title, and how a stream's start is put in
// words. Format.js is the window's own, with qsTr and Qt as QML has them.
class FormatTests : public QObject {
    Q_OBJECT

    // Made in initTestCase: the suite object exists before the application does.
    std::unique_ptr<Translator> translator;
    std::unique_ptr<QQmlEngine> engine;
    std::unique_ptr<QObject> format;

    QString call(const char *function, const QVariant &a, const QVariant &b = {}, const QVariant &c = {})
    {
        QVariant out;
        QMetaObject::invokeMethod(format.get(), function, Q_RETURN_ARG(QVariant, out), Q_ARG(QVariant, a), Q_ARG(QVariant, b),
                                  Q_ARG(QVariant, c));
        return out.toString();
    }
    static qint64 now() { return QDateTime::currentSecsSinceEpoch(); }

private slots:
    void initTestCase()
    {
        // English, whatever language this machine has picked.
        translator = std::make_unique<Translator>("/nonexistent", ":/i18n");
        QCoreApplication::installTranslator(translator.get());
        engine = std::make_unique<QQmlEngine>();
        QQmlComponent component(engine.get());
        component.setData("import QtQuick\nimport \"Format.js\" as Format\nQtObject {\n"
                          "  function startsIn(a, b, c) { return Format.startsIn(a) }\n"
                          "  function dayTime(a, b, c) { return Format.dayTime(a) }\n"
                          "  function meta(a, b, c) { return Format.meta(a, b, c) }\n"
                          "  function waitLine(a, b, c) { return Format.waitLine(a, b) }\n"
                          "}",
                          QUrl("qrc:/FormatStage.qml"));
        format.reset(component.create());
        QVERIFY2(format, qPrintable(component.errorString()));
    }
    void cleanupTestCase()
    {
        format.reset();
        engine.reset();
        QCoreApplication::removeTranslator(translator.get());
        translator.reset();
    }

    void aStartIsSaidInMinutesThenHoursThenDays()
    {
        QCOMPARE(call("startsIn", now() + 5 * 60 + 10), QString("in 5 min"));
        QCOMPARE(call("startsIn", now() + 2 * 3600 + 600), QString("in 2 h"));
        QCOMPARE(call("startsIn", now() + 47 * 3600), QString("in 47 h"));
        QCOMPARE(call("startsIn", now() + 3 * 86400 + 3600), QString("in 3 days"));
        // Never "in 0 min", and nothing for a start that has gone by.
        QCOMPARE(call("startsIn", now() + 10), QString("in 1 min"));
        QCOMPARE(call("startsIn", now() - 60), QString());
    }

    void aStartsDayAndTimeAreLocal()
    {
        const qint64 at = now() + 2 * 86400;
        QCOMPARE(call("dayTime", at), QLocale().toString(QDateTime::fromSecsSinceEpoch(at), "ddd HH:mm"));
        QVERIFY(QRegularExpression("^\\S+ \\d\\d:\\d\\d$").match(call("dayTime", at)).hasMatch());
    }

    void theLineUnderAStreamsTitleSaysWhatItIs()
    {
        QCOMPARE(call("meta", QVariantMap{{"live", "now"}, {"watching", 863}, {"views", 863}}, now() - 600),
                 QString("live now · 863 watching"));
        QCOMPARE(call("meta", QVariantMap{{"live", "now"}, {"watching", 1200}}, 0), QString("live now · 1.2K watching"));
        QCOMPARE(call("meta", QVariantMap{{"live", "now"}, {"watching", 863}}, 0, true), QString("live now · 863"));
        QCOMPARE(call("meta", QVariantMap{{"live", "now"}}, 0), QString("live now"));

        const qint64 starts = now() + 2 * 3600 + 600;
        const QString when = call("dayTime", starts);
        QCOMPARE(call("meta", QVariantMap{{"live", "upcoming"}, {"starts", starts}}, now() - 86400),
                 QString("Starts in 2 h · ") + when);
        // Past its time, and not yet begun.
        QCOMPARE(call("meta", QVariantMap{{"live", "upcoming"}, {"starts", now() - 120}}, 0),
                 QString("Starting soon · ") + call("dayTime", now() - 120));

        QCOMPARE(call("meta", QVariantMap{{"streamed", true}, {"views", 59000}}, now() - 3 * 86400 - 60),
                 QString("Streamed 3 days ago · 59K views"));
        QCOMPARE(call("meta", QVariantMap{{"streamed", true}}, now() - 86400 - 60), QString("Streamed 1 day ago"));
        // Any other video is as it was.
        QCOMPARE(call("meta", QVariantMap{{"views", 59000}}, now() - 3 * 86400 - 60), QString("3 days ago · 59K views"));
    }

    void theWaitingRoomSaysWhenAndHowOftenItAsks()
    {
        const qint64 starts = 1800000000;
        QCOMPARE(call("waitLine", starts, starts - 3600), call("dayTime", starts) + QString(" · plays from the start when it begins"));
        QCOMPARE(call("waitLine", 0, starts), QString("Waiting for it to start · checking every minute"));
        // Late: every minute, as the engine asks, and every 5 minutes from half an hour on.
        QCOMPARE(call("waitLine", starts, starts + 20), QString("Should have started 1 min ago · checking every minute"));
        QCOMPARE(call("waitLine", starts, starts + 12 * 60 + 30), QString("Should have started 12 min ago · checking every minute"));
        QCOMPARE(call("waitLine", starts, starts + 29 * 60 + 59), QString("Should have started 29 min ago · checking every minute"));
        QCOMPARE(call("waitLine", starts, starts + 30 * 60), QString("Should have started 30 min ago · checking every 5 minutes"));
        QCOMPARE(call("waitLine", starts, starts + 3 * 3600 + 34 * 60), QString("Should have started 3 h 34 min ago · checking every 5 minutes"));
    }
};

QVEIL_SUITE(FormatTests)
#include "format_tests.moc"
