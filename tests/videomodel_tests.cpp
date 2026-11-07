#include <QPersistentModelIndex>
#include <QQmlComponent>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQuickItem>
#include <QQuickWindow>
#include <QRandomGenerator>
#include <QSignalSpy>
#include <QtTest>

#include "suite.h"
#include "videomodel.h"

namespace {

QVariantMap video(const QString &id, const QString &title = {}) {
    return {{"id", id}, {"title", title.isEmpty() ? id : title}};
}

QVariantList videos(const QStringList &ids) {
    QVariantList list;
    for (const QString &id : ids)
        list << video(id);
    return list;
}

// What a view of the model makes of its signals: the rows it would show, and
// how many steps it took to get there. A view that was told everything
// correctly ends up with exactly the model's rows.
class Mirror : public QObject {
public:
    explicit Mirror(VideoModel *watched) : shown(watched->ids()) {
        connect(watched, &QAbstractItemModel::rowsInserted, this, [=, this](const QModelIndex &, int first, int last) {
            ++inserts;
            for (int row = first; row <= last; ++row)
                shown.insert(row, watched->ids().at(row));
            check(watched);
        });
        connect(watched, &QAbstractItemModel::rowsRemoved, this, [=, this](const QModelIndex &, int first, int last) {
            ++removals;
            shown.remove(first, last - first + 1);
            check(watched);
        });
        connect(watched, &QAbstractItemModel::rowsMoved, this,
                [=, this](const QModelIndex &, int start, int end, const QModelIndex &, int destination) {
                    ++moves;
                    if (start != end)
                        consistent = false;
                    // The destination is where the row was to go in front of, counted before the move.
                    shown.move(start, destination > start ? destination - 1 : destination);
                    check(watched);
                });
        connect(watched, &QAbstractItemModel::dataChanged, this,
                [=, this](const QModelIndex &top, const QModelIndex &, const QList<int> &roles) {
                    ++changes;
                    changedRows << top.row();
                    changedRoles = roles;
                });
        connect(watched, &QAbstractItemModel::modelReset, this, [this] { ++resets; });
        connect(watched, &QAbstractItemModel::layoutChanged, this, [this] { ++resets; });
    }

    QStringList shown;
    int inserts = 0, removals = 0, moves = 0, changes = 0, resets = 0;
    QList<int> changedRows;
    QList<int> changedRoles;
    // False once the view and the model have disagreed after any one signal.
    bool consistent = true;

private:
    void check(VideoModel *watched) {
        if (shown != watched->ids())
            consistent = false;
    }
};

// The length of a longest increasing run of `sequence`, the slow and obvious way.
int longestRun(const QList<int> &sequence) {
    QList<int> best(sequence.size(), 1);
    int longest = 0;
    for (int i = 0; i < sequence.size(); ++i) {
        for (int j = 0; j < i; ++j)
            if (sequence[j] < sequence[i])
                best[i] = qMax(best[i], best[j] + 1);
        longest = qMax(longest, best[i]);
    }
    return longest;
}

} // namespace

class VideoModelTests : public QObject {
    Q_OBJECT

private slots:
    void aNewVideoAtTheFrontLeavesTheOthersAlone() {
        VideoModel model;
        model.assign(videos({"a", "b", "c"}));
        Mirror mirror(&model);
        QPersistentModelIndex first = model.index(0);

        model.assign(videos({"x", "a", "b", "c"}));
        QCOMPARE(model.ids(), QStringList({"x", "a", "b", "c"}));
        QCOMPARE(mirror.inserts, 1);
        QCOMPARE(mirror.removals + mirror.moves + mirror.changes + mirror.resets, 0);
        QVERIFY(mirror.consistent);
        // The row that was first is still that video, one place further down.
        QCOMPARE(first.row(), 1);
        QCOMPARE(model.data(first, Qt::UserRole + 1).toString(), QString("a"));
    }

    void videosThatAreGoneAreRemovedTogether() {
        VideoModel model;
        model.assign(videos({"a", "b", "c", "d", "e"}));
        Mirror mirror(&model);
        model.assign(videos({"a", "e"}));
        QCOMPARE(model.ids(), QStringList({"a", "e"}));
        QCOMPARE(mirror.removals, 1);

        model.assign(videos({"a", "b", "c", "d", "e"}));
        Mirror again(&model);
        // Three runs: a, c and e.
        model.assign(videos({"b", "d"}));
        QCOMPARE(again.removals, 3);
        QVERIFY(again.consistent);
    }

