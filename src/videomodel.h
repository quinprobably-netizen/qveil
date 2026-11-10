#pragma once

#include <QAbstractListModel>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>
#include <QVector>

// Videos, keyed by their ID: the feed, a search's results and a channel's
// videos are each one of these. An update is applied as the fewest inserts,
// removes, moves and changes that turn the old rows into the new ones, so a
// view keeps the cards (and the selection) of every video that is still
// there. Replacing a QVariantList instead would make every view start over.
//
// The roles are named like the engine's JSON keys, so a delegate reads
// `model.title` or `model.channel_id`; `get(row)` returns the whole entry.
//
// Live streams are not shown (the engine marks them `"live": true`), because
// qveil can't play them: no grid has a card for one.
class VideoModel : public QAbstractListModel {
    Q_OBJECT
    Q_PROPERTY(int count READ count NOTIFY countChanged)

public:
    explicit VideoModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = {}) const override;
    QVariant data(const QModelIndex &index, int role = Qt::DisplayRole) const override;
    QHash<int, QByteArray> roleNames() const override;

    int count() const { return m_rows.size(); }
    // The entry in `row` as the engine sent it, or an empty map past the end.
    Q_INVOKABLE QVariantMap get(int row) const;
    // The IDs in order, which is what the tests compare views against.
    QStringList ids() const;

    // Names the page the rows added from now on belong to; 0 (the start) names none.
    // A row keeps the number it was added with, so the cards of a page that
    // is updated in place aren't told anything, and their thumbnails don't
    // load again. The engine puts the thumbnails of one number on one Tor
    // circuit (`?batch=<n>`), which is what a browser's page load looks like.
    void setBatch(int batch) { m_batch = batch; }
    int batch() const { return m_batch; }

    // Makes the rows exactly `entries`. An entry without an ID is skipped,
    // and so is a repeat of an ID already seen (the first one counts) and a
    // live stream.
    Q_INVOKABLE void assign(const QVariantList &entries);
    // Adds the entries whose IDs aren't there yet, after the last row.
    void append(const QVariantList &entries);
    void clear();

signals:
    void countChanged();

private:
    static QString idOf(const QVariantMap &entry) { return entry.value(QStringLiteral("id")).toString(); }
    // `entries` without those that have no ID or repeat one.
    static QVector<QVariantMap> distinct(const QVariantList &entries);
    void removeRange(int first, int last);
    void move(int from, int to);
    void insertRange(int at, const QVector<QVariantMap> &entries);
    void update(int row, QVariantMap entry);

    QVector<QVariantMap> m_rows;
    int m_batch = 0;
};
