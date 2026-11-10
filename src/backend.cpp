#include "backend.h"

#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMutex>
#include <QNetworkProxy>
#include <QNetworkReply>
#include <QNetworkRequest>
#include <QRandomGenerator>
#include <QRegularExpression>
#include <QSet>
#include <QTimer>

#include <algorithm>
#include <memory>
#include <utility>

namespace {

// What scrub() takes out: the base URL of every engine this process has run
// (its token is in it) and the token alone. Kept for the life of the process,
// because a thumbnail that was loading when an engine died fails afterwards.
// Longest first, so a short one can't break up a longer one before it is
// matched. Qt logs from its worker threads too, hence the lock.
QMutex secretsLock;
QStringList secrets;
QtMessageHandler previousHandler = nullptr;

void remember(const QString &base) {
    const QString token = base.section('/', 3, 3);
    QMutexLocker lock(&secretsLock);
    // The engine's token is 32 hex digits; anything much shorter would be a
    // word that turns up in ordinary text.
    for (const QString &secret : {base, token.size() >= 8 ? token : QString()})
        if (!secret.isEmpty() && !secrets.contains(secret))
            secrets.append(secret);
    std::sort(secrets.begin(), secrets.end(), [](const QString &a, const QString &b) { return a.size() > b.size(); });
}

} // namespace

Backend::Backend(const QProcessEnvironment &environment, const QString &core, QObject *parent)
    : QObject(parent), m_environment(environment), m_corePath(core.isEmpty() ? corePath() : core),
      // Pages count up from a random start, so that a window started later
      // doesn't name its first page like this one did: the engine puts the
      // thumbnails of one name on one circuit, and two runs would share it.
      m_batch(int(QRandomGenerator::global()->bounded(1, 1 << 30))) {
    // Only loopback traffic leaves this process; see main.cpp.
    m_net.setProxy(QNetworkProxy::NoProxy);

    m_stable.setSingleShot(true);
    m_stable.setInterval(StableAfterMs);
    connect(&m_stable, &QTimer::timeout, this, [this] { m_restarts = 0; });

    connect(&m_core, &QProcess::readyReadStandardOutput, this, [this] {
        if (ready())
            return;
        m_coreStdout += m_core.readAllStandardOutput();
        const int newline = m_coreStdout.indexOf('\n');
        if (newline < 0)
            return;
        m_base = parseReady(m_coreStdout.left(newline));
        if (m_base.isEmpty()) {
            setError(tr("The qveil engine sent an unexpected reply."));
            return;
        }
        remember(m_base);
        onEngineReady();
    });
    connect(&m_core, &QProcess::readyReadStandardError, this, [this] {
        m_coreStderr = (m_coreStderr + m_core.readAllStandardError()).right(2000);
    });
    connect(&m_core, &QProcess::errorOccurred, this, [this](QProcess::ProcessError e) {
        if (e == QProcess::FailedToStart)
            setError(tr("Couldn't start the qveil engine (%1).").arg(m_corePath));
    });
    connect(&m_core, &QProcess::finished, this, [this] { onEngineStopped(); });
}

Backend::~Backend() {
    m_quitting = true;
    if (m_core.state() != QProcess::NotRunning) {
        // Closing its stdin is the engine's signal to exit.
        m_core.closeWriteChannel();
        if (!m_core.waitForFinished(1500))
            m_core.kill();
        m_core.waitForFinished(500);
    }
}

QString Backend::corePath() {
    const QString self = QFileInfo("/proc/self/exe").canonicalFilePath();
    const QString beside = QFileInfo(self).absolutePath() + "/qveil-core";
    if (QFileInfo(beside).isExecutable())
        return beside;
    return "/usr/lib/qveil/qveil-core";
}

QString Backend::parseReady(const QByteArray &line) {
    const QString base = QJsonDocument::fromJson(line).object().value("base").toString();
    // The engine only ever listens on loopback; refuse anything else.
    return base.startsWith("http://127.0.0.1:") ? base : QString();
}

