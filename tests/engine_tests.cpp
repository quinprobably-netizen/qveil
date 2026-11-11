#include <QDir>
#include <QFile>
#include <QNetworkAccessManager>
#include <QNetworkProxy>
#include <QNetworkReply>
#include <QProcessEnvironment>
#include <QRegularExpression>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QtTest>

#include <functional>
#include <memory>

#include "backend.h"
#include "suite.h"

namespace {

const QString Alpha = "UCaaaaaaaaaaaaaaaaaaaaaa", Beta = "UCbbbbbbbbbbbbbbbbbbbbbb", Gamma = "UCcccccccccccccccccccccc";

// Waits, with the event loop running, for `condition`, and fails the test
// saying which one it was if it never comes true.
#define WAIT_FOR(condition, milliseconds) \
    QVERIFY2(QTest::qWaitFor([&] { return (condition); }, milliseconds), #condition)

// The window against the real engine: build/qveil-core, started the way the
// app starts it, with everything it touches in a directory of its own. Its
// settings and data are not the user's, and its yt-dlp is tests/fake-ytdlp.sh,
// which keeps a log of what it was asked and can be told to wait or fail.
// The route is a proxy nothing listens on, so a feed's request for a
// channel's RSS fails at once and the engine falls back to yt-dlp, and nothing
// a test does can reach YouTube.
class Rig {
public:
    // The engine of this build tree, or empty when it isn't built.
    static QString core() {
        const QString path = QDir(QCoreApplication::applicationDirPath()).absoluteFilePath("../qveil-core");
        return QFileInfo(path).isExecutable() ? path : QString();
    }

    // `settings` is more lines for config.toml.
    explicit Rig(const QByteArray &settings = {}) {
        // Only the engine, on loopback, is ever spoken to.
        web.setProxy(QNetworkProxy::NoProxy);
        ok = root.isValid() && !core().isEmpty();
        for (const char *name : {"config/qveil", "data", "cache", "run", "bin"})
            ok = ok && QDir().mkpath(root.filePath(name));
        ok = ok && QFile::setPermissions(root.filePath("run"), QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        const QString script = QFINDTESTDATA("fake-ytdlp.sh");
        // A copy beside the files that steer it (see fake-ytdlp.sh).
        ok = ok && !script.isEmpty() && QFile::copy(script, ytdlp());
        ok = ok && QFile::setPermissions(ytdlp(), QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        ok = ok && write(root.filePath("config/qveil/config.toml"),
                         "ytdlp = \"" + ytdlp().toUtf8() + "\"\nproxy = \"http://127.0.0.1:1\"\n" + settings);

        environment = QProcessEnvironment::systemEnvironment();
        environment.insert("XDG_CONFIG_HOME", root.filePath("config"));
        environment.insert("XDG_DATA_HOME", root.filePath("data"));
        environment.insert("XDG_CACHE_HOME", root.filePath("cache"));
        environment.insert("XDG_RUNTIME_DIR", root.filePath("run"));
    }

    // Starts the engine and waits until the window knows its settings.
    bool start() {
        if (!ok)
            return false;
        backend = std::make_unique<Backend>(environment, core());
        backend->start();
        return QTest::qWaitFor([this] { return backend->ready() && backend->config().value("route") == "proxy"; }, 20000);
    }

    // Whether this engine speaks the contract of the engine that sends its own
    // privacy text, codes with its errors, and streams and joins refreshes. The
    // tests of those things need it, and skip on an older one.
    bool modern() const { return backend->config().contains("privacy"); }

    Backend &window() { return *backend; }

    // The engine's address, without the trailing slash.
    QString base() const { return backend->thumbBase().chopped(QString("/thumb/").size()); }

    QString ytdlp() const { return root.filePath("bin/yt-dlp"); }
    // Tells the fake yt-dlp what to do (see fake-ytdlp.sh).
    bool steer(const QString &file, const QByteArray &text) { return write(root.filePath("bin/" + file), text); }
    QStringList calls() const {
        QFile log(root.filePath("bin/calls"));
        return log.open(QIODevice::ReadOnly) ? QString::fromUtf8(log.readAll()).split('\n', Qt::SkipEmptyParts)
                                             : QStringList();
    }
    int calls(const QString &kind) const { return calls().filter(QRegularExpression("^" + kind + " ")).size(); }

    // Moves in: subscribes to these channels from a Takeout export, which is
    // read without any request; the window then refreshes the feed.
    bool importChannels(const QStringList &ids) {
        QByteArray csv = "Channel Id,Channel Url,Channel Title\n";
        for (const QString &id : ids) {
            const QByteArray name = id == Alpha ? "Alpha" : id == Beta ? "Beta" : id.toUtf8();
            csv += id.toUtf8() + ",http://www.youtube.com/channel/" + id.toUtf8() + "," + name + "\n";
        }
        const QString file = root.filePath("subscriptions.csv");
        if (!write(file, csv))
            return false;
        backend->importFile(QUrl::fromLocalFile(file));
        return true;
    }

    // A GET the window isn't making, to the same engine.
    QNetworkReply *fetch(const QString &path) { return web.get(QNetworkRequest(QUrl(base() + "/" + path))); }

    QNetworkAccessManager web;
    bool ok = false;
    QTemporaryDir root;
    QProcessEnvironment environment;
    // Last, so that it is the first to go: the engine must be stopped before
    // the directory it works in is removed.
    std::unique_ptr<Backend> backend;

    static bool write(const QString &path, const QByteArray &contents) {
        QFile file(path);
        return file.open(QIODevice::WriteOnly) && file.write(contents) == contents.size();
    }
};

} // namespace

class EngineTests : public QObject {
    Q_OBJECT

private slots:
    void init() {
        // Nothing here may reach the network but the engine on loopback.
        QNetworkProxy::setApplicationProxy(QNetworkProxy::NoProxy);
    }

    void theWindowAndTheEngineUnderstandEachOther() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY2(rig.start(), "the engine didn't start");
        Backend &window = rig.window();

        // Moving in: a Takeout export is read offline, and then the engine
        // fetches what is new (here both channels, through the fallback).
        QSignalSpy notices(&window, &Backend::notice);
        QVERIFY(rig.importChannels({Alpha, Beta}));
        WAIT_FOR(notices.count() == 1, 10000);
        WAIT_FOR(window.subs().size() == 2, 10000);
        WAIT_FOR(window.feed()->count() == 4, 20000);
        WAIT_FOR(!window.busy().contains("feed"), 20000);
        // Newest first, whichever channel it is from. The Shorts of the two
        // channels are left out, as the window asks.
        QCOMPARE(window.feed()->ids(), QStringList({"BBBBBBBBBB2", "AAAAAAAAAA2", "BBBBBBBBBB1", "AAAAAAAAAA1"}));
        QCOMPARE(window.feed()->get(0).value("title").toString(), QString("Beta, newer"));
        QCOMPARE(window.feed()->get(0).value("channel").toString(), QString("Beta"));
        QCOMPARE(window.feed()->get(0).value("channel_id").toString(), Beta);
        QCOMPARE(window.feedInfo().value("channels").toInt(), 2);

        // A search: results one by one, with what a card shows.
        window.search("canned");
        WAIT_FOR(window.results()->count() == 2, 10000);
        WAIT_FOR(!window.busy().contains("search"), 10000);
        const QVariantMap first = window.results()->get(0);
        QCOMPARE(first.value("id").toString(), QString("SSSSSSSSSS1"));
        QCOMPARE(first.value("title").toString(), QString("First result"));
        QCOMPARE(first.value("views").toInt(), 1500);
        QCOMPARE(first.value("duration").toInt(), 61);
        QVERIFY(first.value("published").toLongLong() > 0);

        // Playing: the window gets loopback relay URLs, never the upstream's.
        QSignalSpy played(&window, &Backend::playReady);
        window.play("AAAAAAAAAA1");
        WAIT_FOR(played.count() == 1, 10000);
        const QVariantMap info = played.at(0).at(0).toMap();
        const QRegularExpression shape("^http://127\\.0\\.0\\.1:\\d+/[0-9a-f]+/s/([0-9a-f]+)/([va])$");
        const auto video = shape.match(info.value("video").toString());
        const auto audio = shape.match(info.value("audio").toString());
        QVERIFY2(video.hasMatch(), qPrintable(info.value("video").toString()));
        QVERIFY2(audio.hasMatch(), qPrintable(info.value("audio").toString()));
        QCOMPARE(video.captured(2), QString("v"));
        QCOMPARE(audio.captured(2), QString("a"));
        QCOMPARE(video.captured(1), audio.captured(1));
        QVERIFY(info.value("video").toString().startsWith(rig.base()));
        QVERIFY(!info.value("video").toString().contains("127.0.0.1:1/"));
        QCOMPARE(info.value("title").toString(), QString("Canned video"));
        QCOMPARE(info.value("id").toString(), QString("AAAAAAAAAA1"));
        QCOMPARE(info.value("duration").toInt(), 61);

        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));
    }

