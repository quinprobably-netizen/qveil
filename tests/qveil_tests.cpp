#include <QDir>
#include <QLocalServer>
#include <QLocalSocket>
#include <QProcessEnvironment>
#include <QFile>
#include <QPersistentModelIndex>
#include <QQmlApplicationEngine>
#include <QQmlComponent>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQuickWindow>
#include <QSettings>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTemporaryFile>
#include <QtTest>

#include <sys/stat.h>
#include <unistd.h>

#include <vector>

#include "backend.h"
#include "datadirs.h"
#include "launch.h"
#include "mpvitem.h"
#include "suite.h"
#include "theme.h"
#include "translator.h"

class QveilTests : public QObject {
    Q_OBJECT

private slots:
    void initTestCase()
    {
        // The player's QML imports it, as the app registers it.
        qmlRegisterType<MpvItem>("Qveil", 1, 0, "MpvItem");
    }

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
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 1, 5000);
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
        QSignalSpy failed(&backend, &Backend::taskFailed);
        backend.loadSubs();
        QVERIFY(backend.error().contains("stopped again"));
        QCOMPARE(failed.count(), 1);
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

    void searchResultsArriveAPageAtATime()
    {
        QTemporaryDir dir;
        // Two results, a pause, a third; a query of "fail" ends with an error
        // line instead, and "slow" waits a second before its results.
        const QString fake = engine(dir, "import http.server,json,time,urllib.parse\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    q=urllib.parse.parse_qs(urllib.parse.urlparse(s.path).query)\n"
                                         "    if 'stream' not in q:\n"
                                         "      b=b'{}';s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b);return\n"
                                         "    term=q['q'][0]\n"
                                         "    s.send_response(200);s.send_header('Content-Type','application/x-ndjson');s.end_headers()\n"
                                         "    if term=='slow': time.sleep(1)\n"
                                         "    line=lambda i: s.wfile.write((json.dumps({'id':term[:4]+'%07d'%i,'title':term})+'\\n').encode()) or s.wfile.flush()\n"
                                         "    line(1);line(2)\n"
                                         "    time.sleep(0.8)\n"
                                         "    if term=='fail': s.wfile.write(b'{\"error\":\"the route went down\"}\\n')\n"
                                         "    else: line(3)\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.ThreadingHTTPServer(('127.0.0.1',0),H)\n"
                                         "srv.protocol_version='HTTP/1.0'\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        Backend backend(QProcessEnvironment::systemEnvironment(), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        // The first page shows before the second has come.
        QElapsedTimer clock;
        clock.start();
        backend.search("rust");
        QTRY_COMPARE_WITH_TIMEOUT(backend.results()->count(), 2, 5000);
        QVERIFY2(clock.elapsed() < 700, qPrintable(QString::number(clock.elapsed())));
        QVERIFY(backend.busy().contains("search"));
        QTRY_COMPARE_WITH_TIMEOUT(backend.results()->count(), 3, 5000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 5000);
        QCOMPARE(backend.results()->get(2).value("id").toString(), QString("rust0000003"));
        QVERIFY(backend.error().isEmpty());

        // A search that is replaced adds nothing to its successor's results.
        backend.search("slow");
        QTest::qWait(100);
        backend.search("next");
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 5000);
        QCOMPARE(backend.results()->count(), 3);
        for (int row = 0; row < backend.results()->count(); ++row)
            QCOMPARE(backend.results()->get(row).value("title").toString(), QString("next"));

        // One that fails part-way keeps what it found and says what happened.
        QSignalSpy failed(&backend, &Backend::taskFailed);
        backend.search("fail");
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 5000);
        QCOMPARE(backend.results()->count(), 2);
        QCOMPARE(backend.error(), QString("the route went down"));
        QCOMPARE(failed.count(), 1);
    }

    void aRequestWithoutAnEngineFailsAtOnce()
    {
        // Nothing started the engine, so nothing can answer. Whoever waits
        // for an answer must hear that now: the player's "Opening the
        // video…" would spin for ever otherwise.
        Backend backend(QProcessEnvironment::systemEnvironment(), "/nonexistent/qveil-core");
        QSignalSpy failed(&backend, &Backend::taskFailed);
        backend.play("dQw4w9WgXcQ");
        backend.search("rust");
        backend.openChannel("UC0123456789abcdefghij");
        backend.refreshFeed(true, false);
        backend.loadSubs();
        QStringList tasks;
        for (const QList<QVariant> &signal : failed)
            tasks << signal.at(0).toString();
        QCOMPARE(tasks, QStringList({"play", "search", "channel", "feed", "subs"}));
    }

    void aReplacedRequestIsHungUpOn()
    {
        // A request whose path says "slow" is held open until the client
        // hangs up, and the engine writes down whether that happened. A
        // newer request of the same task, or a closed player, must hang up
        // on it, so the engine can stop working for an answer nobody wants.
        QTemporaryDir dir;
        const QString log = dir.filePath("requests");
        const QString fake = engine(dir, "import http.server,json,os\n"
                                         "def note(text): open(os.environ['QVEIL_TEST_LOG'],'a').write(text+'\\n')\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def answer(s):\n"
                                         "    if 'slow' in s.path:\n"
                                         "      note('held '+s.path)\n"
                                         "      s.connection.settimeout(8)\n"
                                         "      try: gone=s.connection.recv(1)==b''\n"
                                         "      except Exception: gone=False\n"
                                         "      note(('hung up ' if gone else 'kept waiting ')+s.path)\n"
                                         "      return\n"
                                         "    body={'id':'UCfast','name':'Fast','videos':[]} if 'channel' in s.path else {}\n"
                                         "    b=json.dumps(body).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def do_GET(s): s.answer()\n"
                                         "  def do_POST(s):\n"
                                         "    s.rfile.read(int(s.headers.get('Content-Length') or 0))\n"
                                         "    s.answer()\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.ThreadingHTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        Backend backend(logged(log), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        QSignalSpy failed(&backend, &Backend::taskFailed);
        auto logged = [&](const QString &text) { return QString::fromUtf8(read(log)).contains(text); };

        backend.search("slow");
        QTRY_VERIFY_WITH_TIMEOUT(logged("held /t/api/search?q=slow"), 5000);
        backend.search("next");
        QTRY_VERIFY_WITH_TIMEOUT(logged("hung up /t/api/search?q=slow"), 5000);

        backend.openChannel("UCslow");
        QTRY_VERIFY_WITH_TIMEOUT(logged("held /t/api/channel?id=UCslow"), 5000);
        backend.openChannel("UCfast");
        QTRY_VERIFY_WITH_TIMEOUT(logged("hung up /t/api/channel?id=UCslow"), 5000);
        QTRY_COMPARE_WITH_TIMEOUT(backend.channel().value("name").toString(), QString("Fast"), 5000);

        backend.play("slow");
        QTRY_VERIFY_WITH_TIMEOUT(logged("held /t/api/play?v=slow"), 5000);
        backend.cancelPlay();
        QTRY_VERIFY_WITH_TIMEOUT(logged("hung up /t/api/play?v=slow"), 5000);

        backend.refreshFeed(false, false);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed-cache"), 5000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search") && !backend.busy().contains("channel")
                                     && !backend.busy().contains("play"),
                                 5000);
        // Hanging up is no failure: nobody is told, and nothing is shown.
        QCOMPARE(failed.count(), 0);
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
    }

    void aRefreshLandingUpdatesTheFeedInPlace()
    {
        // The second answer has one video more, at the front. The cards on
        // screen must stay, so their rows are inserted into, not replaced.
        QTemporaryDir dir;
        const QString fake = engine(dir, "import http.server,json\n"
                                         "n=[0]\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    body={}\n"
                                         "    if '/api/feed' in s.path:\n"
                                         "      n[0]+=1\n"
                                         "      ids=['aaaaaaaaaaa','bbbbbbbbbbb','ccccccccccc']\n"
                                         "      if n[0]>1: ids=['xxxxxxxxxxx']+ids\n"
                                         "      body={'channels':1,'entries':[{'id':i,'title':i} for i in ids]}\n"
                                         "    b=json.dumps(body).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        Backend backend(QProcessEnvironment::systemEnvironment(), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        backend.refreshFeed(false, false);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 3, 5000);
        QPersistentModelIndex first = backend.feed()->index(0);

        QSignalSpy inserted(backend.feed(), &QAbstractItemModel::rowsInserted);
        QSignalSpy removed(backend.feed(), &QAbstractItemModel::rowsRemoved);
        QSignalSpy reset(backend.feed(), &QAbstractItemModel::modelReset);
        backend.refreshFeed(true, false);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 4, 5000);
        QCOMPARE(inserted.count(), 1);
        QCOMPARE(inserted.at(0).at(1).toInt(), 0);
        QCOMPARE(removed.count() + reset.count(), 0);
        QCOMPARE(first.row(), 1);
        QCOMPARE(backend.feed()->get(first.row()).value("id").toString(), QString("aaaaaaaaaaa"));
    }

    void qveilOpensTheWindowOnlyAlone()
    {
        // `qveil` and `qveil open` open it; everything else, `open` with
        // arguments included, is a command for the engine.
        auto opens = [](std::initializer_list<const char *> arguments) {
            std::vector<const char *> argv(arguments);
            return opensWindow(int(argv.size()), argv.data());
        };
        QVERIFY(opens({"qveil"}));
        QVERIFY(opens({"qveil", "open"}));
        QVERIFY(!opens({"qveil", "open", "--help"}));
        QVERIFY(!opens({"qveil", "open", "extra"}));
        QVERIFY(!opens({"qveil", "--help"}));
        QVERIFY(!opens({"qveil", "feed", "--json"}));
        QVERIFY(!opens({"qveil", "opened"}));
        QVERIFY(!opens({"qveil", "Open"}));
        // Something an unusual launcher might do.
        QVERIFY(opens({}));
    }

    void theEnginesPrivacyTextIsShownAsItIsOrNotAtAll()
    {
        const QVariantMap sent{
            {"privacy",
             QVariantMap{{"summary", "One sentence."},
                         {"points", QStringList{"First.", "Second."}},
                         {"routes", QVariantMap{{"direct", "D."}, {"tor", "T."}, {"proxy", "P."}}},
                         {"pages", QVariantMap{{"search", "S."}, {"subscriptions", "U."}, {"history_off", "Off."}, {"history_on", "On."}}}}}};
        const QVariantMap shown = Backend::privacyOf(sent);
        QCOMPARE(shown.value("summary").toString(), QString("One sentence."));
        // In the engine's order.
        QCOMPARE(shown.value("points").toStringList(), QStringList({"First.", "Second."}));
        QCOMPARE(shown.value("routes").toMap().value("tor").toString(), QString("T."));
        QCOMPARE(shown.value("pages").toMap().value("history_off").toString(), QString("Off."));

        // An engine that sends none of it: every field is there and empty, so
        // the window shows nothing, and QML needs no checks.
        for (const QVariantMap &nothing : {QVariantMap(), QVariantMap{{"privacy", QVariantMap()}},
                                           QVariantMap{{"privacy", "not a map"}}}) {
            const QVariantMap empty = Backend::privacyOf(nothing);
            QCOMPARE(empty.value("summary").toString(), QString());
            QVERIFY(empty.value("points").toStringList().isEmpty());
            for (const char *key : {"direct", "tor", "proxy"})
                QVERIFY2(empty.value("routes").toMap().contains(key) && empty.value("routes").toMap().value(key).toString().isEmpty(), key);
            for (const char *key : {"search", "subscriptions", "history_off", "history_on"})
                QVERIFY2(empty.value("pages").toMap().contains(key) && empty.value("pages").toMap().value(key).toString().isEmpty(), key);
        }
    }

    void theWindowHasNoPrivacyClaimsOfItsOwn()
    {
        // Every word the window says about privacy is the engine's: a copy
        // written here would go on saying what the engine had stopped doing.
        // So the claims it used to make must not come back.
        const QStringList claims{"Never sent",       "can't link",   "hidden from YouTube", "own circuit",     "never sees",
                                 "YouTube sees",     "Kept only",    "Stored only",         "without an account", "doesn't remember",
                                 "read offline",     "no cookies",   "Your IP is"};
        const QDir source(SRCDIR);
        const QStringList files = source.entryList({"*.qml", "*.js"}, QDir::Files);
        QVERIFY2(files.size() > 15, "the window's QML wasn't found");
        bool sawText = false;
        for (const QString &name : files) {
            QFile file(source.filePath(name));
            QVERIFY(file.open(QIODevice::ReadOnly));
            // Comments may say what is not to be said.
            QString text = QString::fromUtf8(file.readAll());
            text.remove(QRegularExpression("/\\*.*?\\*/", QRegularExpression::DotMatchesEverythingOption));
            text.remove(QRegularExpression("(^|\\s)//[^\\n]*"));
            sawText = sawText || text.contains("qsTr(");
            for (const QString &claim : claims)
                QVERIFY2(!text.contains(claim, Qt::CaseInsensitive),
                         qPrintable(name + " says \"" + claim + "\", which only the engine may"));
        }
        QVERIFY2(sawText, "the check read no strings");
    }

    void errorsOfferARouteOnlyWhereAnotherRouteCanHelp()
    {
        for (const char *code : {"bot_check", "tor_down", "proxy_refused", "rate_limited"})
            QVERIFY2(Backend::offersRoute(code), code);
        for (const char *code : {"", "age_restricted", "route_changed", "live", "private", "ytdlp_outdated", "deno_missing", "other", "unknown"})
            QVERIFY2(!Backend::offersRoute(code), code);
    }

    void theBannerFollowsTheCodeOfTheEnginesError()
    {
        // The engine's errors say what they are, in a plain reply and in a
        // streamed one; the window decides from that what to offer.
        QTemporaryDir dir;
        Backend backend(QProcessEnvironment::systemEnvironment(), contractEngine(dir));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        QSignalSpy failed(&backend, &Backend::taskFailed);

        for (const char *code : {"bot_check", "tor_down", "proxy_refused", "rate_limited"}) {
            backend.clearError();
            backend.play(code);
            QTRY_COMPARE_WITH_TIMEOUT(backend.errorCode(), QString(code), 5000);
            QVERIFY(backend.errorOffersRoute());
            QCOMPARE(backend.error(), QString("The engine's words for ") + code);
        }
        for (const char *code : {"age_restricted", "private", "other"}) {
            backend.clearError();
            backend.play(code);
            QTRY_COMPARE_WITH_TIMEOUT(backend.errorCode(), QString(code), 5000);
            QVERIFY(!backend.errorOffersRoute());
        }
        backend.clearError();
        QCOMPARE(backend.errorCode(), QString());
        QVERIFY(!backend.errorOffersRoute());

        // A search that fails part-way says so in a line of its own, with the same code.
        backend.search("code:tor_down");
        QTRY_COMPARE_WITH_TIMEOUT(backend.errorCode(), QString("tor_down"), 5000);
        QVERIFY(backend.errorOffersRoute());
        QCOMPARE(backend.error(), QString("The engine's words for tor_down"));

        // The user caused a changed route, so it is not shown. The request
        // still failed, which whoever waits for it needs to hear.
        backend.clearError();
        failed.clear();
        backend.play("route_changed");
        QTRY_COMPARE_WITH_TIMEOUT(failed.count(), 1, 5000);
        backend.search("code:route_changed");
        QTRY_COMPARE_WITH_TIMEOUT(failed.count(), 2, 5000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 5000);
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));
        QCOMPARE(backend.errorCode(), QString());
    }

    void aStreamedRefreshShowsHowFarItIsAndThenTheFeed()
    {
        QTemporaryDir dir;
        Backend backend(QProcessEnvironment::systemEnvironment(), contractEngine(dir));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        QList<QPair<int, int>> seen;
        connect(&backend, &Backend::refreshProgressChanged, this, [&] { seen.append({backend.refreshDone(), backend.refreshTotal()}); });
        QCOMPARE(backend.refreshTotal(), 0);
        backend.refreshFeed(true, false);
        QTRY_VERIFY_WITH_TIMEOUT(backend.refreshTotal() == 4, 5000);
        QVERIFY(backend.busy().contains("feed"));
        // The feed lands with the last line, and the progress is over with the stream.
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 2, 5000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        QCOMPARE(backend.refreshTotal(), 0);
        QCOMPARE(backend.refreshDone(), 0);
        QVERIFY(backend.feedInfo().value("refreshedAt").toLongLong() > 0);
        QCOMPARE(backend.feedInfo().value("channels").toInt(), 4);
        QVERIFY(!backend.feedInfo().contains("entries"));
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));

        int done = -1;
        for (const auto &[at, of] : std::as_const(seen)) {
            if (of == 0)
                continue;
            QCOMPARE(of, 4);
            QVERIFY2(at >= done, "progress went backwards");
            done = at;
        }
        QCOMPARE(done, 4);
    }

    void aStreamThatKeepsSendingOutlivesTheIdleTimeoutAndOneThatStopsDoesNot()
    {
        // The stand-in sends a line every 0.4 s for 2 s. The window gives up
        // on 1 s of silence, but not on 2 s of lines.
        QTemporaryDir dir;
        Backend backend(QProcessEnvironment::systemEnvironment(), contractEngine(dir));
        backend.setIdleTimeout(1000);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);

        backend.refreshFeed(true, false);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 2, 8000);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        QVERIFY2(backend.error().isEmpty(), qPrintable(backend.error()));

        // "quiet" says its first line and then nothing for 3 s.
        QSignalSpy failed(&backend, &Backend::taskFailed);
        backend.search("quiet");
        QTRY_COMPARE_WITH_TIMEOUT(failed.count(), 1, 8000);
        QCOMPARE(failed.at(0).at(0).toString(), QString("search"));
        QVERIFY(!backend.error().isEmpty());
        QCOMPARE(backend.results()->count(), 1);
    }

    void theMainWindowLoadsCleanAndSpeaksOnlyForItself()
    {
        // The whole window, as the app loads it (on the offscreen platform,
        // with a stand-in engine): no QML warning, the engine's words where
        // the window used to have its own, a refresh that fills the progress
        // bar, and an error that offers to change the route only when that
        // can help.
        QTest::failOnWarning(QRegularExpression("qrc:/|\\.qml|QML|TypeError|ReferenceError"));
        // The window's settings (Shorts, volume) are not the user's.
        QTemporaryDir settings;
        QSettings::setPath(QSettings::NativeFormat, QSettings::UserScope, settings.path());
        QCoreApplication::setOrganizationName("qveil-tests");

        QTemporaryDir engineDir, themeDir;
        Theme theme(themeDir.path());
        Translator translator;
        Backend backend(QProcessEnvironment::systemEnvironment(), contractEngine(engineDir));
        QQmlApplicationEngine qml;
        qml.rootContext()->setContextProperty("translator", &translator);
        qml.rootContext()->setContextProperty("theme", &theme);
        qml.rootContext()->setContextProperty("backend", &backend);
        qml.load(QUrl("qrc:/Main.qml"));
        QVERIFY(!qml.rootObjects().isEmpty());
        auto *window = qobject_cast<QQuickWindow *>(qml.rootObjects().first());
        QVERIFY(window);
        window->requestActivate();
        auto find = [&](const char *name) {
            QObject *found = window->findChild<QObject *>(name);
            [&] { QVERIFY2(found, name); }();
            return found;
        };

        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        QObject *errorBar = find("errorBar"), *feedPage = find("feedPage"), *progress = find("progress");
        QVERIFY(errorBar && feedPage && progress);
        QVERIFY(!errorBar->property("visible").toBool());

        // Startup refreshes the feed, which the engine streams: how far it
        // is shows in the subheading and in the bar, which then fills.
        QTRY_VERIFY_WITH_TIMEOUT(feedPage->property("subheading").toString().startsWith("Refreshing "), 5000);
        QVERIFY(progress->property("visible").toBool());
        QVERIFY(!progress->property("indeterminate").toBool());
        QTRY_VERIFY_WITH_TIMEOUT(progress->property("value").toDouble() > 0.4, 5000);
        QTRY_COMPARE_WITH_TIMEOUT(feedPage->property("subheading").toString(), QString("Latest from 4 channels"), 8000);
        QTRY_VERIFY_WITH_TIMEOUT(!progress->property("visible").toBool(), 5000);

        // The search page's line is the engine's, as sent.
        QCOMPARE(find("searchPage")->property("subheading").toString(), QString("a"));

        // An error another route may get past offers to change it, and the
        // offer opens the route popup and takes the error away.
        QObject *route = find("routePopup");
        backend.play("bot_check");
        QTRY_VERIFY_WITH_TIMEOUT(errorBar->property("visible").toBool(), 5000);
        QObject *change = find("changeRoute");
        QVERIFY(change->property("visible").toBool());
        QVERIFY(!route->property("opened").toBool());
        QMetaObject::invokeMethod(change, "clicked");
        QTRY_VERIFY_WITH_TIMEOUT(route->property("opened").toBool(), 5000);
        QVERIFY(!errorBar->property("visible").toBool());
        QMetaObject::invokeMethod(route, "close");
        QTRY_VERIFY_WITH_TIMEOUT(!route->property("opened").toBool(), 5000);

        // One no other route can help with offers nothing, and one the user
        // caused isn't shown at all.
        backend.play("age_restricted");
        QTRY_VERIFY_WITH_TIMEOUT(errorBar->property("visible").toBool(), 5000);
        QVERIFY(!change->property("visible").toBool());
        backend.clearError();
        backend.play("route_changed");
        QTest::qWait(300);
        QVERIFY(!errorBar->property("visible").toBool());

        // P opens the route popup, and closes it.
        QTest::keyClick(window, Qt::Key_P);
        QTRY_VERIFY_WITH_TIMEOUT(route->property("opened").toBool(), 5000);
        QTest::keyClick(window, Qt::Key_P);
        QTRY_VERIFY_WITH_TIMEOUT(!route->property("opened").toBool(), 5000);
    }

    void thumbnailsAreNumberedByThePageTheyCameWith()
    {
        // The engine puts the thumbnails of one batch on one Tor circuit, as
        // a page load would, so a new page needs a new number, and cards of a
        // page that is updated in place keep theirs (their pictures aren't
        // fetched again). No two grids share a number. Live streams get no card.
        QTemporaryDir dir;
        Backend backend(QProcessEnvironment::systemEnvironment(), contractEngine(dir));
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        auto batchOf = [](VideoModel *model, int row) { return model->get(row).value("batch").toInt(); };

        // The cache's answer, then a refresh that has one more video.
        backend.refreshFeed(false, false);
        QTRY_COMPARE_WITH_TIMEOUT(backend.feed()->count(), 2, 5000);
        const int cached = batchOf(backend.feed(), 0);
        QVERIFY(cached > 0);
        QCOMPARE(batchOf(backend.feed(), 1), cached);
        backend.refreshFeed(true, false);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("feed"), 5000);
        QCOMPARE(backend.feed()->count(), 2);
        QCOMPARE(batchOf(backend.feed(), 0), cached);

        backend.search("rust");
        QTRY_COMPARE_WITH_TIMEOUT(backend.results()->count(), 2, 5000);
        const int first = batchOf(backend.results(), 0);
        QVERIFY(first > cached);
        QCOMPARE(batchOf(backend.results(), 1), first);
        QTRY_VERIFY_WITH_TIMEOUT(!backend.busy().contains("search"), 5000);

        backend.search("again");
        QTRY_COMPARE_WITH_TIMEOUT(backend.results()->count(), 2, 5000);
        QVERIFY(batchOf(backend.results(), 0) > first);

        backend.openChannel("UC0123456789abcdefghij");
        QTRY_COMPARE_WITH_TIMEOUT(backend.channelVideos()->count(), 1, 5000);
        QVERIFY(batchOf(backend.channelVideos(), 0) > batchOf(backend.results(), 0));

        // The live stream in each answer is not shown.
        for (VideoModel *model : {backend.feed(), backend.results(), backend.channelVideos()})
            QVERIFY(!model->ids().contains("liveliveli1"));
    }

    void theTokenNeverReachesTheWindowOrItsLogs()
    {
        // The engine's token is in every URL the window uses, and it is all
        // that keeps other programs off the engine. Qt quotes the URL in the
        // error text of a failed request and in its warnings (a thumbnail
        // that doesn't load, say), and neither belongs in the error banner,
        // a terminal or the journal.
        QTemporaryDir dir;
        const QString fake = engine(dir, "import http.server,json\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    if s.path.endswith('/api/config'):\n"
                                         "      b=json.dumps({'thumbnails':True}).encode()\n"
                                         "      s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "    else:\n"
                                         "      s.send_response(404);s.send_header('Content-Length','0');s.end_headers()\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/0123456789abcdef'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        Backend backend(QProcessEnvironment::systemEnvironment(), fake);
        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(!backend.thumbBase().isEmpty(), 5000);
        const QString base = backend.thumbBase().chopped(QString("/thumb/").size());
        QVERIFY(base.contains("0123456789abcdef"));

        // Qt's words for a 404 are "Error transferring <URL> - server replied: Not Found".
        backend.loadSubs();
        QTRY_VERIFY_WITH_TIMEOUT(!backend.error().isEmpty(), 5000);
        QVERIFY2(!backend.error().contains("0123456789abcdef") && !backend.error().contains("127.0.0.1"),
                 qPrintable(backend.error()));

        // What Qt logs goes through the same scrubbing before it is printed.
        Backend::scrubLogs();
        QTest::ignoreMessage(QtWarningMsg, "QML Image: Error transferring qveil-core/thumb/<id> - server replied: Not Found");
        qWarning("QML Image: Error transferring %s/thumb/dQw4w9WgXcQ?batch=12 - server replied: Not Found", qPrintable(base));
        QTest::ignoreMessage(QtWarningMsg, "the token is <token>");
        qWarning("the token is %s", "0123456789abcdef");
    }

    void aThumbnailsVideoDoesNotReachTheLogsEither()
    {
        // A thumbnail's address says which video is on screen, and Qt quotes
        // the address of one that fails to load. That is nobody's business
        // in the journal. (With or without the engine's base in front, and
        // with or without a batch.)
        QCOMPARE(Backend::scrub("Error transferring http://127.0.0.1:1/aabbccddeeff0011/thumb/dQw4w9WgXcQ?batch=7 - x"),
                 QString("Error transferring http://127.0.0.1:1/aabbccddeeff0011/thumb/<id> - x"));
        QCOMPARE(Backend::scrub("could not open \"qveil-core/thumb/-_aZ09bcdEF\""), QString("could not open \"qveil-core/thumb/<id>\""));
        // Scrubbing what is scrubbed leaves it alone, and so does text with no thumbnail in it.
        const QString clean = "Error transferring qveil-core/thumb/<id> - x";
        QCOMPARE(Backend::scrub(clean), clean);
        QCOMPARE(Backend::scrub("The thumbnails are off."), QString("The thumbnails are off."));
    }

    void mpvDoesNotLogTheUrlItFailsToOpen()
    {
        // mpv names what it can't open in its log, and everything it is given
        // here is a loopback URL with the engine's token in it. Stdout and
        // stderr both end up in the journal, so both are pointed at a file
        // while it fails to open one.
        MpvItem item;
        QSignalSpy failed(&item, &MpvItem::failed);
        QTemporaryFile capture;
        QVERIFY(capture.open());
        std::fflush(stdout);
        std::fflush(stderr);
        const int savedOut = ::dup(1), savedErr = ::dup(2);
        ::dup2(capture.handle(), 1);
        ::dup2(capture.handle(), 2);
        item.load("http://127.0.0.1:1/0123456789abcdef/s/x/v", "http://127.0.0.1:1/0123456789abcdef/s/x/a", 0);
        QTRY_VERIFY_WITH_TIMEOUT(failed.count() > 0, 10000);
        QTest::qWait(300);
        std::fflush(stdout);
        std::fflush(stderr);
        ::dup2(savedOut, 1);
        ::dup2(savedErr, 2);
        ::close(savedOut);
        ::close(savedErr);

        capture.seek(0);
        const QByteArray said = capture.readAll();
        QVERIFY2(!said.contains("0123456789abcdef"), said.constData());
        QVERIFY2(!said.contains("127.0.0.1"), said.constData());
        QVERIFY(!failed.at(0).at(0).toString().contains("0123456789abcdef"));
    }

    void thePlayerClosesWhenTheEngineIsGone()
    {
        // An engine that goes away when a file called `stop` is written after
        // it started. mpv holds the address of an engine that is gone, and a
        // restarted one has another, so the player must not stay up: not
        // while it looks a video up, and not while a video plays.
        QTemporaryDir dir;
        const QString fake = engine(dir, "import http.server,json,os,threading,time\n"
                                         "stop=os.environ['QVEIL_TEST_DIR']+'/stop'\n"
                                         "started=time.time()\n"
                                         "def watch():\n"
                                         "  while not (os.path.exists(stop) and os.path.getmtime(stop)>started): time.sleep(0.05)\n"
                                         "  os._exit(3)\n"
                                         "threading.Thread(target=watch,daemon=True).start()\n"
                                         "class H(http.server.BaseHTTPRequestHandler):\n"
                                         "  def do_GET(s):\n"
                                         "    b=json.dumps({'thumbnails':False}).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("QVEIL_TEST_DIR", dir.path());

        QTemporaryDir themeDir;
        Theme theme(themeDir.path());
        Backend backend(environment, fake);
        QQmlEngine qml;
        qml.rootContext()->setContextProperty("backend", &backend);
        qml.rootContext()->setContextProperty("theme", &theme);
        QQmlComponent component(&qml, QUrl::fromLocalFile(QString(SRCDIR) + "/PlayerView.qml"));
        QScopedPointer<QObject> player(component.create());
        QVERIFY2(player, qPrintable(component.errorString()));
        QSignalSpy closed(player.data(), SIGNAL(closed()));
        QSignalSpy readiness(&backend, &Backend::readyChanged);

        backend.start();
        QTRY_VERIFY_WITH_TIMEOUT(backend.ready(), 5000);
        const QVariantMap video{{"id", "dQw4w9WgXcQ"}, {"title", "A video"}};

        // While the engine looks the video up.
        QMetaObject::invokeMethod(player.data(), "prepare", Q_ARG(QVariant, video));
        QVERIFY(player->property("active").toBool());
        write(dir.filePath("stop"), "");
        // At least: the restart can be ready again before a poll sees it lost.
        QTRY_VERIFY_WITH_TIMEOUT(readiness.count() >= 2, 5000);
        QVERIFY(!player->property("active").toBool());
        QCOMPARE(closed.count(), 1);

        // While it plays. The engine has been restarted by then, and the
        // restart doesn't bring the video back.
        QTRY_COMPARE_WITH_TIMEOUT(readiness.count(), 3, 10000);
        QVERIFY(backend.ready());
        QVariantMap playing = video;
        playing.insert("video", "http://127.0.0.1:1/t/s/x/v");
        QMetaObject::invokeMethod(player.data(), "open", Q_ARG(QVariant, playing));
        QVERIFY(player->property("active").toBool());
        QVERIFY(!player->property("pending").toBool());
        write(dir.filePath("stop"), "again");
        QTRY_COMPARE_WITH_TIMEOUT(readiness.count(), 4, 5000);
        QVERIFY(!player->property("active").toBool());
        QCOMPARE(closed.count(), 2);
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

    void themeReadsTheRoomAndTheFont()
    {
        QTemporaryDir dir, tools;
        QVERIFY(QDir().mkpath(dir.filePath("theme")));
        write(dir.filePath("theme/colors.toml"),
              "accent = \"#a08dff\"\nbackground = \"#151326\"\ndark_background = \"#100e1d\"\nforeground = \"#dad6ec\"\n");
        const QString fc = tools.filePath("fc-match");
        write(fc, "#!/bin/sh\nprintf 'Test Mono'\n");
        QVERIFY(QFile::setPermissions(fc, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        Theme theme(dir.path(), nullptr, {}, {}, fc);
        QTRY_COMPARE(theme.fontFamily(), QString("Test Mono"));
        QCOMPARE(theme.background(), QString("#151326"));
        QCOMPARE(theme.foreground(), QString("#dad6ec"));
        QCOMPARE(theme.accent(), QString("#a08dff"));

        // Without a theme, or without fontconfig, there is still a room and a typeface.
        QTemporaryDir empty;
        Theme bare(empty.path(), nullptr, {}, {}, "/nonexistent/fc-match");
        QCOMPARE(bare.background(), QString("#111115"));
        QCOMPARE(bare.foreground(), QString("#e9e4f0"));
        QCOMPARE(bare.fontFamily(), QString("monospace"));
    }

    void themeFollowsAChangeOfColours()
    {
        QTemporaryDir dir;
        QVERIFY(QDir().mkpath(dir.filePath("theme")));
        write(dir.filePath("theme/colors.toml"), "background = \"#101010\"\n");
        Theme theme(dir.path());
        QCOMPARE(theme.background(), QString("#101010"));
        QSignalSpy changed(&theme, &Theme::changed);
        write(dir.filePath("theme/colors.toml"), "background = \"#202020\"\nforeground = \"#fafafa\"\n");
        QTRY_COMPARE(theme.background(), QString("#202020"));
        QCOMPARE(theme.foreground(), QString("#fafafa"));
        QVERIFY(changed.count() >= 1);
    }

    void themeFollowsHyprlandRounding()
    {
        // The fake hyprctl lives apart from the theme directory: the theme
        // watches its directory and re-reads rounding on any change there,
        // which would make rewriting the response look like a reload. It
        // watches the directory's parent too, so that is a private one, not
        // /tmp, which other programs change at any moment.
        QTemporaryDir root, tools;
        QVERIFY(QDir(root.path()).mkdir("theme"));
        const QString themeDir = root.filePath("theme");
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

        Theme theme(themeDir, nullptr, command, sockets.filePath("events.sock"));
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

    // A stand-in for an engine that speaks the newest contract: its privacy
    // text, codes with its errors, a refresh that streams its progress, and
    // a live stream in every answer. The real engine's tests of the same
    // things (engine_tests.cpp) wait for that engine; this one lets the
    // window's side be tested with any. A `code:<code>` search or a play of
    // `<code>` fails with that code; a search for "quiet" goes silent after its
    // first line.
    static QString contractEngine(const QTemporaryDir &dir)
    {
        return engine(dir, R"PY(import http.server,json,time,urllib.parse
def entries(n): return [{'id':'feed%07d'%i,'title':'Video %d'%i} for i in range(n)] + [{'id':'liveliveli1','title':'Live','live':True}]
class H(http.server.BaseHTTPRequestHandler):
  protocol_version='HTTP/1.0'
  def send(s,code,body):
    b=json.dumps(body).encode()
    s.send_response(code);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)
  def line(s,o): s.wfile.write((json.dumps(o)+'\n').encode());s.wfile.flush()
  def ndjson(s):
    s.send_response(200);s.send_header('Content-Type','application/x-ndjson');s.end_headers()
  def do_GET(s):
    u=urllib.parse.urlparse(s.path);q=urllib.parse.parse_qs(u.query);path=u.path.split('/t/',1)[-1]
    if path=='api/config':
      s.send(200,{'route':'direct','thumbnails':False,'privacy':{'summary':'S','points':['P1','P2'],'routes':{'direct':'D','tor':'T','proxy':'X'},'pages':{'search':'a','subscriptions':'b','history_off':'c','history_on':'d'}}})
    elif path=='api/feed' and 'stream' in q:
      s.ndjson()
      for i in range(5):
        s.line({'progress':{'done':i,'total':4}})
        time.sleep(0.4 if i<4 else 0)
      s.line({'result':{'channels':4,'entries':entries(2)}})
    elif path=='api/feed': s.send(200,{'channels':4,'entries':entries(2)})
    elif path=='api/channel': s.send(200,{'id':'UC0123456789abcdefghij','name':'Chan','videos':entries(1)})
    elif path=='api/search':
      term=q['q'][0];s.ndjson()
      if term.startswith('code:'): s.line({'error':"The engine's words for "+term[5:],'code':term[5:]});return
      s.line({'id':'res0000001','title':term})
      if term=='quiet': time.sleep(3);return
      s.line({'id':'res0000002','title':term});s.line({'id':'liveliveli1','title':'Live','live':True})
    else: s.send(200,{})
  def do_POST(s):
    u=urllib.parse.urlparse(s.path);q=urllib.parse.parse_qs(u.query)
    s.rfile.read(int(s.headers.get('Content-Length') or 0))
    if u.path.endswith('/api/play'):
      code=q['v'][0];s.send(500,{'error':"The engine's words for "+code,'code':code})
    else: s.send(200,{})
  def log_message(s,*a): pass
srv=http.server.ThreadingHTTPServer(('127.0.0.1',0),H)
print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)
srv.serve_forever()
)PY");
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

QVEIL_SUITE(QveilTests)
#include "qveil_tests.moc"
