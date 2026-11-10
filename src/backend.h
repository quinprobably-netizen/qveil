#pragma once

#include <QHash>
#include <QJsonValue>
#include <QNetworkAccessManager>
#include <QObject>
#include <QPointer>
#include <QProcess>
#include <QProcessEnvironment>
#include <QTimer>
#include <QUrl>
#include <QUrlQuery>
#include <QVariantList>
#include <QVariantMap>

#include <functional>

#include "videomodel.h"

class QNetworkReply;

// The window's view of qveil-core. It starts `qveil-core serve`, then talks to
// it over loopback HTTP; every result lands in a property QML binds to.
class Backend : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool ready READ ready NOTIFY readyChanged)
    Q_PROPERTY(QString error READ error NOTIFY errorChanged)
    // The engine's code for the error (see errorOffersRoute), or empty.
    Q_PROPERTY(QString errorCode READ errorCode NOTIFY errorChanged)
    // Whether the error is one that another route may get past, so the window
    // offers to change it.
    Q_PROPERTY(bool errorOffersRoute READ errorOffersRoute NOTIFY errorChanged)
    Q_PROPERTY(QStringList busy READ busy NOTIFY busyChanged)
    Q_PROPERTY(QVariantMap config READ config NOTIFY configChanged)
    // What the engine says about privacy, which the window shows as it is and
    // never words itself (see privacyOf).
    Q_PROPERTY(QVariantMap privacy READ privacy NOTIFY configChanged)
    // The videos of each page live in a model that is updated in place, so a
    // refresh landing in the background doesn't rebuild the grid showing it.
    Q_PROPERTY(VideoModel *feed READ feed CONSTANT)
    Q_PROPERTY(QVariantMap feedInfo READ feedInfo NOTIFY feedChanged)
    // How far the running refresh is: channels done of channels to do. Both
    // are 0 while no refresh runs, or one that hasn't said how many there are.
    Q_PROPERTY(int refreshDone READ refreshDone NOTIFY refreshProgressChanged)
    Q_PROPERTY(int refreshTotal READ refreshTotal NOTIFY refreshProgressChanged)
    Q_PROPERTY(VideoModel *results READ results CONSTANT)
    Q_PROPERTY(QString query READ query NOTIFY queryChanged)
    // The open channel's id, name and whether it is subscribed; its videos are `channelVideos`.
    Q_PROPERTY(QVariantMap channel READ channel NOTIFY channelChanged)
    Q_PROPERTY(VideoModel *channelVideos READ channelVideos CONSTANT)
    Q_PROPERTY(QVariantList subs READ subs NOTIFY subsChanged)
    Q_PROPERTY(QVariantList history READ history NOTIFY historyChanged)
    // Where unfinished videos were stopped: [{id, pos, dur, at, title, channel, expires}], newest first.
    Q_PROPERTY(QVariantList resume READ resume NOTIFY resumeChanged)
    // The comments last asked for: {video, sort, n, replies, count, items}, or
    // empty while there are none to show.
    Q_PROPERTY(QVariantMap comments READ comments NOTIFY commentsChanged)
    // The lenses, as the engine lists them: {id, text, tags, searches, count, fetchedAt}.
    Q_PROPERTY(QVariantList lenses READ lenses NOTIFY lensesChanged)
    // The lens that is open: {id, text, tags, searches, fetchedAt}; its videos are `lensVideos`.
    Q_PROPERTY(QVariantMap lensInfo READ lensInfo NOTIFY lensChanged)
    Q_PROPERTY(VideoModel *lensVideos READ lensVideos CONSTANT)
    // How far a lens's refresh is: searches done of searches to do, 0 of 0 while none runs.
    Q_PROPERTY(int lensDone READ lensDone NOTIFY lensProgressChanged)
    Q_PROPERTY(int lensTotal READ lensTotal NOTIFY lensProgressChanged)
    // How the text last given to read() was understood: {text, tags, searches, rules, error}.
    Q_PROPERTY(QVariantMap reading READ reading NOTIFY readingChanged)
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
    // `text` without the engine's base URL and token. The token is the only
    // thing keeping other programs off the engine, and Qt quotes request URLs
    // in its error text and warnings (a thumbnail that fails to load, say),
    // which would carry it into the window's error banner, the terminal and
    // the journal.
    static QString scrub(QString text);
    // Makes everything Qt logs go through scrub().
    static void scrubLogs();
    // The engine's privacy text from its config, in the shape QML reads
    // without checking: every field is there, empty when the engine didn't
    // send it, so the window shows nothing rather than words of its own.
    static QVariantMap privacyOf(const QVariantMap &config);
    // Whether another route may get past an error with this code.
    static bool offersRoute(const QString &code);

    // Starts the engine. Also how it is brought back after it dies.
    void start();

    bool ready() const { return !m_base.isEmpty(); }
    QString error() const { return m_error; }
    QString errorCode() const { return m_errorCode; }
    bool errorOffersRoute() const { return offersRoute(m_errorCode); }
    QStringList busy() const { return m_busy.keys(); }
    QVariantMap config() const { return m_config; }
    QVariantMap privacy() const { return privacyOf(m_config); }
    VideoModel *feed() const { return m_feed; }
    QVariantMap feedInfo() const { return m_feedInfo; }
    int refreshDone() const { return m_refreshDone; }
    int refreshTotal() const { return m_refreshTotal; }
    VideoModel *results() const { return m_results; }
    QString query() const { return m_query; }
    QVariantMap channel() const { return m_channel; }
    VideoModel *channelVideos() const { return m_channelVideos; }
    QVariantList subs() const { return m_subs; }
    QVariantList history() const { return m_history; }
    QVariantList resume() const { return m_resume; }
    QVariantMap comments() const { return m_comments; }
    QVariantList lenses() const { return m_lenses; }
    QVariantMap lensInfo() const { return m_lensInfo; }
    VideoModel *lensVideos() const { return m_lensVideos; }
    int lensDone() const { return m_lensDone; }
    int lensTotal() const { return m_lensTotal; }
    QVariantMap reading() const { return m_reading; }
    QString thumbBase() const;

    // `network` asks the engine to fetch the channels not fetched in the last
    // 15 minutes. The window never forces the others: pressing refresh again
    // must not repeat a burst of requests for exactly your channels.
    Q_INVOKABLE void refreshFeed(bool network, bool shorts);
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
    // Drops the answer of a video still being looked up, and hangs up on the
    // engine, which can then stop looking: the player it was for has been closed.
    Q_INVOKABLE void cancelPlay();
    Q_INVOKABLE void loadResume();
    // Tells the engine where `video` (id, title, channel) is. Says nothing
    // until the player knows how long the video is.
    Q_INVOKABLE void noteResume(const QVariantMap &video, double position, double duration);
    Q_INVOKABLE void forgetResume(const QString &id);
    Q_INVOKABLE void clearResume();
    // `n` threads in `sort` order ("top" or "new"), with replies if asked.
    Q_INVOKABLE void loadComments(const QString &video, const QString &sort, int n, bool replies);
    // Hangs up on a lookup still running and shows nothing: the tab was left.
    Q_INVOKABLE void closeComments();
    Q_INVOKABLE void loadLenses();
    Q_INVOKABLE void addLens(const QString &text);
    Q_INVOKABLE void editLens(const QString &id, const QString &text);
    Q_INVOKABLE void removeLens(const QString &id);
    // `refresh`: 0 shows what is stored, 1 fetches when that is stale, 2 always fetches.
    Q_INVOKABLE void openLens(const QString &id, int refresh);
    // Asks how `text` reads. Nothing leaves the machine for it.
    Q_INVOKABLE void read(const QString &text);
    Q_INVOKABLE void reportError(const QString &message);
    Q_INVOKABLE void clearError();

    // How long an engine must stay ready before a later death counts as a
    // new failure rather than a repeat of the last one.
    static constexpr int StableAfterMs = 60000;
    void setStableAfter(int milliseconds) { m_stable.setInterval(milliseconds); }
    // How long a request may go without receiving anything before it is given
    // up on. A refresh sends a line for each channel it has done, so it only
    // runs into this when the engine has stopped answering.
    static constexpr int IdleTimeoutMs = 300000;
    void setIdleTimeout(int milliseconds) { m_idleTimeout = milliseconds; }

