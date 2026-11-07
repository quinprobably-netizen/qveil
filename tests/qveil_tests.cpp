#include <QDir>
#include <QLocalServer>
#include <QLocalSocket>
#include <QProcessEnvironment>
#include <QFile>
#include <QTemporaryDir>
#include <QtTest>

#include <sys/stat.h>

#include "backend.h"
#include "datadirs.h"
#include "mpvitem.h"
#include "theme.h"
#include "translator.h"

class QveilTests : public QObject {
    Q_OBJECT

private slots:
    void acceptsOnlyLoopbackEngines()
    {
        const QByteArray good = "{\"base\":\"http://127.0.0.1:4242/abc\"}";
        const QByteArray remote = "{\"base\":\"http://203.0.113.9:4242/abc\"}";
        const QByteArray lookalike = "{\"base\":\"http://127.0.0.1.evil.example/abc\"}";
        QCOMPARE(Backend::parseReady(good), QString("http://127.0.0.1:4242/abc"));
        QVERIFY(Backend::parseReady(remote).isEmpty());
        QVERIFY(Backend::parseReady(lookalike).isEmpty());
        QVERIFY(Backend::parseReady("not json").isEmpty());
    }

    void talksToTheEngine()
    {
        // A stand-in engine: prints a base URL, then serves one config reply.
        QTemporaryDir dir;
        const QString fake = dir.filePath("qveil-core");
        write(fake, "#!/bin/sh\n"
                    "python3 -c 'import http.server,json,sys\n"
                    "class H(http.server.BaseHTTPRequestHandler):\n"
                    "  def do_GET(s):\n"
                    "    b=json.dumps({\"route\":\"tor\",\"history\":False}).encode()\n"
                    "    s.send_response(200);s.send_header(\"Content-Length\",str(len(b)));s.end_headers();s.wfile.write(b)\n"
                    "  def log_message(s,*a): pass\n"
                    "srv=http.server.HTTPServer((\"127.0.0.1\",0),H)\n"
                    "print(json.dumps({\"base\":\"http://127.0.0.1:%d/t\"%srv.server_port}),flush=True)\n"
                    "srv.serve_forever()'\n");
        QVERIFY(QFile::setPermissions(fake, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));

        Backend backend(QProcessEnvironment::systemEnvironment(), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.config().value("route").toString(), QString("tor"), 5000);
        QVERIFY(backend.thumbBase().startsWith("http://127.0.0.1:"));
        QVERIFY(backend.error().isEmpty());
    }

    void restartsTheEngineOnceAndReloads()
    {
        // The first engine answers the config request and then dies. Its
        // successor must be started, and the window must reload everything
        // it shows (config, subscriptions, cached feed) from it.
        QTemporaryDir dir;
        const QString fake = engine(dir, "import http.server,json,os,sys,threading\n"
                                         "marker=os.environ['QVEIL_TEST_DIR']+'/started'\n"
                                         "first=not os.path.exists(marker)\n"
                                         "open(marker,'w').close()\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    path=s.path.split('?')[0]\n"
                                         "    if path.endswith('/api/subs'): body=[{'id':'UC1','name':'One'}]\n"
                                         "    elif path.endswith('/api/feed'): body={'channels':1,'entries':[{'id':'v1','title':'T'}]}\n"
                                         "    else: body={'route':'tor','instance':1 if first else 2}\n"
                                         "    b=json.dumps(body).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers()\n"
                                         "    s.wfile.write(b);s.wfile.flush()\n"
                                         "    if first and path.endswith('/api/config'): threading.Timer(0.05,lambda:os._exit(3)).start()\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_DIR", dir.path());

        Backend backend(environment, fake);
        QSignalSpy ready(&backend, &Backend::readyChanged);
        backend.start();
        QTRY_COMPARE_WITH_TIMEOUT(backend.config().value("instance").toInt(), 2, 10000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.subs().size(), 1, 5000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed().size(), 1, 5000);
        QVERIFY(backend.ready());
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
        // Ready, lost, ready again.
        QCOMPARE(ready.size(), 3);
    }