    void aVideoThatChangedPlaceMovesOnce() {
        VideoModel model;
        model.assign(videos({"a", "b", "c", "d", "e"}));
        Mirror mirror(&model);
        // The one at the front goes to the back: one move, not four.
        model.assign(videos({"b", "c", "d", "e", "a"}));
        QCOMPARE(model.ids(), QStringList({"b", "c", "d", "e", "a"}));
        QCOMPARE(mirror.moves, 1);
        QCOMPARE(mirror.inserts + mirror.removals + mirror.resets, 0);
        QVERIFY(mirror.consistent);

        // In reverse only one of them can stay, so the other four move.
        Mirror reversed(&model);
        model.assign(videos({"a", "e", "d", "c", "b"}));
        QCOMPARE(model.ids(), QStringList({"a", "e", "d", "c", "b"}));
        QVERIFY(reversed.consistent);
        QCOMPARE(reversed.moves, 4);
    }

    void aChangedVideoSaysWhatChanged() {
        VideoModel model;
        model.assign({video("a"), video("b", "Old")});
        Mirror mirror(&model);

        QVariantMap changed = video("b", "New");
        changed.insert("views", 12);
        model.assign({video("a"), changed});
        QCOMPARE(mirror.changes, 1);
        QCOMPARE(mirror.changedRows, QList<int>({1}));
        QCOMPARE(QSet<int>(mirror.changedRoles.begin(), mirror.changedRoles.end()).size(), 2);
        QCOMPARE(mirror.inserts + mirror.removals + mirror.moves + mirror.resets, 0);
        QCOMPARE(model.get(1).value("title").toString(), QString("New"));

        // Nothing a delegate reads moved, so nobody is told; get() still has it.
        QVariantMap quiet = changed;
        quiet.insert("something-else", true);
        model.assign({video("a"), quiet});
        QCOMPARE(mirror.changes, 1);
        QVERIFY(model.get(1).contains("something-else"));

        // And a list that is the same changes nothing at all.
        model.assign({video("a"), quiet});
        QCOMPARE(mirror.changes, 1);
    }

    void repeatsAndEntriesWithoutAnIdAreSkipped() {
        VideoModel model;
        model.assign({video("a", "First"), QVariantMap{{"title", "No id"}}, video("a", "Second"), video("b")});
        QCOMPARE(model.ids(), QStringList({"a", "b"}));
        QCOMPARE(model.get(0).value("title").toString(), QString("First"));

        Mirror mirror(&model);
        model.append(videos({"b", "c", "c", "d"}));
        QCOMPARE(model.ids(), QStringList({"a", "b", "c", "d"}));
        QCOMPARE(mirror.inserts, 1);

        model.clear();
        QCOMPARE(model.count(), 0);
        QCOMPARE(mirror.removals, 1);
        QVERIFY(mirror.consistent);
        QCOMPARE(mirror.resets, 0);
        // Past the end there is nothing, and nothing goes wrong.
        QVERIFY(model.get(0).isEmpty());
        QVERIFY(model.get(-1).isEmpty());
    }

    void rolesAreNamedLikeTheEnginesKeys() {
        VideoModel model;
        const QList<QByteArray> names = model.roleNames().values();
        for (const char *key : {"id", "title", "channel", "channel_id", "published", "date", "estimated", "duration", "views", "short"})
            QVERIFY2(names.contains(key), key);
        QVariantMap entry = video("a", "Title");
        entry.insert("channel_id", "UC1");
        model.assign({entry});
        const QHash<int, QByteArray> roles = model.roleNames();
        for (auto it = roles.begin(); it != roles.end(); ++it)
            if (it.value() == "channel_id")
                QCOMPARE(model.data(model.index(0), it.key()).toString(), QString("UC1"));
    }