signals:
    void readyChanged();
    void errorChanged();
    void busyChanged();
    void configChanged();
    void feedChanged();
    void refreshProgressChanged();
    void queryChanged();
    void channelChanged();
    void subsChanged();
    void historyChanged();
    void resumeChanged();
    void commentsChanged();
    void lensesChanged();
    void lensChanged();
    void lensProgressChanged();
    void readingChanged();
    // A lens was added; `id` is its.
    void lensAdded(const QString &id);
    // The route or proxy was changed, so whatever plays over the old one must stop.
    void routeChanged();
    void playReady(const QVariantMap &info);
    // The latest request of `task` failed (the error itself is in `error`).
    void taskFailed(const QString &task);
    void notice(const QString &text);

private:
    using Done = std::function<void(const QJsonValue &)>;
    using Batch = std::function<void(const QVariantList &)>;
    // One request to the engine. `task` drives the busy state. Reads deliver
    // only the newest answer per task, so a slow old search can't overwrite a
    // newer one; a write (`everyAnswer`) delivers each answer, because every
    // one of them changed something the window must hear about. When the
    // engine isn't there, the request fails at once (see refuse()).
    void call(const QByteArray &verb, const QString &path, const QUrlQuery &query, const QByteArray &body,
              const QString &task, Done done, bool everyAnswer = false);
    // A GET whose answer is one JSON object per line, sent as they are ready
    // (see qveil-core's streamed search). `batch` gets them a few at a time.
    // `ended` runs once the request is over, whatever became of it.
    void stream(const QString &path, const QUrlQuery &query, const QString &task, Batch batch,
                std::function<void()> ended = {});
    // Lines that arrive together are handed on together, so a page of
    // results is one update of the grid rather than twenty.
    static constexpr int BatchMs = 100;
    void setError(const QString &message, const QString &code = {});
    // Shows what the engine said about a failed request, or `fallback` when it
    // said nothing. An error whose cause is the user (they switched the route
    // while the request ran) is not shown.
    void fail(const QString &message, const QString &code, const QString &fallback);
    void setBusy(const QString &task, int delta);
    // A request that can't be sent because the engine isn't ready. Says why
    // when it is gone for good, and always says that `task` won't be answered,
    // so a player waiting on a video doesn't spin forever.
    void refuse(const QString &task);
    // Whether only the newest request of `task` matters, so that starting one
    // hangs up on the one before it. Not so for writes, or for tasks that
    // mix reads and writes: dropping a write's answer is bad enough, and
    // hanging up on it could stop it from happening at all.
    static bool newestOnly(const QString &task);
    // Makes the next request of `task` its latest, and hangs up on the reply
    // still running for the one before it. The engine sees the disconnect,
    // which ends its work for it if it can; the answer would be dropped anyway.
    quint64 supersede(const QString &task);
    // What went wrong with a failed request, as text that is safe to show.
    static QString failure(const QNetworkReply *reply);
    void onEngineReady();
    void onEngineStopped();
    // The feed request itself, without the one-refresh-at-a-time guard of
    // refreshFeed(). `channel` limits a refresh to that one channel.
    void loadFeed(bool network, bool shorts, const QString &channel = {});
    // Shows a feed the engine answered with, unless it was asked for with
    // another Shorts choice than the current one.
    void applyFeed(const QVariantMap &answer, bool network, bool shorts);
    void setRefreshProgress(int done, int total);
    void setLensProgress(int done, int total);
    // Forgets the lens that is open: it was removed, or another is being opened.
    void closeLens();
    // Starts a new page of thumbnails for `model` (see VideoModel::setBatch).
    void newPage(VideoModel *model);
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
    QString m_errorCode;
    QHash<QString, int> m_busy;
    QHash<QString, quint64> m_latest;
    // The request in flight for each task that is newestOnly().
    QHash<QString, QPointer<QNetworkReply>> m_replies;
    quint64 m_sequence = 0;
    int m_idleTimeout = IdleTimeoutMs;
    // The number of the latest page of thumbnails; see newPage().
    int m_batch;
    int m_refreshDone = 0;
    int m_refreshTotal = 0;
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
    VideoModel *m_feed = new VideoModel(this);
    QVariantMap m_feedInfo;
    VideoModel *m_results = new VideoModel(this);
    QString m_query;
    QVariantMap m_channel;
    VideoModel *m_channelVideos = new VideoModel(this);
    QVariantList m_subs;
    QVariantList m_history;
    QVariantList m_resume;
    QVariantMap m_comments;
    QVariantList m_lenses;
    QVariantMap m_lensInfo;
    VideoModel *m_lensVideos = new VideoModel(this);
    int m_lensDone = 0;
    int m_lensTotal = 0;
    QVariantMap m_reading;
};