QString Backend::scrub(QString text) {
    {
        QMutexLocker lock(&secretsLock);
        for (const QString &secret : std::as_const(secrets))
            text.replace(secret, secret.contains('/') ? QStringLiteral("qveil-core") : QStringLiteral("<token>"));
    }
    // A thumbnail's address names its video, and so does its batch. Qt quotes
    // the address of one that fails to load, and the journal would then say
    // what was on screen. (At least one character, so that scrubbing what has
    // been scrubbed leaves it alone.)
    static const QRegularExpression thumb(QStringLiteral("thumb/[^\\s\"'<>)]+"));
    text.replace(thumb, QStringLiteral("thumb/<id>"));
    return text;
}

QVariantMap Backend::privacyOf(const QVariantMap &config) {
    const QVariantMap sent = config.value("privacy").toMap();
    auto text = [](const QVariantMap &from, const char *key) { return from.value(key).toString(); };
    QVariantMap routes, pages;
    const QVariantMap sentRoutes = sent.value("routes").toMap(), sentPages = sent.value("pages").toMap();
    for (const char *key : {"direct", "tor", "proxy"})
        routes.insert(key, text(sentRoutes, key));
    for (const char *key : {"search", "subscriptions", "history_off", "history_on", "resume", "comments", "lenses"})
        pages.insert(key, text(sentPages, key));
    QStringList points;
    for (const QVariant &point : sent.value("points").toList())
        if (!point.toString().isEmpty())
            points << point.toString();
    return {{"summary", text(sent, "summary")}, {"points", points}, {"routes", routes}, {"pages", pages}};
}

bool Backend::offersRoute(const QString &code) {
    // The ones a different route may get past: YouTube refusing this IP, a
    // route that isn't there, and YouTube slowing this IP down.
    static const QSet<QString> codes{"bot_check", "tor_down", "proxy_refused", "rate_limited"};
    return codes.contains(code);
}

void Backend::scrubLogs() {
    static bool installed = false;
    // A second time would make the handler its own predecessor.
    if (std::exchange(installed, true))
        return;
    previousHandler = qInstallMessageHandler([](QtMsgType type, const QMessageLogContext &context, const QString &message) {
        const QString clean = scrub(message);
        if (previousHandler)
            previousHandler(type, context, clean);
        else
            qt_message_output(type, context, clean);
    });
}

QString Backend::failure(const QNetworkReply *reply) { return scrub(reply->errorString()); }

void Backend::start() {
    if (m_core.state() != QProcess::NotRunning)
        return;
    // A restarted engine must not inherit the last one's ready line or last words.
    m_coreStdout.clear();
    m_coreStderr.clear();
    m_core.setProcessEnvironment(m_environment);
    m_core.setProgram(m_corePath);
    m_core.setArguments({"serve"});
    m_core.start();
}

void Backend::onEngineReady() {
    // Only an engine that came back has lost the window's state: the first
    // one is loaded by the page that asked for it.
    const bool recovered = m_hadReady;
    m_hadReady = true;
    m_stable.start();
    emit readyChanged();
    loadConfig();
    if (recovered) {
        loadSubs();
        loadFeed(false, m_shorts);
        loadResume();
        loadLenses();
    }
}

void Backend::onEngineStopped() {
    if (m_quitting)
        return;
    const bool wasReady = ready();
    m_stable.stop();
    // Whatever was in flight belonged to the dead engine; its answers are
    // dropped, not shown as connection errors.
    ++m_generation;
    m_base.clear();
    if (wasReady)
        emit readyChanged();

    // An engine that died before it was ready would die the same way again,
    // so it isn't restarted: its own words say more than a retry would.
    if (!wasReady) {
        const QStringList lines = QString::fromUtf8(m_coreStderr).trimmed().split('\n', Qt::SkipEmptyParts);
        const QString detail = lines.mid(qMax(0, lines.size() - 4)).join('\n');
        m_fatal = detail.isEmpty() ? tr("The qveil engine stopped before it was ready.")
                                   : tr("The qveil engine couldn't start:\n%1").arg(detail);
        setError(m_fatal);
        return;
    }

    if (m_restarts < MaxRestarts) {
        ++m_restarts;
        // Clears an error caused by the death itself, such as a dropped connection.
        setError({});
        // Queued, so the process isn't restarted from inside its own signal.
        QMetaObject::invokeMethod(this, &Backend::start, Qt::QueuedConnection);
        return;
    }

    const QString detail = QString::fromUtf8(m_coreStderr).trimmed().section('\n', -1);
    m_fatal = detail.isEmpty() ? tr("The qveil engine stopped again. Close and reopen qveil to continue.")
                               : tr("The qveil engine stopped again: %1. Close and reopen qveil to continue.").arg(detail);
    setError(m_fatal);
}