    void aRefreshLeavesFreshChannelsAlone() {
        // The Refresh button after startup must not repeat a burst of
        // requests for exactly your channels (the README promises it): the
        // window asks for the channels that have gone stale, and none have.
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QVERIFY(rig.importChannels({Alpha, Beta}));
        WAIT_FOR(window.subs().size() == 2 && window.feed()->count() == 4 && !window.busy().contains("feed"), 20000);
        QCOMPARE(rig.calls("channel"), 2);

        window.refreshFeed(false, false);
        window.refreshFeed(true, false);
        WAIT_FOR(!window.busy().contains("feed") && !window.busy().contains("feed-cache"), 10000);
        QCOMPARE(rig.calls("channel"), 2);
        QCOMPARE(window.feed()->count(), 4);
    }

    void subscribingFetchesOnlyTheNewChannel() {
        // The other channels are as fresh as they were, and fetching them
        // again would be a burst of requests for the whole list.
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QVERIFY(rig.importChannels({Alpha, Beta}));
        WAIT_FOR(window.subs().size() == 2 && window.feed()->count() == 4 && !window.busy().contains("feed"), 20000);
        const int before = rig.calls().size();

        QSignalSpy notices(&window, &Backend::notice);
        window.subscribe(Gamma);
        WAIT_FOR(notices.count() == 1, 10000);
        WAIT_FOR(window.subs().size() == 3 && window.feed()->count() == 5 && !window.busy().contains("feed"), 10000);
        // Whatever it takes to look the channel up and fetch its videos, and
        // nothing for the other two.
        const QStringList after = rig.calls().mid(before);
        QVERIFY(!after.isEmpty());
        for (const QString &call : after)
            QVERIFY2(call.contains(Gamma), qPrintable(call));
    }

