#pragma once

#include <QHash>
#include <QJsonValue>
#include <QNetworkAccessManager>
#include <QObject>
#include <QProcess>
#include <QProcessEnvironment>
#include <QTimer>
#include <QUrl>
#include <QUrlQuery>
#include <QVariantList>
#include <QVariantMap>

#include <functional>

// The window's view of qveil-core. It starts `qveil-core serve`, then talks to
// it over loopback HTTP; every result lands in a property QML binds to.
class Backend : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool ready READ ready NOTIFY readyChanged)
    Q_PROPERTY(QString error READ error NOTIFY errorChanged)
    Q_PROPERTY(QStringList busy READ busy NOTIFY busyChanged)
    Q_PROPERTY(QVariantMap config READ config NOTIFY configChanged)
    Q_PROPERTY(QVariantList feed READ feed NOTIFY feedChanged)
    Q_PROPERTY(QVariantMap feedInfo READ feedInfo NOTIFY feedChanged)
    Q_PROPERTY(QVariantList results READ results NOTIFY resultsChanged)
    Q_PROPERTY(QString query READ query NOTIFY resultsChanged)
    Q_PROPERTY(QVariantMap channel READ channel NOTIFY channelChanged)
    Q_PROPERTY(QVariantList subs READ subs NOTIFY subsChanged)
    Q_PROPERTY(QVariantList history READ history NOTIFY historyChanged)
    // Thumbnail URLs are thumbBase + video ID; empty while unavailable or switched off.
    Q_PROPERTY(QString thumbBase READ thumbBase NOTIFY configChanged)

public:
    explicit Backend(const QProcessEnvironment &environment = QProcessEnvironment::systemEnvironment(),
                     const QString &core = {}, QObject *parent = nullptr);
    ~Backend() override;

    // qveil-core next to this binary (a build tree), else the installed one.
    static QString corePath();
    // The base URL from `qveil-core serve`'s first line, or empty.
    static QString parseReady(const QByteArray &line);

    // Starts the engine. Also how it is brought back after it dies.
    void start();

    bool ready() const { return !m_base.isEmpty(); }
    QString error() const { return m_error; }
    QStringList busy() const { return m_busy.keys(); }
    QVariantMap config() const { return m_config; }
    QVariantList feed() const { return m_feed; }
    QVariantMap feedInfo() const { return m_feedInfo; }
    QVariantList results() const { return m_results; }
    QString query() const { return m_query; }
    QVariantMap channel() const { return m_channel; }
    QVariantList subs() const { return m_subs; }
    QVariantList history() const { return m_history; }
    QString thumbBase() const;

    // `network` asks the engine to fetch. That covers the channels not fetched
    // in the last 15 minutes, or every channel with `force`.
    Q_INVOKABLE void refreshFeed(bool network, bool shorts, bool force = false);
    Q_INVOKABLE void search(const QString &query);
    Q_INVOKABLE void openChannel(const QString &id);
    Q_INVOKABLE void loadSubs();
    Q_INVOKABLE void subscribe(const QString &input);
    Q_INVOKABLE void unsubscribe(const QString &id);
    Q_INVOKABLE void importFile(const QUrl &file);
    Q_INVOKABLE void loadHistory();
    Q_INVOKABLE void clearHistory();
    Q_INVOKABLE void loadConfig();
    Q_INVOKABLE void setConfig(const QVariantMap &patch);
    Q_INVOKABLE void play(const QString &video);
    // Drops the answer of a video still being looked up: the player it was
    // for has been closed.
    Q_INVOKABLE void cancelPlay();
    Q_INVOKABLE void reportError(const QString &message);
    Q_INVOKABLE void clearError();

    // How long an engine must stay ready before a later death counts as a
    // new failure rather than a repeat of the last one.
    static constexpr int StableAfterMs = 60000;
    void setStableAfter(int milliseconds) { m_stable.setInterval(milliseconds); }

signals:
    void readyChanged();
    void errorChanged();
    void busyChanged();
    void configChanged();
    void feedChanged();
    void resultsChanged();
    void channelChanged();
    void subsChanged();
    void historyChanged();
    // The route or proxy was changed, so whatever plays over the old one must stop.
    void routeChanged();
    void playReady(const QVariantMap &info);
    // The latest request of `task` failed (the error itself is in `error`).
    void taskFailed(const QString &task);
    void notice(const QString &text);

private:
    using Done = std::function<void(const QJsonValue &)>;
    // One request to the engine. `task` drives the busy state. Reads deliver
    // only the newest answer per task, so a slow old search can't overwrite a
    // newer one; a write (`everyAnswer`) delivers each answer, because every
    // one of them changed something the window must hear about.
    void call(const QByteArray &verb, const QString &path, const QUrlQuery &query, const QByteArray &body,
              const QString &task, Done done, bool everyAnswer = false);
    void setError(const QString &message);
    void setBusy(const QString &task, int delta);
    void onEngineReady();
    void onEngineStopped();
    // The feed request itself, without the one-refresh-at-a-time guard of
    // refreshFeed(). `channel` limits a refresh to that one channel.
    void loadFeed(bool network, bool shorts, bool force = false, const QString &channel = {});
    // Takes a config from the engine, and says so if it moved the route.
    // `routeSet` is true when the window itself just asked for that route.
    void adoptConfig(const QVariantMap &next, bool routeSet);

    QProcessEnvironment m_environment;
    QString m_corePath;
    QProcess m_core;
    QByteArray m_coreStdout;
    QByteArray m_coreStderr;
    QNetworkAccessManager m_net;
    QString m_base;
    QString m_error;
    QHash<QString, int> m_busy;
    QHash<QString, quint64> m_latest;
    quint64 m_sequence = 0;
    bool m_quitting = false;

    // An engine that dies is restarted once. A second death in a row is final,
    // and so is a death before the engine was ever ready, which a restart
    // would only repeat (a bad config, say). Once an engine has been ready for
    // StableAfterMs, the next death is a fresh failure and gets its restart.
    static constexpr int MaxRestarts = 1;
    int m_restarts = 0;
    QTimer m_stable;
    bool m_hadReady = false;
    // Counts engine lives, so an answer from a dead engine can be told apart.
    quint64 m_generation = 0;
    // Set once the engine is gone for good; later requests show it again.
    QString m_fatal;
    // The Shorts choice of the last feed request, for reloading after a restart.
    bool m_shorts = false;

    QVariantMap m_config;
    QVariantList m_feed;
    QVariantMap m_feedInfo;
    QVariantList m_results;
    QString m_query;
    QVariantMap m_channel;
    QVariantList m_subs;
    QVariantList m_history;
};
