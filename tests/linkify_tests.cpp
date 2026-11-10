#include <QFile>
#include <QJSEngine>
#include <QJSValue>
#include <QtTest>

#include <memory>

#include "suite.h"

// Descriptions and comments are other people's text. Linkify.js decides what
// in them becomes something to click, and everything else must stay words.
class LinkifyTests : public QObject {
    Q_OBJECT

    // Made in initTestCase: the suite object exists before the application does.
    std::unique_ptr<QJSEngine> engine;

    QJSValue call(const char *function, const QJSValueList &arguments) { return engine->globalObject().property(function).call(arguments); }

    // "kind:text" for every part of `text`.
    QStringList kinds(const QString &text)
    {
        QStringList found;
        const QJSValue parts = call("parts", {text});
        for (int i = 0; i < parts.property("length").toInt(); ++i)
            found << parts.property(i).property("kind").toString();
        return found;
    }
    QJSValue part(const QString &text, const QString &kind, int nth = 0)
    {
        const QJSValue parts = call("parts", {text});
        for (int i = 0; i < parts.property("length").toInt(); ++i)
            if (parts.property(i).property("kind").toString() == kind && nth-- == 0)
                return parts.property(i);
        return {};
    }

private slots:
    void cleanupTestCase() { engine.reset(); }

    void initTestCase()
    {
        engine = std::make_unique<QJSEngine>();
        QFile file(QString(SRCDIR) + "/Linkify.js");
        QVERIFY(file.open(QIODevice::ReadOnly));
        // The library pragma is QML's, not JavaScript's.
        const QString source = QString::fromUtf8(file.readAll()).replace(".pragma library", "");
        const QJSValue loaded = engine->evaluate(source, "Linkify.js");
        QVERIFY2(!loaded.isError(), qPrintable(loaded.toString()));
    }

    void timestampsAndYouTubeLinksBecomeActionsAndNothingElseDoes()
    {
        const QString text = "Intro 0:00, deep dive 1:02:03 https://youtu.be/dQw4w9WgXcQ?t=5 https://example.com/a?b=1 UCx";
        QCOMPARE(kinds(text), QStringList({"text", "time", "text", "time", "text", "video", "text", "link", "text"}));
        QCOMPARE(part(text, "time", 0).property("seconds").toInt(), 0);
        QCOMPARE(part(text, "time", 1).property("seconds").toInt(), 3723);
        QCOMPARE(part(text, "video").property("id").toString(), QString("dQw4w9WgXcQ"));
        QCOMPARE(part(text, "link").property("url").toString(), QString("https://example.com/a?b=1"));
        // Put back together, the parts are the text.
        QString joined;
        const QJSValue parts = call("parts", {text});
        for (int i = 0; i < parts.property("length").toInt(); ++i)
            joined += parts.property(i).property("text").toString();
        QCOMPARE(joined, text);
    }

    void everyKindOfYouTubeLinkIsReducedToWhatItNames()
    {
        for (const char *url : {"https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=TRACK&pp=x", "https://youtube.com/shorts/dQw4w9WgXcQ",
                                "https://m.youtube.com/live/dQw4w9WgXcQ?feature=share", "http://youtu.be/dQw4w9WgXcQ"})
            QCOMPARE(part(url, "video").property("id").toString(), QString("dQw4w9WgXcQ"));
        QCOMPARE(part("see https://www.youtube.com/channel/UCsBjURrPoezykLs9EqgamOA.", "channel").property("id").toString(),
                 QString("UCsBjURrPoezykLs9EqgamOA"));
        QCOMPARE(part("https://www.youtube.com/@fireship/videos", "channel").property("id").toString(), QString("@fireship"));
        // Another site that only has YouTube in its name is another site.
        QCOMPARE(kinds("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"), QStringList({"link"}));
        QCOMPARE(kinds("https://notyoutube.com/watch?v=dQw4w9WgXcQ"), QStringList({"link"}));
        // A full stop after a link belongs to the sentence.
        QCOMPARE(part("Slides: https://example.com/slides.", "link").property("url").toString(), QString("https://example.com/slides"));
    }

    void markupIsShownNotRun()
    {
        const QString html = call("html", {"<script>alert(1)</script> <a href='javascript:x'>y</a> & 5 < 6", "#ffffff"}).toString();
        QVERIFY2(html.contains("&lt;script&gt;") && html.contains("&amp;"), qPrintable(html));
        QVERIFY2(!html.contains("<script") && !html.contains("<a "), qPrintable(html));
        // The only links are the ones made here, and they only lead back into the app.
        const QString linked = call("html", {"at 12:40 see https://example.com/\"onmouseover=\"x and https://youtu.be/dQw4w9WgXcQ\nnext line", "#ffffff"}).toString();
        QVERIFY2(linked.contains("<a href=\"qveil:time/760\">12:40</a>"), qPrintable(linked));
        QVERIFY2(linked.contains("<a href=\"qveil:video/dQw4w9WgXcQ\">"), qPrintable(linked));
        QVERIFY2(linked.contains("href=\"qveil:copy/"), qPrintable(linked));
        QVERIFY2(linked.contains("<br>"), qPrintable(linked));
        QCOMPARE(linked.count("href=\""), linked.count("href=\"qveil:"));
        QVERIFY2(!linked.contains("onmouseover=\"x"), qPrintable(linked));
    }

    void aNumberThatOnlyLooksLikeATimeIsText()
    {
        const QString text = "ratio 16:9, score 3:2, at 12:40. Then 1:99 and 25:61:00, v1:23, 10:30:45:10";
        QCOMPARE(kinds(text).count("time"), 1);
        QCOMPARE(part(text, "time").property("seconds").toInt(), 760);
    }

    void aVeryLongLineAndRightToLeftTextSurvive()
    {
        const QString unbroken(20000, 'x');
        QCOMPARE(kinds(unbroken), QStringList({"text"}));
        QCOMPARE(part(unbroken, "text").property("text").toString().size(), 20000);
        const QString hebrew = QString::fromUtf8("שלום עולם, זה סרטון");
        QCOMPARE(part(hebrew, "text").property("text").toString(), hebrew);
        QCOMPARE(kinds(""), QStringList());
    }
};

QVEIL_SUITE(LinkifyTests)
#include "linkify_tests.moc"