    void aShelfIsWhatALensAlreadyFoundAndAsksNobody() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QSignalSpy added(&window, &Backend::lensAdded);
        window.addLens("bread, sourdough");
        WAIT_FOR(added.count() == 1, 10000);
        const QString id = added.first().first().toString();
        window.openLens(id, 2);
        WAIT_FOR(window.lensVideos()->count() > 0 && !window.busy().contains("lens"), 20000);
        const int before = rig.calls().size();

        QMetaObject::invokeMethod(&window, "loadShelf", Q_ARG(QString, id));
        WAIT_FOR(window.property("shelves").toMap().contains(id), 10000);
        const QVariantList videos = window.property("shelves").toMap().value(id).toList();
        QCOMPARE(videos.size(), window.lensVideos()->count());
        QVERIFY(!videos.first().toMap().value("id").toString().isEmpty());
        QVERIFY(!videos.first().toMap().value("why").toList().isEmpty());
        // It is read from what was kept: yt-dlp is not run for it.
        QCOMPARE(rig.calls().size(), before);
    }

    void aHandleOpensTheChannelItNames() {
        // Links to a channel are mostly written with its handle.
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QSignalSpy errors(&window, &Backend::errorChanged);
        window.openChannel("@gamma");
        WAIT_FOR(window.channel().value("id").toString() == Gamma, 10000);
        QCOMPARE(window.channelVideos()->count(), 1);
        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));
        // Something that is neither is still refused, without asking anybody.
        const int before = rig.calls().size();
        window.openChannel("not a channel");
        WAIT_FOR(!window.error().isEmpty(), 10000);
        QCOMPARE(rig.calls().size(), before);
    }

    void aClosedPlayerDropsTheAnswerOfItsVideo() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QSignalSpy ready(&window, &Backend::playReady);
        QVERIFY(rig.steer("resolve-delay", "0.6"));

        // The player is closed while the video is looked up: its answer
        // must not open it again.
        window.play("dQw4w9WgXcQ");
        window.cancelPlay();
        WAIT_FOR(!window.busy().contains("play"), 5000);
        QTest::qWait(1000);
        QCOMPARE(ready.count(), 0);

        window.play("dQw4w9WgXcQ");
        WAIT_FOR(ready.count() == 1, 10000);
    }

    void aFailedLookupSaysSoOnlyForTheLatestVideo() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        // Every lookup fails, the first one slowly.
        QVERIFY(rig.steer("resolve-error", "ERROR: [youtube] x: no such video\n"));
        QVERIFY(rig.steer("resolve-delay-once", "1"));
        QSignalSpy failed(&window, &Backend::taskFailed);

        // The first video is still being looked up when the second is asked
        // for; only the second one's failure is the player's business.
        window.play("aaaaaaaaaaa");
        WAIT_FOR(rig.calls("resolve") == 1, 5000);
        window.play("bbbbbbbbbbb");
        WAIT_FOR(failed.count() == 1, 10000);
        QCOMPARE(failed.at(0).at(0).toString(), QString("play"));
        WAIT_FOR(!window.busy().contains("play"), 5000);
        QTest::qWait(1200);
        QCOMPARE(failed.count(), 1);
        QVERIFY2(window.error().contains("no such video"), qPrintable(window.error()));
    }

    void theShortsChoiceDecidesWhichFeedLands() {
        // A refresh is slow, and Shorts is switched on while it runs. Its
        // answer was asked for without Shorts, so it must not replace the
        // feed that has them; the window reads the cache again for the choice
        // that stands. And while a refresh runs, the cache answer for the new
        // choice lands: it is not held back for the refresh.
        Rig rig("feed_workers = 1\n");
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QVERIFY(rig.steer("channel-delay", "0.8"));
        QVERIFY(rig.importChannels({Alpha, Beta}));

        // One channel has been fetched and saved, the other is on its way.
        WAIT_FOR(rig.calls("channel") == 2, 10000);
        window.refreshFeed(false, false);
        WAIT_FOR(window.feed()->count() == 2, 10000);
        QVERIFY(window.busy().contains("feed"));

        window.refreshFeed(false, true);
        // The Short of the finished channel, while the refresh still runs.
        WAIT_FOR(window.feed()->count() == 3, 10000);
        QVERIFY(window.busy().contains("feed"));

        // Both channels, Shorts included, once the refresh is over; the
        // refresh's own answer (without them) never showed.
        WAIT_FOR(!window.busy().contains("feed"), 20000);
        WAIT_FOR(window.feed()->count() == 6, 10000);
        QVERIFY(window.feed()->ids().contains("ASHORTSSSS1"));
        QVERIFY(window.feed()->ids().contains("BSHORTSSSS1"));
    }

    void aHungUpPlayIsNeverRecorded() {
        // The player is closed while the engine looks the video up. Nobody is
        // waiting for it, so the engine stops, and what it never answered is
        // not something the user watched.
        Rig rig("history = true\n");
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that cancels a lookup when the window hangs up");
        Backend &window = rig.window();
        QVERIFY(rig.steer("resolve-delay", "2"));

        window.play("dQw4w9WgXcQ");
        WAIT_FOR(rig.calls("resolve") == 1, 5000);
        window.cancelPlay();
        // Past the end of the lookup, which would have recorded it.
        QTest::qWait(3500);
        window.loadHistory();
        WAIT_FOR(!window.busy().contains("history"), 5000);
        QVERIFY(window.history().isEmpty());

        // A lookup left alone is recorded, so an empty history above says something.
        QVERIFY(rig.steer("resolve-delay", "0.2"));
        QSignalSpy played(&window, &Backend::playReady);
        window.play("dQw4w9WgXcQ");
        WAIT_FOR(played.count() == 1, 10000);
        WAIT_FOR(window.history().size() == 1, 10000);
    }

    void aRefreshSaysHowFarItIs() {
        // Declared first, so that it outlives the window that reports into it.
        QList<QPair<int, int>> seen;
        Rig rig("feed_workers = 1\n");
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that streams a refresh's progress");
        Backend &window = rig.window();
        QVERIFY(rig.steer("channel-delay", "0.3"));

        connect(&window, &Backend::refreshProgressChanged, this, [&] { seen.append({window.refreshDone(), window.refreshTotal()}); });
        QVERIFY(rig.importChannels({Alpha, Beta, Gamma}));
        WAIT_FOR(window.feed()->count() == 5 && !window.busy().contains("feed"), 20000);

        QVERIFY(!seen.isEmpty());
        int done = 0;
        for (const auto &[at, of] : std::as_const(seen)) {
            if (of == 0)
                continue;
            QCOMPARE(of, 3);
            QVERIFY2(at >= done, "progress went backwards");
            done = at;
        }
        QCOMPARE(done, 3);
        // And it is over when the refresh is.
        QCOMPARE(window.refreshTotal(), 0);
        QCOMPARE(window.refreshDone(), 0);
        QCOMPARE(window.feedInfo().value("channels").toInt(), 3);
    }

    void aRefreshThatKeepsStreamingOutlivesTheIdleTimeout() {
        // Three channels take 1.8 s, longer than the 1 s the window waits for
        // a silent engine; it only hears from the engine as each channel is
        // done, and that is often enough.
        Rig rig("feed_workers = 1\n");
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that streams a refresh's progress");
        Backend &window = rig.window();
        window.setIdleTimeout(1000);
        QVERIFY(rig.steer("channel-delay", "0.6"));
        QVERIFY(rig.importChannels({Alpha, Beta, Gamma}));
        WAIT_FOR(window.feed()->count() == 5 && !window.busy().contains("feed"), 20000);
        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));

        // An engine that goes quiet for longer is given up on: a fourth
        // channel takes 2 s, and nothing is heard from the engine meanwhile.
        QVERIFY(rig.steer("channel-delay", "2"));
        QSignalSpy failed(&window, &Backend::taskFailed);
        QVERIFY(rig.importChannels({Alpha, Beta, Gamma, "UCdddddddddddddddddddddd"}));
        WAIT_FOR(failed.count() == 1, 10000);
        QCOMPARE(failed.at(0).at(0).toString(), QString("feed"));
        QVERIFY(!window.error().isEmpty());
    }

    void twoRefreshesAreOneBurst() {
        // A second request for a refresh that is running joins it, rather
        // than making the same requests for the same channels again.
        Rig rig("feed_workers = 1\n");
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that runs one refresh for everyone who asks");
        Backend &window = rig.window();
        QVERIFY(rig.steer("channel-delay", "0.5"));

        // The window's refresh, as startup and the Refresh button ask for it.
        QVERIFY(rig.importChannels({Alpha, Beta, Gamma}));
        WAIT_FOR(rig.calls("channel") >= 1, 10000);
        std::unique_ptr<QNetworkReply> joined(rig.fetch("api/feed?refresh=1&shorts=0&stream=1"));
        WAIT_FOR(joined->isFinished(), 20000);
        WAIT_FOR(window.feed()->count() == 5 && !window.busy().contains("feed"), 20000);

        // Both heard the feed; the channels were fetched once.
        QCOMPARE(joined->error(), QNetworkReply::NoError);
        const QList<QByteArray> lines = joined->readAll().trimmed().split('\n');
        QVERIFY2(lines.last().contains("\"result\""), lines.last().constData());
        QCOMPARE(rig.calls("channel"), 3);
    }

    void errorsCarryACodeToTheWindow() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that sends a code with its errors");
        Backend &window = rig.window();

        struct Case {
            const char *said;
            const char *code;
            bool offersRoute;
        };
        // What yt-dlp says, and what the window is to make of it. Only the
        // errors a different route may get past offer to change it.
        const Case cases[] = {
            {"ERROR: [youtube] x: Sign in to confirm you're not a bot", "bot_check", true},
            {"ERROR: [youtube] x: HTTP Error 429: Too Many Requests", "rate_limited", true},
            // The route here is a proxy, and nothing listens on it.
            {"ERROR: [youtube] x: Unable to download API page: Connection refused", "proxy_refused", true},
            {"ERROR: [youtube] x: Connection refused 127.0.0.1:9050", "tor_down", true},
            {"ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.",
             "age_restricted", false},
            {"ERROR: [youtube] x: No supported JavaScript runtime could be found", "deno_missing", false},
            {"ERROR: [youtube] x: Unable to extract initial data; please report this issue", "ytdlp_outdated", false},
        };
        for (const Case &c : cases) {
            QVERIFY(rig.steer("resolve-error", QByteArray(c.said) + "\n"));
            window.clearError();
            QSignalSpy failed(&window, &Backend::taskFailed);
            window.play("dQw4w9WgXcQ");
            WAIT_FOR(failed.count() == 1, 10000);
            QVERIFY2(!window.error().isEmpty(), c.code);
            QCOMPARE(window.errorCode(), QString(c.code));
            QCOMPARE(window.errorOffersRoute(), c.offersRoute);
        }
    }

    void whatIsTypedInASearchReachesTheEngineAsTyped() {
        // A plus is a plus: a query string leaves it as it is, and the engine
        // would read it as a space ("c++ tutorial" searched for "c   tutorial").
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();

        window.search("c++ 1+1=2 100% 50%25 a&b #c");
        WAIT_FOR(rig.calls("search") == 1, 10000);
        WAIT_FOR(!window.busy().contains("search"), 10000);
        // The fake yt-dlp logs the target the engine gave it, quoted.
        QCOMPARE(rig.calls().last(), QString("search 'ytsearch40:c++ 1+1=2 100% 50%25 a&b #c'"));
    }

    void aRouteChangedUnderARequestIsNotAnError() {
        // The user did it: they chose another route while a search was still
        // running on the old one, and the engine ended it. They know.
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        if (!rig.modern())
            QSKIP("needs the engine that sends a code with its errors");
        Backend &window = rig.window();
        QVERIFY(rig.steer("search-delay", "3"));
        QSignalSpy failed(&window, &Backend::taskFailed);

        window.search("canned");
        WAIT_FOR(rig.calls("search") == 1, 5000);
        // Another proxy that nothing listens on: a different route, and no traffic.
        window.setConfig({{"route", "proxy"}, {"proxy", "http://127.0.0.1:2"}});
        WAIT_FOR(failed.count() == 1, 10000);
        WAIT_FOR(!window.busy().contains("search"), 5000);
        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));
    }

    void thePlayAnswerCarriesTheDetails() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QSignalSpy ready(&window, &Backend::playReady);
        window.play("dQw4w9WgXcQ");
        WAIT_FOR(ready.count() == 1, 10000);
        const QVariantMap info = ready.first().first().toMap();
        QVERIFY(info.value("description").toString().startsWith("About this video."));
        QCOMPARE(info.value("views").toInt(), 59000);
        const QVariantList chapters = info.value("chapters").toList();
        QCOMPARE(chapters.size(), 2);
        QCOMPARE(chapters.at(1).toMap().value("title").toString(), QString("Middle"));
        QCOMPARE(chapters.at(1).toMap().value("start").toInt(), 60);
    }

    void commentsLoadOnRequestAndClosingHangsUp() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QVERIFY(window.comments().isEmpty());
        QCOMPARE(rig.calls("comments"), 0);

        window.loadComments("dQw4w9WgXcQ", "top", 40, false);
        WAIT_FOR(!window.comments().isEmpty(), 10000);
        const QVariantMap answer = window.comments();
        QCOMPARE(answer.value("video").toString(), QString("dQw4w9WgXcQ"));
        QCOMPARE(answer.value("sort").toString(), QString("top"));
        QCOMPARE(answer.value("count").toInt(), 1204);
        const QVariantList items = answer.value("items").toList();
        QCOMPARE(items.size(), 3);
        QVERIFY(items.first().toMap().value("pinned").toBool());
        QVERIFY(items.first().toMap().value("byUploader").toBool());
        QVERIFY(items.at(1).toMap().value("replies").toList().isEmpty());
        QCOMPARE(rig.calls("comments"), 1);

        // With replies, the thread that has one shows it.
        window.loadComments("dQw4w9WgXcQ", "top", 40, true);
        WAIT_FOR(window.comments().value("replies").toBool(), 10000);
        QCOMPARE(window.comments().value("items").toList().at(1).toMap().value("replies").toList().size(), 1);

        // Closed while a slow lookup runs: nothing is shown, and nothing is waited for.
        QVERIFY(rig.steer("comments-delay", "5"));
        window.loadComments("dQw4w9WgXcQ", "new", 40, false);
        WAIT_FOR(window.busy().contains("comments"), 2000);
        window.closeComments();
        QVERIFY(window.comments().isEmpty());
        WAIT_FOR(!window.busy().contains("comments"), 1000);
        QTest::qWait(300);
        QVERIFY(window.comments().isEmpty());
        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));
    }

    void aPositionIsNotedOfferedAndForgotten() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QVERIFY(window.resume().isEmpty());
        const QVariantMap video{{"id", "dQw4w9WgXcQ"}, {"title", "T"}, {"channel", "C"}};

        // Before the length is known there is nothing to note, and nothing is asked.
        window.noteResume(video, 845, 0);
        window.noteResume(video, 845, 2463);
        WAIT_FOR(window.resume().size() == 1, 5000);
        const QVariantMap kept = window.resume().first().toMap();
        QCOMPARE(kept.value("pos").toInt(), 845);
        QCOMPARE(kept.value("title").toString(), QString("T"));
        QVERIFY(kept.value("expires").toLongLong() > kept.value("at").toLongLong());

        QSignalSpy ready(&window, &Backend::playReady);
        window.play("dQw4w9WgXcQ");
        WAIT_FOR(ready.count() == 1, 10000);
        QCOMPARE(ready.first().first().toMap().value("resume").toInt(), 845);

        // Watched to the end: it is forgotten without being asked to.
        window.noteResume(video, 2460, 2463);
        WAIT_FOR(window.resume().isEmpty(), 5000);
        window.noteResume(video, 845, 2463);
        WAIT_FOR(window.resume().size() == 1, 5000);
        window.forgetResume("dQw4w9WgXcQ");
        WAIT_FOR(window.resume().isEmpty(), 5000);
        QVERIFY2(window.error().isEmpty(), qPrintable(window.error()));
    }

    void lensesAreAddedOpenedAndRemoved() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        QSignalSpy added(&window, &Backend::lensAdded);
        window.addLens("bread, sourdough");
        WAIT_FOR(added.count() == 1 && window.lenses().size() == 1, 5000);
        const QString id = added.first().first().toString();
        QVERIFY(!id.isEmpty());
        const QVariantMap lens = window.lenses().first().toMap();
        QCOMPARE(lens.value("id").toString(), id);
        QCOMPARE(lens.value("searches").toStringList(), QStringList({"bread", "sourdough"}));
        // Adding one asks YouTube for nothing.
        QCOMPARE(rig.calls("search"), 0);

        QList<QPair<int, int>> seen;
        connect(&window, &Backend::lensProgressChanged, this, [&] { seen.append({window.lensDone(), window.lensTotal()}); });
        window.openLens(id, 1);
        WAIT_FOR(window.lensVideos()->count() > 0, 10000);
        QCOMPARE(rig.calls("search"), 2);
        QCOMPARE(window.lensInfo().value("id").toString(), id);
        QCOMPARE(window.lensInfo().value("text").toString(), QString("bread, sourdough"));
        QVERIFY(window.lensInfo().value("fetchedAt").toLongLong() > 0);
        const QStringList why = window.lensVideos()->get(0).value("why").toStringList();
        QVERIFY2(!why.isEmpty() && (why.first() == "bread" || why.first() == "sourdough"), qPrintable(why.join(",")));
        QVERIFY2(seen.contains({2, 2}), "the refresh said when both searches were done");
        WAIT_FOR(window.lensTotal() == 0 && !window.busy().contains("lens"), 5000);
        // Opened again while fresh: answered from what is stored.
        window.openLens(id, 1);
        WAIT_FOR(!window.busy().contains("lens"), 5000);
        QCOMPARE(rig.calls("search"), 2);

        // Rules alone are not a lens: the engine says what is missing.
        window.addLens("no music");
        WAIT_FOR(window.error().startsWith("Say what it should be about"), 5000);
        QCOMPARE(window.lenses().size(), 1);
        window.clearError();

        window.removeLens(id);
        WAIT_FOR(window.lenses().isEmpty(), 5000);
        QVERIFY(window.lensInfo().isEmpty());
        QCOMPARE(window.lensVideos()->count(), 0);
    }

    void readingFollowsTheNewestTextOnly() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        Backend &window = rig.window();
        window.read("talks over 30 min");
        WAIT_FOR(window.reading().value("text").toString() == "talks over 30 min", 5000);
        QVERIFY(window.reading().value("rules").toBool());
        QCOMPARE(window.reading().value("tags").toList().size(), 2);
        QCOMPARE(window.reading().value("searches").toStringList(), QStringList({"talks"}));

        // Typed quickly: only the last text's reading is ever shown.
        QStringList shown;
        connect(&window, &Backend::readingChanged, this, [&] { shown << window.reading().value("text").toString(); });
        window.read("a");
        window.read("ab");
        window.read("abc");
        WAIT_FOR(window.reading().value("text").toString() == "abc", 5000);
        QTest::qWait(200);
        QCOMPARE(shown, QStringList({"abc"}));
        QVERIFY(!window.reading().value("rules").toBool());

        // A lens has to be about something; in the search box the same words are only words.
        window.read("no music", true);
        WAIT_FOR(window.reading().value("error").toString().startsWith("Say what it should be about"), 5000);
        window.read("no music");
        WAIT_FOR(!window.reading().contains("error"), 5000);
        QVERIFY(!window.reading().value("rules").toBool());
        // Nothing to read: nothing shown, and nothing asked.
        window.read("   ");
        QVERIFY(window.reading().isEmpty());
        QCOMPARE(rig.calls().size(), 0);
    }

    void theEnginesNewPrivacyLinesReachTheWindow() {
        Rig rig;
        if (Rig::core().isEmpty())
            QSKIP("build/qveil-core isn't built; bin/build builds it");
        QVERIFY(rig.start());
        const QVariantMap pages = rig.window().privacy().value("pages").toMap();
        QVERIFY(pages.value("resume").toString().contains("7 days"));
        QVERIFY(pages.value("comments").toString().contains("only when you open them"));
        QVERIFY(pages.value("lenses").toString().contains("one search per topic"));
        QCOMPARE(rig.window().config().value("resumeDays").toInt(), 7);
        // An engine that sends none of them leaves them empty, not missing.
        const QVariantMap none = Backend::privacyOf({}).value("pages").toMap();
        for (const char *key : {"resume", "comments", "lenses"})
            QVERIFY(none.contains(key) && none.value(key).toString().isEmpty());
    }
};

QVEIL_SUITE(EngineTests)
#include "engine_tests.moc"
