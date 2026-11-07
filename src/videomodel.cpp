#include "videomodel.h"

#include <QHash>
#include <QSet>

#include <algorithm>

namespace {

// The keys of an engine entry that a delegate can read as roles. Others stay
// in the entry for get(), and a change in one of them tells no view anything.
const QList<QByteArray> &roleKeys() {
    static const QList<QByteArray> keys{"id",       "title",    "channel",  "channel_id", "published",
                                        "date",     "estimated", "duration", "views",      "short",
                                        "batch"};
    return keys;
}

// Which elements of `seq` (distinct numbers) make up one longest increasing
// run. These are the rows that need not move.
QVector<bool> longestIncreasing(const QVector<int> &seq) {
    // tails[k] is the index of the smallest last element of any increasing
    // run of length k + 1 seen so far; `before` links each element to the one
    // that precedes it in the best run ending there.
    QVector<int> tails, before(seq.size(), -1);
    for (int i = 0; i < seq.size(); ++i) {
        auto at = std::lower_bound(tails.begin(), tails.end(), seq[i], [&](int tail, int value) { return seq[tail] < value; });
        if (at != tails.begin())
            before[i] = *(at - 1);
        if (at == tails.end())
            tails.append(i);
        else
            *at = i;
    }
    QVector<bool> member(seq.size(), false);
    for (int i = tails.isEmpty() ? -1 : tails.last(); i >= 0; i = before[i])
        member[i] = true;
    return member;
}

} // namespace

VideoModel::VideoModel(QObject *parent) : QAbstractListModel(parent) {}

int VideoModel::rowCount(const QModelIndex &parent) const { return parent.isValid() ? 0 : m_rows.size(); }

QVariant VideoModel::data(const QModelIndex &index, int role) const {
    const int key = role - Qt::UserRole - 1;
    if (!index.isValid() || index.row() >= m_rows.size() || key < 0 || key >= roleKeys().size())
        return {};
    return m_rows.at(index.row()).value(QString::fromLatin1(roleKeys().at(key)));
}

QHash<int, QByteArray> VideoModel::roleNames() const {
    QHash<int, QByteArray> names;
    for (int i = 0; i < roleKeys().size(); ++i)
        names.insert(Qt::UserRole + 1 + i, roleKeys().at(i));
    return names;
}

QVariantMap VideoModel::get(int row) const { return row >= 0 && row < m_rows.size() ? m_rows.at(row) : QVariantMap(); }

QStringList VideoModel::ids() const {
    QStringList found;
    for (const QVariantMap &row : m_rows)
        found << idOf(row);
    return found;
}

QVector<QVariantMap> VideoModel::distinct(const QVariantList &entries) {
    QVector<QVariantMap> found;
    QSet<QString> seen;
    for (const QVariant &entry : entries) {
        const QVariantMap map = entry.toMap();
        const QString id = idOf(map);
        if (id.isEmpty() || seen.contains(id) || map.value(QStringLiteral("live")).toBool())
            continue;
        seen.insert(id);
        found.append(map);
    }
    return found;
}

void VideoModel::assign(const QVariantList &entries) {
    const QVector<QVariantMap> next = distinct(entries);
    QSet<QString> wanted;
    for (const QVariantMap &entry : next)
        wanted.insert(idOf(entry));

    // The videos that are gone, last first so the rows above keep their
    // numbers, and neighbours together in one removal.
    for (int row = m_rows.size() - 1; row >= 0; --row) {
        if (wanted.contains(idOf(m_rows.at(row))))
            continue;
        int first = row;
        while (first > 0 && !wanted.contains(idOf(m_rows.at(first - 1))))
            --first;
        removeRange(first, row);
        row = first;
    }

    // The videos that stay, in the order they are to have. Those on a longest
    // run that already is in that order keep their places, and every other one
    // moves once, to just after the video before it, which is where it ends
    // up when the movers are taken in the new order.
    QHash<QString, int> rowOf;
    for (int row = 0; row < m_rows.size(); ++row)
        rowOf.insert(idOf(m_rows.at(row)), row);
    QStringList kept;
    QVector<int> rows;
    for (const QVariantMap &entry : next) {
        const auto found = rowOf.constFind(idOf(entry));
        if (found == rowOf.constEnd())
            continue;
        kept << found.key();
        rows << found.value();
    }
    const QVector<bool> stays = longestIncreasing(rows);
    QStringList current = ids();
    for (int k = 0; k < kept.size(); ++k) {
        if (stays.at(k))
            continue;
        const int from = current.indexOf(kept.at(k));
        int to = 0;
        if (k > 0) {
            const int after = current.indexOf(kept.at(k - 1));
            // Taking the row out shifts everything below it up by one.
            to = from > after ? after + 1 : after;
        }
        if (to == from)
            continue;
        move(from, to);
        current.move(from, to);
    }

    // The new videos, each run of them at the place it will have; every row
    // above is final by then.
    for (int at = 0; at < next.size();) {
        if (rowOf.contains(idOf(next.at(at)))) {
            ++at;
            continue;
        }
        int end = at;
        while (end < next.size() && !rowOf.contains(idOf(next.at(end))))
            ++end;
        insertRange(at, next.mid(at, end - at));
        at = end;
    }

    for (int row = 0; row < next.size(); ++row)
        update(row, next.at(row));
}

void VideoModel::append(const QVariantList &entries) {
    QSet<QString> have;
    for (const QVariantMap &row : m_rows)
        have.insert(idOf(row));
    QVector<QVariantMap> fresh;
    for (const QVariantMap &entry : distinct(entries))
        if (!have.contains(idOf(entry)))
            fresh.append(entry);
    if (!fresh.isEmpty())
        insertRange(m_rows.size(), fresh);
}

void VideoModel::clear() {
    if (!m_rows.isEmpty())
        removeRange(0, m_rows.size() - 1);
}

void VideoModel::removeRange(int first, int last) {
    beginRemoveRows({}, first, last);
    m_rows.remove(first, last - first + 1);
    endRemoveRows();
    emit countChanged();
}

void VideoModel::move(int from, int to) {
    // beginMoveRows wants the row to put the moved one in front of, counted
    // as the rows are before the move.
    if (!beginMoveRows({}, from, from, {}, to > from ? to + 1 : to))
        return;
    m_rows.move(from, to);
    endMoveRows();
}

void VideoModel::insertRange(int at, const QVector<QVariantMap> &entries) {
    beginInsertRows({}, at, at + entries.size() - 1);
    for (int i = 0; i < entries.size(); ++i) {
        QVariantMap row = entries.at(i);
        if (m_batch > 0)
            row.insert(QStringLiteral("batch"), m_batch);
        m_rows.insert(at + i, row);
    }
    endInsertRows();
    emit countChanged();
}

void VideoModel::update(int row, QVariantMap entry) {
    const QVariantMap &old = m_rows.at(row);
    // The engine doesn't know the page a row came with.
    if (old.contains(QStringLiteral("batch")))
        entry.insert(QStringLiteral("batch"), old.value(QStringLiteral("batch")));
    if (old == entry)
        return;
    QVector<int> changed;
    for (int key = 0; key < roleKeys().size(); ++key) {
        const QString name = QString::fromLatin1(roleKeys().at(key));
        if (old.value(name) != entry.value(name))
            changed << Qt::UserRole + 1 + key;
    }
    m_rows[row] = entry;
    if (!changed.isEmpty())
        emit dataChanged(index(row), index(row), changed);
}
