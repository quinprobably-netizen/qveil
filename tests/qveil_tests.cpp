#include <QDir>
#include <QProcessEnvironment>
#include <QFile>
#include <QTemporaryDir>
#include <QtTest>

#include <clocale>

#include "backend.h"
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
                                         "marker=os.environ['VEIL_TEST_DIR']+'/started'\n"
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
        environment.insert("VEIL_TEST_DIR", dir.path());

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
                                         "open(os.environ['VEIL_TEST_LOG'],'a').write('x\\n')\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:9/t'}),flush=True)\n"
                                         "sys.stderr.write('out of coffee\\n');sys.stderr.flush()\n"
                                         "time.sleep(0.2)\n"
                                         "sys.exit(3)\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("VEIL_TEST_LOG", log);

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
                                         "    open(os.environ['VEIL_TEST_LOG'],'a').write(s.path+'\\n')\n"
                                         "    if 'refresh=1' in s.path: time.sleep(0.5)\n"
                                         "    b=json.dumps({'channels':0,'entries':[]}).encode()\n"
                                         "    s.send_response(200);s.send_header('Content-Length',str(len(b)));s.end_headers();s.wfile.write(b)\n"
                                         "  def log_message(s,*a): pass\n"
                                         "srv=http.server.HTTPServer(('127.0.0.1',0),H)\n"
                                         "print(json.dumps({'base':'http://127.0.0.1:%d/t'%srv.server_port}),flush=True)\n"
                                         "srv.serve_forever()\n");
        QProcessEnvironment environment = QProcessEnvironment::systemEnvironment();
        environment.insert("VEIL_TEST_LOG", log);

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

    void reportsAMissingEngine()
    {
        Backend backend(QProcessEnvironment::systemEnvironment(), "/nonexistent/qveil-core");
        backend.start();
        QTRY_VERIFY(!backend.error().isEmpty());
        QVERIFY(!backend.ready());
    }

    void aPlayerThatWasNeverDrawnShutsDown()
    {
        // Without a window nothing makes a render context, so the player is
        // destroyed with the item. It must not wait for a renderer that
        // doesn't exist (the scene graph can't be had offscreen).
        std::setlocale(LC_NUMERIC, "C");
        {
            MpvItem item;
            item.setVolume(40);
            item.load("/nonexistent/video.mp4", "", 0);
            QTest::qWait(50);
        }
        QVERIFY(true);
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
        QTemporaryDir dir;
        const QString response = dir.filePath("response.json"), command = dir.filePath("hyprctl");
        auto write = [](const QString &path, const QByteArray &data) {
            QFile file(path);
            return file.open(QIODevice::WriteOnly) && file.write(data) == data.size();
        };
        QVERIFY(write(command, "#!/bin/sh\ncat '" + response.toUtf8() + "'\n"));
        QVERIFY(QFile::setPermissions(command, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner));
        QVERIFY(write(response, "{\"int\":0}"));

        Theme theme(dir.path(), nullptr, command);
        QCOMPARE(theme.radius(), 0);
        QVERIFY(write(response, "{\"int\":6}"));
        QTRY_COMPARE(theme.radius(), 6);
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