void Backend::setError(const QString &message, const QString &code) {
    if (m_error == message && m_errorCode == code)
        return;
    m_error = message;
    m_errorCode = message.isEmpty() ? QString() : code;
    emit errorChanged();
}

void Backend::fail(const QString &message, const QString &code, const QString &fallback) {
    // The user caused this one: they changed the route while the request ran,
    // and the engine ended it. They know.
    if (code == "route_changed")
        return;
    setError(message.isEmpty() ? fallback : message, code);
}

void Backend::reportError(const QString &message) { setError(message); }
void Backend::clearError() { setError({}); }

void Backend::setBusy(const QString &task, int delta) {
    const int n = m_busy.value(task) + delta;
    if (n > 0)
        m_busy.insert(task, n);
    else
        m_busy.remove(task);
    emit busyChanged();
}

void Backend::refuse(const QString &task) {
    // While the engine restarts, requests are dropped and the restart reloads
    // what the window shows. Once it is gone for good, say so instead of
    // ignoring the click.
    if (!m_fatal.isEmpty())
        setError(m_fatal);
    emit taskFailed(task);
}

bool Backend::newestOnly(const QString &task) {
    static const QSet<QString> tasks{"search", "channel", "play", "feed", "feed-cache", "comments", "lens", "read"};
    return tasks.contains(task);
}

quint64 Backend::supersede(const QString &task) {
    const quint64 id = ++m_sequence;
    m_latest.insert(task, id);
    if (QNetworkReply *old = m_replies.take(task))
        old->abort();
    return id;
}

void Backend::call(const QByteArray &verb, const QString &path, const QUrlQuery &query, const QByteArray &body,
                   const QString &task, Done done, bool everyAnswer) {
    if (!ready()) {
        refuse(task);
        return;
    }
    QUrl url(m_base + "/" + path);
    url.setQuery(query);
    QNetworkRequest request(url);
    // A stuck request still ends eventually. The slow ones (a feed refresh
    // over Tor) are streamed, which keeps this from running out (see stream()).
    request.setTransferTimeout(m_idleTimeout);
    request.setHeader(QNetworkRequest::ContentTypeHeader, "application/octet-stream");

    const quint64 id = supersede(task);
    const quint64 generation = m_generation;
    setBusy(task, +1);
    QNetworkReply *reply = m_net.sendCustomRequest(request, verb, body);
    if (newestOnly(task))
        m_replies.insert(task, reply);
    connect(reply, &QNetworkReply::finished, this, [this, reply, task, id, generation, done, everyAnswer] {
        reply->deleteLater();
        if (m_replies.value(task) == reply)
            m_replies.remove(task);
        setBusy(task, -1);
        if (generation != m_generation)
            return;
        // Hung up on by supersede(): it is no failure, and nobody is waiting for it.
        if (reply->error() == QNetworkReply::OperationCanceledError)
            return;
        const QByteArray data = reply->readAll();
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const QJsonDocument doc = QJsonDocument::fromJson(data);
        if (status != 200) {
            const QJsonObject body = doc.object();
            fail(body.value("error").toString(), body.value("code").toString(), failure(reply));
            if (m_latest.value(task) == id)
                emit taskFailed(task);
            return;
        }
        if (!everyAnswer && m_latest.value(task) != id)
            return;
        done(doc.isArray() ? QJsonValue(doc.array()) : QJsonValue(doc.object()));
    });
}

void Backend::loadConfig() {
    call("GET", "api/config", {}, {}, "config", [this](const QJsonValue &v) {
        adoptConfig(v.toObject().toVariantMap(), false);
    });
}