    void givesUpWhenTheEngineDiesAgain()
    {
        QTemporaryDir dir;
        const QString log = dir.filePath("starts");
        const QString fake = engine(dir, "import json,os,sys,time\n"
                                         "open(os.environ['QVEIL_TEST_LOG'],'a').write('x\\n')\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:9/t'}),flush=True)\n"
                                         "sys.stderr.write('out of coffee\\n');sys.stderr.flush()\n"
                                         "time.sleep(0.2)\n"
                                         "sys.exit(3)\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_LOG", log);

        Backend backend(environment, fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.error().contains("stopped again"), 10000);
        QVERIFY2(backend.error().contains("out of coffee"), qPrintable(backend.error()));
        QVERIFY(!backend.ready());

        // No third start, and a request that can't be served says why
        // instead of being ignored.
        QTest::qWait(600);
        QFile starts(log);
        QVERIFY(starts.open(QIODevice::ReadOnly));
        QCOMPARE(starts.readAll().count('\n'), 2);
        backend.clearError();
        backend.loadSubs();
        QVERIFY(backend.error().contains("stopped again"));
    }

    void refreshesOneFeedAtATime()
    {
        // A second full refresh while one runs (the R shortcut, say) is dropped.
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        const QString fake = engine(dir, "import http.server,json,os,time\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    open(os.environ['QVEIL_TEST_LOG'],'a').write(s.path+'\\n')\n"
                                         "    if 'refresh=1' in s.path: time.sleep(0.5)\n"
                                         "    b=json.dumps({'channels':0,'entries':[]}).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_LOG", log);

        Backend backend(environment, fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        backend.refreshFeed(true, false);
        backend.refreshFeed(true, false);
        QVERIFY(backend.busy().contains("feed"));
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);

        QFile requests(log);
        QVERIFY(requests.open(QIODevice::ReadOnly));
        QCOMPARE(requests.readAll().count("refresh=1"), 1);

        // Once it is done, the next one goes through.
        backend.refreshFeed(true, false);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        requests.seek(0);
        QCOMPARE(requests.readAll().count("refresh=1"), 2);
    }

    void anEngineThatNeverGotReadyIsNotRestarted()
    {
        // A restart would die the same way (a bad config, say), so the first
        // death says what the engine said instead.
        QTemporaryDir dir;
        const QString log = dir.filePath("starts");
        const QString fake = engine(dir, "import os,sys\n"
                                         "open(os.environ['QVEIL_TEST_LOG'],'a').write('x\\n')\n"
                                         "sys.stderr.write('bad config: unknown key\\n');sys.stderr.flush()\n"
                                         "sys.exit(2)\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_LOG", log);

        Backend backend(environment, fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.error().contains("unknown key"), 10000);
        QVERIFY2(!backend.error().contains("stopped again"), qPrintable(backend.error()));
        QVERIFY(!backend.ready());

        QTest::qWait(600);
        QCOMPARE(lines(log), 1);
        backend.clearError();
        backend.loadSubs();
        QVERIFY(backend.error().contains("unknown key"));
    }

    void anEngineThatStaysUpGetsItsRestartBackLater()
    {
        // Each engine is ready for 0.6 s and then dies. With a restart only
        // for the first death, the third would never start; once an engine
        // has been ready for a while (0.25 s here, a minute in the app), its
        // death counts as a new failure again.
        QTemporaryDir dir;
        const QString log = dir.filePath("starts");
        const QString fake = engine(dir, "import json,os,sys,time\n"
                                         "open(os.environ['QVEIL_TEST_LOG'],'a').write('x\\n')\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:9/t'}),flush=True)\n"
                                         "time.sleep(0.6)\n"
                                         "sys.exit(3)\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_LOG", log);

        Backend backend(environment, fake);
        backend.setStableAfter(250);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(lines(log) >= 3, 10000);
        // The stand-in serves nothing, so requests to it fail and may show an
        // error; what matters is that the window never gave up on the engine.
        QVERIFY2(!backend.error().contains("again"), qPrintable(backend.error()));
    }

    void feedRefreshesAskForWhatTheyMean()
    {
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        Backend backend(logged(log), requestLogger(dir, ""));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        backend.refreshFeed(false, false);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed-cache"), 5000);
        backend.refreshFeed(true, false);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        backend.refreshFeed(true, false, true);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);

