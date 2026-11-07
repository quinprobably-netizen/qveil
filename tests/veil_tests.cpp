#include <QDir>
#include <QProcessEnvironment>
#include <QFile>
#include <QTemporaryDir>
#include <QtTest>

#include "backend.h"
#include "theme.h"
#include "translator.h"

class VeilTests : public QObject {
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
        const QString fake = dir.filePath("veil-core");
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

    void reportsAMissingEngine()
    {
        Backend backend(QProcessEnvironment::systemEnvironment(), "/nonexistent/veil-core");
        backend.start();
        QTRY_VERIFY(!backend.error().isEmpty());
        QVERIFY(!backend.ready());
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
    static void write(const QString &path, const QByteArray &contents)
    {
        QFile file(path);
        QVERIFY(file.open(QIODevice::WriteOnly));
        file.write(contents);
    }
};

QTEST_MAIN(VeilTests)
#include "veil_tests.moc"