void Backend::adoptConfig(const QVariantMap &next, bool routeSet) {
    // A proxy address only matters while the proxy is the route, and an
    // unchanged one comes back redacted, so it compares equal to itself.
    auto way = [](const QVariantMap &config) {
        const QString route = config.value("route").toString();
        return route == "proxy" ? route + ' ' + config.value("proxy").toString() : route;
    };
    const bool moved = !m_config.isEmpty() && (routeSet || way(next) != way(m_config));
    m_config = next;
    emit configChanged();
    if (!moved)
        return;
    // A video that was still being looked up went through the old route, so
    // its answer must not open a player now.
    supersede("play");
    emit routeChanged();
}

void Backend::setConfig(const QVariantMap &patch) {
    call("POST", "api/config", {}, QJsonDocument(QJsonObject::fromVariantMap(patch)).toJson(QJsonDocument::Compact),
         "config", [this, patch](const QJsonValue &v) {
             const QVariantMap next = v.toObject().toVariantMap();
             // The engine reports a saved proxy redacted, so a different
             // string than the one it reported is a new address, even if it
             // redacts to the same text (a changed password).
             const bool newProxy = next.value("route").toString() == "proxy" && patch.contains("proxy")
                                   && patch.value("proxy").toString() != m_config.value("proxy").toString();
             adoptConfig(next, newProxy);
             // Fewer days, or none, takes kept positions with it.
             if (patch.contains("resumeDays"))
                 loadResume();
             emit notice(tr("Settings saved"));
         });
}

void Backend::refreshFeed(bool network, bool shorts) {
    m_shorts = shorts;
    // A refresh can take minutes over Tor, so a second one while it runs would
    // only repeat the work. Every caller (startup, the button, the R shortcut)
    // gets this guard.
    if (network && m_busy.contains("feed"))
        return;
    loadFeed(network, shorts);
}

void Backend::loadFeed(bool network, bool shorts, const QString &channel) {
    QUrlQuery q;
    // The engine fetches only channels it hasn't fetched in the last 15
    // minutes; a single channel is fetched whatever its age.
    q.addQueryItem("refresh", network ? "1" : "0");
    if (network && !channel.isEmpty())
        q.addQueryItem("channel", channel);
    q.addQueryItem("shorts", shorts ? "1" : "0");

    // Refreshing every stale channel is a job of minutes over Tor, one request
    // for each channel. The engine runs it once however many windows ask, says
    // how far it is as it goes, and ends with the feed. A refresh of one
    // channel is a single request and answers like any other.
    if (network && channel.isEmpty()) {
        q.addQueryItem("stream", "1");
        stream(
            "api/feed", q, "feed",
            [this, shorts](const QVariantList &lines) {
                for (const QVariant &line : lines) {
                    const QVariantMap o = line.toMap();
                    if (o.contains("progress")) {
                        const QVariantMap progress = o.value("progress").toMap();
                        setRefreshProgress(progress.value("done").toInt(), progress.value("total").toInt());
                    } else if (o.contains("result")) {
                        applyFeed(o.value("result").toMap(), true, shorts);
                    } else if (o.contains("entries")) {
                        // An engine that doesn't stream a refresh answers with the feed itself.
                        applyFeed(o, true, shorts);
                    }
                }
            },
            [this] { setRefreshProgress(0, 0); });
        return;
    }

    // The cached feed and the network refresh are separate tasks, so the quick
    // cached answer shows while the refresh is still running.
    call("GET", "api/feed", q, {}, network ? "feed" : "feed-cache", [this, network, shorts](const QJsonValue &v) {
        applyFeed(v.toObject().toVariantMap(), network, shorts);
    });
}

void Backend::applyFeed(const QVariantMap &answer, bool network, bool shorts) {
    // Asked for before the Shorts choice changed, so its videos are the wrong
    // ones. A refresh that was still running has saved what it fetched, though,
    // so the cache has that now, for the choice that stands.
    if (shorts != m_shorts) {
        if (network)
            loadFeed(false, m_shorts);
        return;
    }
    newPage(m_feed);
    m_feed->assign(answer.value("entries").toList());
    m_feedInfo = answer;
    m_feedInfo.remove("entries");
    if (network)
        m_feedInfo.insert("refreshedAt", QDateTime::currentSecsSinceEpoch());
    emit feedChanged();
}