        const QStringList feeds = requests(log, "/api/feed");
        QCOMPARE(feeds.size(), 3);
        // From the cache alone.
        QVERIFY(feeds[0].contains("refresh=0") && !feeds[0].contains("force"));
        // Startup: the channels that have gone stale, not all of them.
        QVERIFY(feeds[1].contains("refresh=1") && !feeds[1].contains("force") && !feeds[1].contains("channel="));
        // The Refresh button: everything.
        QVERIFY(feeds[2].contains("refresh=1") && feeds[2].contains("force=1"));
    }

    void subscribingRefreshesOnlyThatChannel()
    {
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        Backend backend(logged(log), requestLogger(dir, ""));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        backend.subscribe("@someone");
        QTRY_VERIFY_WITH_TIMEOUT(!requests(log, "/api/feed").isEmpty(), 5000);
        const QStringList feeds = requests(log, "/api/feed");
        QCOMPARE(feeds.size(), 1);
        QVERIFY2(feeds[0].contains("refresh=1") && feeds[0].contains("channel=UC0123456789abcdefghij")
                     && !feeds[0].contains("force"),
                 qPrintable(feeds[0]));
        // Neither a stale-channels nor a full refresh.
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        QCOMPARE(requests(log, "/api/feed").size(), 1);
    }

    void importingRefreshesTheStaleChannels()
    {
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        const QString file = dir.filePath("subscriptions.csv");
        write(file, "Channel Id,Channel Url,Channel Title\nUC0123456789abcdefghij,x,X\n");
        Backend backend(logged(log), requestLogger(dir, ""));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        backend.importFile(QUrl::fromLocalFile(file));
        QTRY_VERIFY_WITH_TIMEOUT(!requests(log, "/api/feed").isEmpty(), 5000);
        const QString feed = requests(log, "/api/feed").first();
        QVERIFY2(feed.contains("refresh=1") && !feed.contains("force") && !feed.contains("channel="), qPrintable(feed));
    }

    void overlappingSubscriptionChangesAllGetTheirAnswers()
    {
        // Each change is slow (0.3 s in the stand-in engine) and they overlap.
        // None may drop another's notice, whichever kind it is.
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        const QString file = dir.filePath("subscriptions.csv");
        write(file, "Channel Id,Channel Url,Channel Title\nUC0123456789abcdefghij,x,X\n");
        Backend backend(logged(log), requestLogger(dir, "time.sleep(0.3)"));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        QSignalSpy notices(&backend, &Backend::notice);
        backend.subscribe("@one");
        backend.subscribe("@two");
        backend.unsubscribe("UC0123456789abcdefghij");
        backend.importFile(QUrl::fromLocalFile(file));
        QVERIFY(backend.busy().contains("subscribe"));
        QVERIFY(backend.busy().contains("unsubscribe"));
        QVERIFY(backend.busy().contains("import"));
        QTRY_COMPARE_WITH_TIMEOUT(notices.size(), 4, 10000);
    }

    void aChangedRouteIsSaidOnceAndOnlyWhenItMoved()
    {
        // The window closes the player on this signal, so it must come for a
        // new route or a new proxy, and not for the other settings or a
        // route chosen again.
        QTemporaryDir dir;
        const QString fake = engine(dir, "import http.server,json,sys\n"
                                         "state={'route':'direct','proxy':'','thumbnails':True}\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def reply(s):\n"
                                         "    b=json.dumps(state).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def do_GET(s): s.reply()\n"
                                         "  def do_POST(s):\n"
                                         "    n=int(s.headers.get('Content-Length') or 0)\n"
                                         "    patch=json.loads(s.rfile.read(n) or b'{}')\n"
                                         "    state.update({k:v for k,v in patch.items() if k!='proxy'})\n"
                                         "    if 'proxy' in patch: state['proxy']=patch['proxy'].replace('secret','***')\n"
                                         "    s.reply()\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        Backend backend(QProcessEnvironment::systemEnvironment(), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.config().value("route").toString(), QString("direct"), 5000);

        QSignalSpy moved(&backend, &Backend::routeChanged);
        auto change = [&](const QVariantMap &patch) {
            QSignalSpy saved(&backend, &Backend::configChanged);
            backend.setConfig(patch);
            QTRY_COMPARE_WITH_TIMEOUT(saved.size(), 1, 5000);
        };
        change({{"thumbnails", false}});
        QCOMPARE(moved.size(), 0);
        change({{"route", "direct"}});
        QCOMPARE(moved.size(), 0);
        change({{"route", "tor"}});
        QCOMPARE(moved.size(), 1);
        change({{"route", "proxy"}, {"proxy", "socks5h://u:secret@host:1"}});
        QCOMPARE(moved.size(), 2);
        // The saved proxy comes back redacted; sending that back keeps it.
        const QString redacted = backend.config().value("proxy").toString();
        QVERIFY(redacted.contains("***"));
        change({{"route", "proxy"}, {"proxy", redacted}});
        QCOMPARE(moved.size(), 2);
        // A different address that redacts to the same text is still another proxy.
        change({{"route", "proxy"}, {"proxy", "socks5h://u:secret@host:1/"}});
        QCOMPARE(moved.size(), 3);
    }

    void reportsAMissingEngine()
    {
        Backend backend(QProcessEnvironment::systemEnvironment(), "/nonexistent/qveil-core");
        backend.start();
        QTRY_VERIFY(!backend.error().isEmpty());
        QVERIFY(!backend.ready());
    }

    void theOldDirectoriesMoveToTheNewNames()
    {
        QTemporaryDir dir;
        const QString base = dir.path();
        QDir(base).mkpath("veil");
        write(base + "/veil/subscriptions.json", "{}");
        write(base + "/veil/veil.conf", "[General]\n");

        DataDirs::migrate(base, false);
        QVERIFY(!QFileInfo::exists(base + "/veil"));
        QVERIFY(QFileInfo(base + "/qveil/subscriptions.json").isFile());

        // An existing qveil directory is never overwritten, and for config and
        // data the old one is kept: it may hold something the user wants.
        QDir(base).mkpath("veil");
        write(base + "/veil/subscriptions.json", "stale");
        DataDirs::migrate(base, false);
        QVERIFY(QFileInfo(base + "/veil/subscriptions.json").isFile());
        QCOMPARE(read(base + "/qveil/subscriptions.json"), QByteArray("{}"));
    }

    void anOldCacheNextToANewOneIsDeleted()
    {
        // Qt makes the new cache directory on its own, so both exist. The old
        // one is regenerable, and records what was watched.
        QTemporaryDir dir;
        const QString base = dir.path();
        QDir(base).mkpath("veil/nested");
        write(base + "/veil/nested/feed.json", "{}");
        QDir(base).mkpath("qveil");
        write(base + "/qveil/qmlcache", "fresh");

        DataDirs::migrate(base, true);
        QVERIFY(!QFileInfo::exists(base + "/veil"));
        QCOMPARE(read(base + "/qveil/qmlcache"), QByteArray("fresh"));
    }

    void startingUpMovesAndLocksDownTheDirectories()
    {
        QTemporaryDir dir;
        const QString config = dir.filePath("config"), data = dir.filePath("data"), cache = dir.filePath("cache");
        for (const QString &base : {config, data, cache})
            QDir(base).mkpath("veil");
        write(config + "/veil/veil.conf", "[General]\nvolume=33\n");
        write(cache + "/veil/feed.json", "{}");
        QDir(cache).mkpath("qveil");
        write(data + "/veil/subscriptions.json", "{}");

        const EnvironmentVariables xdg{{"XDG_CONFIG_HOME", config}, {"XDG_DATA_HOME", data}, {"XDG_CACHE_HOME", cache}};
        DataDirs::prepare();

        QVERIFY(!QFileInfo::exists(config + "/veil") && !QFileInfo::exists(data + "/veil"));
        QVERIFY(!QFileInfo::exists(cache + "/veil"));
        QVERIFY(QFileInfo(data + "/qveil/subscriptions.json").isFile());
        // The window's settings file followed the rename and is private.
        QCOMPARE(read(config + "/qveil/qveil.conf"), QByteArray("[General]\nvolume=33\n"));
        QCOMPARE(mode(config + "/qveil/qveil.conf"), 0600);
        for (const QString &base : {config, data, cache})
            QCOMPARE(mode(base + "/qveil"), 0700);
    }

    void themeFallsBack()
    {
        QTemporaryDir dir;
        Theme theme(dir.path());
        QCOMPARE(theme.accent(), QString("#FFD60A"));
        QCOMPARE(theme.accentForeground(), QString("black"));
    }

    void themeReadsAccent()
    {
        QTemporaryDir dir;
        QDir(dir.path()).mkpath("theme");
        QFile colors(dir.path() + "/theme/colors.toml");
        QVERIFY(colors.open(QIODevice::WriteOnly));
        colors.write("accent = \"#112233\"\n");
        colors.close();

        Theme theme(dir.path());
        QCOMPARE(theme.accent(), QString("#112233"));
        QCOMPARE(theme.accentForeground(), QString("white"));
    }

    void themeFollowsHyprlandRounding()
    {
        // The fake hyprctl lives apart from the theme directory: the theme
        // watches its directory and re-reads rounding on any change there,
        // which would make rewriting the response look like a reload.
        QTemporaryDir dir, tools;
        const QString response = tools.filePath("response.json"), command = tools.filePath("hyprctl");
        auto write = [](const QString &path, const QByteArray &data) {
            QFile file(path);
            return file.open(QIODevice::WriteOnly) && file.write(data) == data.size();
        };
        QVERIFY(write(command, "#!/bin/sh\ncat '" + response.toUtf8() + "'\n"));
        QVERIFY(QFile::setPermissions(command, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        QVERIFY(write(response, "{\"int\":4}"));

        // Hyprland's event socket (short path: socket paths are limited).
        QTemporaryDir sockets("/tmp/qveil-sock-XXXXXX");
        QVERIFY(sockets.isValid());
        QLocalServer hyprland;
        QVERIFY(hyprland.listen(sockets.filePath("events.sock")));

        Theme theme(dir.path(), nullptr, command, sockets.filePath("events.sock"));
        // The first query reads the rounding.
        QTRY_COMPARE(theme.radius(), 4);
        QTRY_VERIFY(hyprland.hasPendingConnections());
        QLocalSocket *events = hyprland.nextPendingConnection();
        auto send = [&](const QByteArray &data) {
            events->write(data);
            events->flush();
        };

        // It isn't asked again on a timer: this used to poll every second.
        QVERIFY(write(response, "{\"int\":6}"));
        QTest::qWait(1500);
        QCOMPARE(theme.radius(), 4);
        // Other events don't count either.
        send("workspace>>2\nactivewindow>>x,y\n");
        QTest::qWait(300);
        QCOMPARE(theme.radius(), 4);

        // A reload does, even when its line arrives in pieces.
        send("configrel");
        QTest::qWait(100);
        QCOMPARE(theme.radius(), 4);
        send("oaded>>\n");
        QTRY_COMPARE(theme.radius(), 6);

        // A reload that lands while a query is still running isn't lost.
        QVERIFY(write(response, "{\"int\":9}"));
        send("configreloaded>>\nconfigreloaded>>\n");
        QTRY_COMPARE(theme.radius(), 9);
    }

    void translatorFallsBackToEnglish()
    {
        QTemporaryDir dir;
        Translator translator(dir.path() + "/language", dir.path());
        QCOMPARE(translator.language(), QString("en"));
        QVERIFY(translator.translate("Main", "Save").isNull());
    }

    void translatorFollowsTheLanguageFile()
    {
        QTemporaryDir dir;
        write(dir.path() + "/language", "pt-BR\n");
        write(dir.path() + "/pt.json", R"({"Save": "Guardar", "Open": "Abrir"})");
        write(dir.path() + "/pt-BR.json", R"({"Save": "Salvar"})");

        Translator translator(dir.path() + "/language", dir.path());
        QCOMPARE(translator.language(), QString("pt-BR"));
        QCOMPARE(translator.translate("Main", "Save"), QString("Salvar"));
        QCOMPARE(translator.translate("Main", "Open"), QString("Abrir"));
        QVERIFY(translator.translate("Main", "Quit").isNull());
    }

    void translatorPicksPluralForms()
    {
        QTemporaryDir dir;
        write(dir.path() + "/language", "ru\n");
        write(dir.path() + "/en.json", R"({"%n clips": {"one": "%n clip", "other": "%n clips"}})");
        write(dir.path() + "/ru.json", R"({"%n clips": {"one": "%n клип", "few": "%n клипа", "many": "%n клипов", "other": "%n клипа"}})");

        Translator translator(dir.path() + "/language", dir.path());
        QCOMPARE(translator.translate("Main", "%n clips", nullptr, 21), QString("%n клип"));
        QCOMPARE(translator.translate("Main", "%n clips", nullptr, 22), QString("%n клипа"));
        QCOMPARE(translator.translate("Main", "%n clips", nullptr, 25), QString("%n клипов"));
        QCOMPARE(Translator::pluralCategory("ar", 11), QString("many"));
        QCOMPARE(Translator::pluralCategory("ja", 1), QString("other"));
    }

private:
    // Sets environment variables for its lifetime, and puts back what was there.
    struct EnvironmentVariables {
        EnvironmentVariables(std::initializer_list<std::pair<const char *, QString>> values)
        {
            for (const auto &[name, value] : values) {
                saved.append({name, qEnvironmentVariableIsSet(name), qgetenv(name)});
                qputenv(name, value.toUtf8());
            }
        }
        ~EnvironmentVariables()
        {
            for (const Saved &old : saved) {
                if (old.wasSet)
                    qputenv(old.name, old.value);
                else
                    qunsetenv(old.name);
            }
        }
        struct Saved {
            const char *name;
            bool wasSet;
            QByteArray value;
        };
        QList<Saved> saved;
    };

    // A stand-in qveil-core that logs every request ("GET /api/feed?...") to
    // the file named by QVEIL_TEST_LOG, and answers each with something the
    // window accepts. `delay` is a line of Python run before every POST.
    static QString requestLogger(const QTemporaryDir &dir, const QByteArray &delay)
    {
        return engine(dir, "import http.server,json,os,time\n"
                           "class H(http.server.BaseHTTPRequestHandler):\n"
                           "  def answer(s):\n"
                           "    open(os.environ['QVEIL_TEST_LOG'],'a').write(s.command+' '+s.path+'\\n')\n"
                           "    path=s.path.split('?')[0]\n"
                           "    if path.endswith('/api/subs/add'): body={'added':True,'sub':{'id':'UC0123456789abcdefghij','name':'Some Channel'}}\n"
                           "    elif path.endswith('/api/subs/rm'): body={'name':'Some Channel'}\n"
                           "    elif path.endswith('/api/subs/import'): body={'added':1}\n"
                           "    elif path.endswith('/api/subs') or path.endswith('/api/history'): body=[]\n"
                           "    elif path.endswith('/api/feed'): body={'channels':1,'entries':[]}\n"
                           "    else: body={}\n"
                           "    b=json.dumps(body).encode()\n"
                           "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                           "  def do_GET(s): s.answer()\n"
                           "  def do_POST(s):\n"
                           "    s.rfile.read(int(s.headers.get('Content-Length') or 0))\n"
                           "    " + delay + "\n"
                           "    s.answer()\n"
                           "  def log_message(s,*a): pass\n"
                           "srv=http.server.ThreadingHTTPServer(('127.0.0.1',0),H)\n"
                           "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                           "srv.serve_forever()\n");
    }

    // The environment for a stand-in engine that logs to `log`.
    static QProcessEnvironment logged(const QString &log)
    {
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_LOG", log);
        return environment;
    }

    // The logged requests whose path starts with `path`.
    static QStringList requests(const QString &log, const QString &path)
    {
        QStringList found;
        QFile file(log);
        if (file.open(QIODevice::ReadOnly))
            for (const QString &line : QString::fromUtf8(file.readAll()).split('\n', Qt::SkipEmptyParts))
                if (line.section(' ', 1, 1).startsWith("/t" + path))
                    found << line;
        return found;
    }

    static int lines(const QString &path)
    {
        QFile file(path);
        return file.open(QIODevice::ReadOnly) ? file.readAll().count('\n') : 0;
    }

    static QByteArray read(const QString &path)
    {
        QFile file(path);
        return file.open(QIODevice::ReadOnly) ? file.readAll() : QByteArray();
    }

    static int mode(const QString &path)
    {
        struct stat info {};
        return ::stat(path.toLocal8Bit().constData(), &info) == 0 ? info.st_mode & 0777 : -1;
    }

    // A stand-in qveil-core: a Python script that prints a ready line and then
    // does whatever the test needs.
    static QString engine(const QTemporaryDir &dir, const QByteArray &python)
    {
        const QString path = dir.filePath("qveil-core");
        write(path, "#!/usr/bin/env python3\n" + python);
        QFile::setPermissions(path, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        return path;
    }

    static void write(const QString &path, const QByteArray &contents)
    {
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(contents);
    }
};

QTEST_MAIN(QveilTests)
#include "qveil_tests.moc"
