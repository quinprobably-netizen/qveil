#include "backend.h"

#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QNetworkProxy>
#include <QNetworkReply>
#include <QNetworkRequest>

Backend::Backend(const QProcessEnvironment &environment, const QString &core, QObject *parent)
    : QObject(parent), m_environment(environment), m_corePath(core.isEmpty() ? corePath() : core) {
    // Only loopback traffic leaves this process; see main.cpp.
    m_net.setProxy(QNetworkProxy::NoProxy);

    connect(&m_core, &QProcess::readyReadStandardOutput, this, [this] {
        if (ready())
            return;
        m_coreStdout += m_core.readAllStandardOutput();
        const int newline = m_coreStdout.indexOf('\n');
        if (newline < 0)
            return;
        m_base = parseReady(m_coreStdout.left(newline));
        if (m_base.isEmpty()) {
            setError(tr("The veil engine sent an unexpected reply."));
            return;
        }
        emit readyChanged();
        loadConfig();
    });
    connect(&m_core, &QProcess::readyReadStandardError, this, [this] {
        m_coreStderr = (m_coreStderr + m_core.readAllStandardError()).right(2000);
    });
    connect(&m_core, &QProcess::errorOccurred, this, [this](QProcess::ProcessError e) {
        if (e == QProcess::FailedToStart)
            setError(tr("Couldn't start the veil engine (%1).").arg(m_corePath));
    });
    connect(&m_core, &QProcess::finished, this, [this] {
        if (m_quitting)
            return;
        const QString detail = QString::fromUtf8(m_coreStderr).trimmed().section('\n', -1);
        setError(detail.isEmpty() ? tr("The veil engine stopped.") : tr("The veil engine stopped: %1").arg(detail));
        if (ready()) {
            m_base.clear();
            emit readyChanged();
        }
    });
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
    const QString beside = QFileInfo(self).absolutePath() + "/veil-core";
    if (QFileInfo(beside).isExecutable())
        return beside;
    return "/usr/lib/veil/veil-core";
}

QString Backend::parseReady(const QByteArray &line) {
    const QString base = QJsonDocument::fromJson(line).object().value("base").toString();
    // The engine only ever listens on loopback; refuse anything else.
    return base.startsWith("http://127.0.0.1:") ? base : QString();
}

void Backend::start() {
    m_core.setProcessEnvironment(m_environment);
    m_core.setProgram(m_corePath);
    m_core.setArguments({"serve"});
    m_core.start();
}

void Backend::setError(const QString &message) {
    if (m_error == message)
        return;
    m_error = message;
    emit errorChanged();
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

void Backend::call(const QByteArray &verb, const QString &path, const QUrlQuery &query, const QByteArray &body,
                   const QString &task, Done done) {
    if (!ready())
        return;
    QUrl url(m_base + "/" + path);
    url.setQuery(query);
    QNetworkRequest request(url);
    // Feeds over Tor can take minutes; a stuck request still ends eventually.
    request.setTransferTimeout(300000);
    request.setHeader(QNetworkRequest::ContentTypeHeader, "application/octet-stream");

    const quint64 id = ++m_sequence;
    m_latest.insert(task, id);
    setBusy(task, +1);
    QNetworkReply *reply = m_net.sendCustomRequest(request, verb, body);
    connect(reply, &QNetworkReply::finished, this, [this, reply, task, id, done] {
        reply->deleteLater();
        setBusy(task, -1);
        const QByteArray data = reply->readAll();
        const int status = reply->attribute(QNetworkRequest::HttpStatusCodeAttribute).toInt();
        const QJsonDocument doc = QJsonDocument::fromJson(data);
        if (status != 200) {
            const QString message = doc.object().value("error").toString();
            setError(message.isEmpty() ? reply->errorString() : message);
            return;
        }
        if (m_latest.value(task) != id)
            return;
        done(doc.isArray() ? QJsonValue(doc.array()) : QJsonValue(doc.object()));
    });
}

void Backend::loadConfig() {
    call("GET", "api/config", {}, {}, "config", [this](const QJsonValue &v) {
        m_config = v.toObject().toVariantMap();
        emit configChanged();
    });
}

void Backend::setConfig(const QVariantMap &patch) {
    call("POST", "api/config", {}, QJsonDocument(QJsonObject::fromVariantMap(patch)).toJson(QJsonDocument::Compact),
         "config", [this](const QJsonValue &v) {
             m_config = v.toObject().toVariantMap();
             emit configChanged();
             emit notice(tr("Settings saved"));
         });
}

void Backend::refreshFeed(bool network, bool shorts) {
    QUrlQuery q;
    q.addQueryItem("refresh", network ? "1" : "0");
    q.addQueryItem("shorts", shorts ? "1" : "0");
    // The cached feed and the network refresh are separate tasks, so the quick
    // cached answer shows while the refresh is still running.
    call("GET", "api/feed", q, {}, network ? "feed" : "feed-cache", [this, network](const QJsonValue &v) {
        if (!network && m_busy.contains("feed") && !m_feed.isEmpty())
            return;
        const QJsonObject o = v.toObject();
        m_feed = o.value("entries").toArray().toVariantList();
        m_feedInfo = o.toVariantMap();
        m_feedInfo.remove("entries");
        if (network)
            m_feedInfo.insert("refreshedAt", QDateTime::currentSecsSinceEpoch());
        emit feedChanged();
    });
}

void Backend::search(const QString &query) {
    const QString q = query.trimmed();
    if (q.isEmpty())
        return;
    m_query = q;
    m_results.clear();
    emit resultsChanged();
    QUrlQuery params;
    params.addQueryItem("q", q);
    call("GET", "api/search", params, {}, "search", [this](const QJsonValue &v) {
        m_results = v.toArray().toVariantList();
        emit resultsChanged();
    });
}

void Backend::openChannel(const QString &id) {
    if (id.isEmpty())
        return;
    m_channel = QVariantMap{{"id", id}, {"name", QString()}, {"videos", QVariantList()}};
    emit channelChanged();
    QUrlQuery q;
    q.addQueryItem("id", id);
    call("GET", "api/channel", q, {}, "channel", [this](const QJsonValue &v) {
        m_channel = v.toObject().toVariantMap();
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
        refreshFeed(true, false);
    });
}

void Backend::unsubscribe(const QString &id) {
    QUrlQuery q;
    q.addQueryItem("id", id);
    call("POST", "api/subs/rm", q, {}, "subscribe", [this, id](const QJsonValue &v) {
        emit notice(tr("Unsubscribed from %1").arg(v.toObject().value("name").toString()));
        if (m_channel.value("id").toString() == id) {
            m_channel.insert("subscribed", false);
            emit channelChanged();
        }
        loadSubs();
        refreshFeed(false, false);
    });
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
    call("POST", "api/subs/import", {}, f.readAll(), "subscribe", [this](const QJsonValue &v) {
        const int added = v.toObject().value("added").toInt();
        emit notice(tr("Imported %n new channel(s), offline", "", added));
        loadSubs();
        refreshFeed(true, false);
    });
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

QString Backend::thumbBase() const {
    if (!ready() || m_config.isEmpty() || !m_config.value("thumbnails", true).toBool())
        return {};
    return m_base + "/thumb/";
}