void Backend::setRefreshProgress(int done, int total) {
    total = qMax(0, total);
    done = qBound(0, done, total);
    if (done == m_refreshDone && total == m_refreshTotal)
        return;
    m_refreshDone = done;
    m_refreshTotal = total;
    emit refreshProgressChanged();
}

void Backend::newPage(VideoModel *model) { model->setBatch(++m_batch); }

void Backend::search(const QString &query) {
    const QString q = query.trimmed();
    if (q.isEmpty())
        return;
    m_query = q;
    m_results->clear();
    newPage(m_results);
    emit queryChanged();
    QUrlQuery params;
    params.addQueryItem("q", q);
    // YouTube answers 20 results at a time; the first page is shown while
    // the second is on its way.
    params.addQueryItem("stream", "1");
    stream("api/search", params, "search", [this](const QVariantList &batch) { m_results->append(batch); });
}

void Backend::stream(const QString &path, const QUrlQuery &query, const QString &task, Batch batch,
                     std::function<void()> ended) {
    if (!ready()) {
        refuse(task);
        if (ended)
            ended();
        return;
    }
    QUrl url(m_base + "/" + path);
    url.setQuery(query);
    QNetworkRequest request(url);
    // Qt counts this from the last bytes received, so a stream that keeps
    // sending (a refresh sends a line per channel) never runs into it.
    request.setTransferTimeout(m_idleTimeout);

    const quint64 id = supersede(task);
    const quint64 generation = m_generation;
    setBusy(task, +1);
    QNetworkReply *reply = m_net.get(request);
    m_replies.insert(task, reply);

    struct State {
        QByteArray buffer;
        QVariantList lines;
        QString error;
        QString code;
    };
    auto state = std::make_shared<State>();
    auto *flush = new QTimer(reply);
    flush->setSingleShot(true);
    flush->setInterval(BatchMs);
    // Only the latest request of the task, from the engine that is running, gets through.
    auto current = [this, task, id, generation] { return generation == m_generation && m_latest.value(task) == id; };
    auto deliver = [state, batch, current] {
        QVariantList lines = std::exchange(state->lines, {});
        if (!lines.isEmpty() && current())
            batch(lines);
    };
    // Takes the complete lines out of the buffer; `all` takes a last one without a newline too.
    auto parse = [state](bool all) {
        for (;;) {
            qsizetype end = state->buffer.indexOf('\n');
            if (end < 0 && !(all && !state->buffer.trimmed().isEmpty()))
                return;
            if (end < 0)
                end = state->buffer.size();
            const QJsonObject o = QJsonDocument::fromJson(state->buffer.left(end)).object();
            state->buffer.remove(0, end + 1);
            if (o.contains("error")) {
                state->error = o.value("error").toString();
                state->code = o.value("code").toString();
            } else if (!o.isEmpty())
                state->lines.append(o.toVariantMap());
        }
    };
    connect(flush, &QTimer::timeout, this, deliver);
    connect(reply, &QNetworkReply::readyRead, this, [reply, state, flush, parse] {
        // An error comes whole, and is read when the reply has finished.
        if (reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt() != 200)
            return;
        state->buffer += reply->readAll();
        parse(false);
        if (!state->lines.isEmpty() && !flush->isActive())
            flush->start();
    });
    connect(reply, &QNetworkReply::finished, this, [this, reply, state, flush, parse, deliver, current, task, ended] {
        reply->deleteLater();
        if (m_replies.value(task) == reply)
            m_replies.remove(task);
        flush->stop();
        // However this goes on, `ended` hears of it last: lines that were
        // waiting to be handed on, a refresh's last progress among them, come
        // before it.
        struct Last {
            std::function<void()> run;
            ~Last() {
                if (run)
                    run();
            }
        } last{ended};
        setBusy(task, -1);
        // Hung up on by supersede(): what it had sent is no longer wanted,
        // and its device is closed.
        if (reply->error() == QNetworkReply::OperationCanceledError)
            return;
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        if (status != 200) {
            if (!current())
                return;
            const QJsonObject body = QJsonDocument::fromJson(reply->readAll()).object();
            fail(body.value("error").toString(), body.value("code").toString(), failure(reply));
            emit taskFailed(task);
            return;
        }
        // A request that timed out has been closed, and has nothing to read.
        if (reply->isOpen())
            state->buffer += reply->readAll();
        parse(true);
        deliver();
        if (!current())
            return;
        // The engine's own words if it said any; else the connection's (cut
        // short, or silent for too long).
        if (!state->error.isEmpty() || reply->error() != QNetworkReply::NoError) {
            fail(state->error, state->code, failure(reply));
            emit taskFailed(task);
        }
    });
}