    void anyUpdateIsAppliedExactlyAndInTheFewestMoves() {
        // Random lists, and every one is turned into every other: the view
        // must end up with the new rows, survivors must be the same rows
        // (persistent indexes follow them), and no row may move that could
        // have stayed. The seed is fixed so a failure can be repeated.
        QRandomGenerator rng(20260607);
        QStringList universe;
        for (int i = 0; i < 16; ++i)
            universe << QString("v%1").arg(i, 2, 10, QChar('0'));
        auto sample = [&] {
            QStringList pool = universe, picked;
            const int wanted = rng.bounded(13);
            while (picked.size() < wanted)
                picked << pool.takeAt(rng.bounded(pool.size()));
            return picked;
        };

        for (int round = 0; round < 600; ++round) {
            const QStringList before = sample(), after = sample();
            const QString what = QString("round %1: %2 -> %3").arg(round).arg(before.join(' '), after.join(' '));
            VideoModel model;
            model.assign(videos(before));
            QList<QPersistentModelIndex> held;
            for (int row = 0; row < before.size(); ++row)
                held << model.index(row);
            Mirror mirror(&model);

            QVariantList next;
            for (const QString &id : after)
                next << video(id, rng.bounded(2) ? id : id + "!");
            model.assign(next);

            QVERIFY2(model.ids() == after, qPrintable(what));
            QVERIFY2(mirror.shown == after && mirror.consistent, qPrintable(what));
            QVERIFY2(mirror.resets == 0, qPrintable(what));
            QList<int> kept;
            for (const QString &id : after)
                if (before.contains(id))
                    kept << before.indexOf(id);
            QVERIFY2(mirror.moves == kept.size() - longestRun(kept), qPrintable(what));
            for (int i = 0; i < before.size(); ++i) {
                if (after.contains(before[i]))
                    QVERIFY2(held[i].isValid() && held[i].row() == after.indexOf(before[i]), qPrintable(what));
                else
                    QVERIFY2(!held[i].isValid(), qPrintable(what));
            }
            for (int row = 0; row < after.size(); ++row)
                QVERIFY2(model.get(row) == next[row].toMap(), qPrintable(what));
        }
    }

    void aGridKeepsItsCardsAndItsSelectionThroughAnUpdate() {
        // What the window relies on: a background refresh must not rebuild
        // the cards on screen, and the selected video stays selected even
        // though new videos arrive above it.
        VideoModel model;
        model.assign(videos({"a", "b", "c", "d"}));
        QQmlEngine engine;
        engine.rootContext()->setContextProperty("videos", &model);
        QQmlComponent component(&engine);
        component.setData(R"(
            import QtQuick
            GridView {
                id: grid
                property int built: 0
                property int dropped: 0
                width: 400; height: 400
                cellWidth: 100; cellHeight: 100
                model: videos
                delegate: Item {
                    required property var model
                    property string videoId: model.id
                    width: 100; height: 100
                    Component.onCompleted: grid.built++
                    Component.onDestruction: grid.dropped++
                }
            }
        )", QUrl());
        QScopedPointer<QObject> object(component.create());
        QVERIFY2(object, qPrintable(component.errorString()));
        auto *grid = qobject_cast<QQuickItem *>(object.data());
        QQuickWindow window;
        window.resize(400, 400);
        grid->setParentItem(window.contentItem());
        window.show();
        QVERIFY(QTest::qWaitForWindowExposed(&window));
        QTRY_COMPARE(grid->property("built").toInt(), 4);

        grid->setProperty("currentIndex", 2);
        auto current = [&] { return grid->property("currentItem").value<QQuickItem *>(); };
        QCOMPARE(current()->property("videoId").toString(), QString("c"));

        // One new video above, one gone below: what a refresh does.
        model.assign(videos({"x", "a", "b", "c"}));
        QTRY_COMPARE(grid->property("built").toInt(), 5);
        QCOMPARE(grid->property("dropped").toInt(), 1);
        QCOMPARE(grid->property("currentIndex").toInt(), 3);
        QCOMPARE(current()->property("videoId").toString(), QString("c"));

        // And one whose title changed is the same card, not a new one.
        QVariantMap renamed = video("b", "Renamed");
        model.assign({video("x"), video("a"), renamed, video("c")});
        QCOMPARE(grid->property("built").toInt(), 5);
        QCOMPARE(grid->property("dropped").toInt(), 1);
    }
};

QVEIL_SUITE(VideoModelTests)
#include "videomodel_tests.moc"
