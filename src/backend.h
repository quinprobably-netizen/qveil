#pragma once

#include <QHash>
#include <QJsonValue>
#include <QNetworkAccessManager>
#include <QObject>
#include <QProcess>
#include <QProcessEnvironment>
#include <QUrl>
#include <QUrlQuery>
#include <QVariantList>
#include <QVariantMap>

#include <functional>

// The window's view of veil-core. It starts `veil-core serve`, then talks to
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

    // veil-core next to this binary (a build tree), else the installed one.
    static QString corePath();
    // The base URL from `veil-core serve`'s first line, or empty.
    static QString parseReady(const QByteArray &line);

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
    Q_INVOKABLE void reportError(const QString &message);
    Q_INVOKABLE void clearError();

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
    void playReady(const QVariantMap &info);
    void notice(const QString &text);

private:
    using Done = std::function<void(const QJsonValue &)>;
    // One request to the engine. `task` drives the busy state, and only the
    // newest request per task is delivered, so a slow old search can't
    // overwrite a newer one.
    void call(const QByteArray &verb, const QString &path, const QUrlQuery &query, const QByteArray &body,
              const QString &task, Done done);
    void setError(const QString &message);
    void setBusy(const QString &task, int delta);

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

    QVariantMap m_config;
    QVariantList m_feed;
    QVariantMap m_feedInfo;
    QVariantList m_results;
    QString m_query;
    QVariantMap m_channel;
    QVariantList m_subs;
    QVariantList m_history;
};