void Backend::openChannel(const QString &id) {
    if (id.isEmpty())
        return;
    // Opening the channel that is already open (refresh) leaves its videos
    // where they are until the answer updates them.
    if (m_channel.value("id").toString() != id) {
        m_channel = QVariantMap{{"id", id}, {"name", QString()}};
        m_channelVideos->clear();
        newPage(m_channelVideos);
        emit channelChanged();
    }
    QUrlQuery q;
    q.addQueryItem("id", id);
    call("GET", "api/channel", q, {}, "channel", [this](const QJsonValue &v) {
        QJsonObject page = v.toObject();
        // The videos of a channel opened again (refreshed) are a new page too;
        // the cards that stay keep the thumbnails they have.
        newPage(m_channelVideos);
        m_channelVideos->assign(page.take("videos").toArray().toVariantList());
        m_channel = page.toVariantMap();
        emit channelChanged();
    });
}

void Backend::loadSubs() {
    call("GET", "api/subs", {}, {}, "subs", [this](const QJsonValue &v) {
        m_subs = v.toArray().toVariantList();
        emit subsChanged();
    });
}

void Backend::subscribe(const QString &input) {
    QUrlQuery q;
    q.addQueryItem("c", input.trimmed());
    call("POST", "api/subs/add", q, {}, "subscribe", [this](const QJsonValue &v) {
        const QJsonObject sub = v.toObject().value("sub").toObject();
        const QString name = sub.value("name").toString();
        emit notice(v.toObject().value("added").toBool() ? tr("Subscribed to %1").arg(name)
                                                         : tr("Already subscribed to %1").arg(name));
        if (m_channel.value("id").toString() == sub.value("id").toString()) {
            m_channel.insert("subscribed", true);
            emit channelChanged();
        }
        loadSubs();
        // Only the new channel needs fetching: the others are as fresh as
        // they were, and a refresh of all of them is a burst of requests that
        // would link the whole list. A running refresh started with the old
        // list, so this one is not held back by the guard of refreshFeed().
        loadFeed(true, m_shorts, sub.value("id").toString());
    }, true);
}

void Backend::unsubscribe(const QString &id) {
    QUrlQuery q;
    q.addQueryItem("id", id);
    call("POST", "api/subs/rm", q, {}, "unsubscribe", [this, id](const QJsonValue &v) {
        emit notice(tr("Unsubscribed from %1").arg(v.toObject().value("name").toString()));
        if (m_channel.value("id").toString() == id) {
            m_channel.insert("subscribed", false);
            emit channelChanged();
        }
        loadSubs();
        loadFeed(false, m_shorts);
    }, true);
}

void Backend::importFile(const QUrl &file) {
    QFile f(file.toLocalFile());
    if (!file.isLocalFile() || !f.open(QIODevice::ReadOnly)) {
        setError(tr("Couldn't read %1.").arg(file.toDisplayString()));
        return;
    }
    if (f.size() > 8 * 1024 * 1024) {
        setError(tr("%1 is too large to be a subscriptions export.").arg(QFileInfo(f).fileName()));
        return;
    }
    call("POST", "api/subs/import", {}, f.readAll(), "import", [this](const QJsonValue &v) {
        const int added = v.toObject().value("added").toInt();
        emit notice(tr("Imported %n new channel(s)", "", added));
        loadSubs();
        // The imported channels have never been fetched, so they count as stale.
        loadFeed(true, m_shorts);
    }, true);
}

