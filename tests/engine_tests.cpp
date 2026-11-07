#include <QDir>
#include <QFile>
#include <QProcessEnvironment>
#include <QRegularExpression>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QtTest>

#include "backend.h"
#include "suite.h"

// The window against the real engine: build/qveil-core, started the way the
// app starts it. The other Backend tests use stand-in engines written in
// Python, which can only be as right as their author's idea of the engine.
class EngineTests : public QObject {
    Q_OBJECT

private slots:
    void theWindowAndTheEngineUnderstandEachOther() {
        const QString core = QDir(QCoreApplication::applicationDirPath()).absoluteFilePath("../qveil-core");
        if (!QFileInfo(core).isExecutable())
            QSKIP("build/qveil-core isn't built; bin/build builds it");

        // Everything the engine touches is in this directory: its settings
        // and data are not the user's, and its yt-dlp is the script below.
        QTemporaryDir root;
        QVERIFY(root.isValid());
        const QString config = root.filePath("config"), data = root.filePath("data"), cache = root.filePath("cache"),
                      run = root.filePath("run");
        for (const QString &dir : {config, data, cache, run})
            QVERIFY(QDir().mkpath(dir));
        QVERIFY(QFile::setPermissions(run, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        const QString ytdlp = QFINDTESTDATA("fake-ytdlp.sh");
        QVERIFY2(QFileInfo(ytdlp).isExecutable(), "tests/fake-ytdlp.sh is missing or not executable");
        QVERIFY(QDir().mkpath(config + "/qveil"));
        // The route is a proxy nothing listens on, so a feed's request for a
        // channel's RSS fails at once and the engine falls back to its yt-dlp,
        // and nothing in this test can reach YouTube.
        write(config + "/qveil/config.toml", "ytdlp = \"" + ytdlp.toUtf8() + "\"\nproxy = \"http://127.0.0.1:1\"\n");

        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("XDG_CONFIG_HOME", config);
        environment.insert("XDG_DATA_HOME", data);
        environment.insert("XDG_CACHE_HOME", cache);
        environment.insert("XDG_RUNTIME_DIR", run);

        Backend backend(environment, core);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 15000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.config().value("route").toString(), QString("proxy"), 10000);

        // Moving in: a Takeout export is read offline, and then the engine
        // fetches what is new (here both channels, through the fallback).
        const QString csv = root.filePath("subscriptions.csv");
        write(csv, "Channel Id,Channel Url,Channel Title\n"
                   "UCaaaaaaaaaaaaaaaaaaaaaa,http://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa,Alpha\n"
                   "UCbbbbbbbbbbbbbbbbbbbbbb,http://www.youtube.com/channel/UCbbbbbbbbbbbbbbbbbbbbbb,Beta\n");
        QSignalSpy notices(&backend, &Backend::notice);
        backend.importFile(QUrl::fromLocalFile(csv));
        QTRY_COMPARE_WITH_TIMEOUT(notices.count(), 1, 10000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.subs().size(), 2, 10000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 4, 20000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 20000);
        // Newest first, whichever channel it is from.
        QCOMPARE(backend.feed()->ids(), QStringList({"BBBBBBBBBB2", "AAAAAAAAAA2", "BBBBBBBBBB1", "AAAAAAAAAA1"}));
        QCOMPARE(backend.feed()->get(0).value("title").toString(), QString("Beta, newer"));
        QCOMPARE(backend.feed()->get(0).value("channel").toString(), QString("Beta"));
        QCOMPARE(backend.feed()->get(0).value("channel_id").toString(), QString("UCbbbbbbbbbbbbbbbbbbbbbb"));
        QCOMPARE(backend.feedInfo().value("channels").toInt(), 2);

        // A search: results one by one, with what a card shows.
        backend.search("canned");
        QTRY_COMPARE_WITH_TIMEOUT(backend.results()->count(), 2, 10000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 10000);
        const QVariantMap first = backend.results()->get(0);
        QCOMPARE(first.value("id").toString(), QString("SSSSSSSSSS1"));
        QCOMPARE(first.value("title").toString(), QString("First result"));
        QCOMPARE(first.value("views").toInt(), 1500);
        QCOMPARE(first.value("duration").toInt(), 61);
        QVERIFY(first.value("published").toLongLong() > 0);

        // Playing: the window gets loopback relay URLs, never the upstream's.
        QSignalSpy played(&backend, &Backend::playReady);
        backend.play("AAAAAAAAAA1");
        QTRY_COMPARE_WITH_TIMEOUT(played.count(), 1, 10000);
        const QVariantMap info = played.at(0).at(0).toMap();
        const QRegularExpression shape("^http://127\\.0\\.0\\.1:\\d+/[0-9a-f]+/s/([0-9a-f]+)/([va])$");
        const auto video = shape.match(info.value("video").toString());
        const auto audio = shape.match(info.value("audio").toString());
        QVERIFY2(video.hasMatch(), qPrintable(info.value("video").toString()));
        QVERIFY2(audio.hasMatch(), qPrintable(info.value("audio").toString()));
        QCOMPARE(video.captured(2), QString("v"));
        QCOMPARE(audio.captured(2), QString("a"));
        QCOMPARE(video.captured(1), audio.captured(1));
        QVERIFY(info.value("video").toString().startsWith(backend.thumbBase().chopped(QString("/thumb/").size())));
        QVERIFY(!info.value("video").toString().contains("upstream.invalid"));
        QCOMPARE(info.value("title").toString(), QString("Canned video"));
        QCOMPARE(info.value("id").toString(), QString("AAAAAAAAAA1"));
        QCOMPARE(info.value("duration").toInt(), 61);

        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
    }

private:
    static void write(const QString &path, const QByteArray &contents) {
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(contents);
    }
};

QVEIL_SUITE(EngineTests)
#include "engine_tests.moc"