void Backend::loadHistory() {
    call("GET", "api/history", {}, {}, "history", [this](const QJsonValue &v) {
        m_history = v.toArray().toVariantList();
        emit historyChanged();
    });
}

void Backend::clearHistory() {
    call("POST", "api/history/clear", {}, {}, "history", [this](const QJsonValue &) {
        m_history.clear();
        emit historyChanged();
        emit notice(tr("History cleared"));
    });
}

void Backend::play(const QString &video) {
    QUrlQuery q;
    q.addQueryItem("v", video);
    call("POST", "api/play", q, {}, "play", [this](const QJsonValue &v) {
        emit playReady(v.toObject().toVariantMap());
        if (m_config.value("history").toBool())
            loadHistory();
    });
}

void Backend::cancelPlay() { supersede("play"); }

void Backend::loadResume() {
    call("GET", "api/resume", {}, {}, "resume", [this](const QJsonValue &v) {
        m_resume = v.toArray().toVariantList();
        emit resumeChanged();
    });
}

void Backend::noteResume(const QVariantMap &video, double position, double duration) {
    const QString id = video.value("id").toString();
    // Until the player knows the length there is nothing to judge a position by.
    if (id.isEmpty() || !(duration > 0) || position < 0)
        return;
    QJsonObject body{{"v", id},
                     {"pos", qint64(position)},
                     {"dur", qint64(duration)},
                     {"title", video.value("title").toString()}};
    if (!video.value("channel").toString().isEmpty())
        body.insert("channel", video.value("channel").toString());
    call("POST", "api/resume", {}, QJsonDocument(body).toJson(QJsonDocument::Compact), "resume-note",
         [this, id](const QJsonValue &v) {
             // Only what changes the list is worth reading it again for.
             const QString noted = v.toObject().value("noted").toString();
             bool listed = false;
             for (const QVariant &kept : std::as_const(m_resume))
                 listed = listed || kept.toMap().value("id").toString() == id;
             if (noted == "kept" || listed)
                 loadResume();
             if (noted == "kept")
                 emit positionKept();
         },
         true);
}

void Backend::forgetResume(const QString &id) {
    QUrlQuery q;
    q.addQueryItem("v", id);
    call("POST", "api/resume/rm", q, {}, "resume-note", [this](const QJsonValue &) { loadResume(); }, true);
}

void Backend::clearResume() {
    call("POST", "api/resume/clear", {}, {}, "resume-note", [this](const QJsonValue &) { loadResume(); }, true);
}

void Backend::loadComments(const QString &video, const QString &sort, int n, bool replies) {
    QUrlQuery q;
    q.addQueryItem("v", video);
    q.addQueryItem("sort", sort);
    q.addQueryItem("n", QString::number(n));
    q.addQueryItem("replies", replies ? "1" : "0");
    call("GET", "api/comments", q, {}, "comments", [this, video, n, replies](const QJsonValue &v) {
        const QVariantMap answer = v.toObject().toVariantMap();
        m_comments = {{"video", video},
                      {"sort", answer.value("sort")},
                      {"n", n},
                      {"replies", replies},
                      {"count", answer.value("count")},
                      {"items", answer.value("comments")}};
        emit commentsChanged();
    });
}

void Backend::closeComments() {
    // The engine sees the hang-up and stops looking.
    supersede("comments");
    if (m_comments.isEmpty())
        return;
    m_comments.clear();
    emit commentsChanged();
}

void Backend::loadLenses() {
    call("GET", "api/lenses", {}, {}, "lenses", [this](const QJsonValue &v) {
        m_lenses = v.toArray().toVariantList();
        emit lensesChanged();
    });
}

void Backend::addLens(const QString &text) {
    const QByteArray body = QJsonDocument(QJsonObject{{"text", text}}).toJson(QJsonDocument::Compact);
    call("POST", "api/lenses", {}, body, "lens-write", [this](const QJsonValue &v) {
        loadLenses();
        emit lensAdded(v.toObject().value("id").toString());
    }, true);
}

void Backend::editLens(const QString &id, const QString &text) {
    const QByteArray body = QJsonDocument(QJsonObject{{"text", text}}).toJson(QJsonDocument::Compact);
    call("POST", "api/lenses/" + id, {}, body, "lens-write", [this, id](const QJsonValue &) {
        loadLenses();
        // Its results were for the old words; saving new ones asks for them again.
        if (m_lensInfo.value("id").toString() == id)
            openLens(id, 1);
    }, true);
}

void Backend::removeLens(const QString &id) {
    call("POST", "api/lenses/" + id + "/rm", {}, {}, "lens-write", [this, id](const QJsonValue &) {
        if (m_lensInfo.value("id").toString() == id)
            closeLens();
        loadLenses();
    }, true);
}

void Backend::closeLens() {
    supersede("lens");
    m_lensVideos->clear();
    setLensProgress(0, 0);
    if (m_lensInfo.isEmpty())
        return;
    m_lensInfo.clear();
    emit lensChanged();
}

void Backend::setLensProgress(int done, int total) {
    total = qMax(0, total);
    done = qBound(0, done, total);
    if (done == m_lensDone && total == m_lensTotal)
        return;
    m_lensDone = done;
    m_lensTotal = total;
    emit lensProgressChanged();
}

void Backend::openLens(const QString &id, int refresh) {
    // Another lens's videos are not this one's: they go at once, and this
    // one's come when the engine has them.
    if (m_lensInfo.value("id").toString() != id) {
        m_lensVideos->clear();
        for (const QVariant &listed : std::as_const(m_lenses)) {
            const QVariantMap lens = listed.toMap();
            if (lens.value("id").toString() == id)
                m_lensInfo = {{"id", id}, {"text", lens.value("text")}, {"tags", lens.value("tags")},
                              {"searches", lens.value("searches")}};
        }
        if (m_lensInfo.value("id").toString() != id)
            m_lensInfo = {{"id", id}};
        emit lensChanged();
    }
    QUrlQuery q;
    q.addQueryItem("id", id);
    q.addQueryItem("stream", "1");
    q.addQueryItem(refresh >= 2 ? "force" : "refresh", refresh >= 1 ? "1" : "0");
    stream(
        "api/lens", q, "lens",
        [this](const QVariantList &lines) {
            for (const QVariant &line : lines) {
                const QVariantMap o = line.toMap();
                if (o.contains("progress")) {
                    const QVariantMap progress = o.value("progress").toMap();
                    setLensProgress(progress.value("done").toInt(), progress.value("total").toInt());
                } else if (o.contains("result")) {
                    const QVariantMap result = o.value("result").toMap();
                    const QVariantMap lens = result.value("lens").toMap();
                    newPage(m_lensVideos);
                    m_lensVideos->assign(result.value("videos").toList());
                    m_lensInfo = {{"id", lens.value("id")},
                                  {"text", lens.value("text")},
                                  {"tags", result.value("tags")},
                                  {"searches", result.value("searches")},
                                  {"fetchedAt", result.value("fetchedAt")}};
                    emit lensChanged();
                    // How many it has, and when they were fetched, is in the list too.
                    loadLenses();
                }
            }
        },
        [this] { setLensProgress(0, 0); });
}

void Backend::read(const QString &text, bool lens) {
    const QString typed = text.trimmed();
    if (typed.isEmpty()) {
        // Nothing to read; an answer still on its way is for words that are gone.
        supersede("read");
        if (!m_reading.isEmpty()) {
            m_reading.clear();
            emit readingChanged();
        }
        return;
    }
    QUrlQuery q;
    q.addQueryItem("q", typed);
    if (lens)
        q.addQueryItem("for", "lens");
    call("GET", "api/read", q, {}, "read", [this, typed](const QJsonValue &v) {
        m_reading = v.toObject().toVariantMap();
        m_reading.insert("text", typed);
        emit readingChanged();
    });
}

QString Backend::thumbBase() const {
    if (!ready() || m_config.isEmpty() || !m_config.value("thumbnails", true).toBool())
        return {};
    return m_base + "/thumb/";
}
