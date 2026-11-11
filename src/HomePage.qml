import QtQuick
import Qveil

// Home. It opens on the shelves (see ShelvesHome); a shelf's heading opens
// its feed in full: the grid, with a tab for the latest from the channels
// you follow and one for each of your lenses.
FocusScope {
    id: page

    // Which feed is showing: "latest", or a lens's id.
    property string feed: "latest"
    // Whether the shelves are showing, and not one feed in full.
    property bool overview: true
    property string thumbs: "frosted"
    property bool shorts: false
    // How far each video was watched: { id: fraction }.
    property var progress: ({})
    // Where focus belongs when the page is shown.
    readonly property Item content: overview ? shelves : grid
    readonly property alias grid: grid
    // What the grid shows, as something with count and get.
    readonly property var model: grid.list
    // How each feed and shelf is sorted and filtered, by its key, and what
    // is known to have been watched.
    property var arrangements: ({})
    property var watched: ({})
    signal arranged(string key, var setting)
    readonly property bool onLens: feed !== "latest"
    readonly property bool noChannels: backend.feedInfo.channels === 0 && backend.subs.length === 0
    readonly property bool loading: onLens ? backend.busy.indexOf("lens") >= 0
        : (backend.busy.indexOf("feed") >= 0 || backend.busy.indexOf("feed-cache") >= 0 || !backend.ready)
    // What is loading, or what is there.
    readonly property string subheading: {
        if (onLens) {
            if (backend.lensTotal > 0 && backend.busy.indexOf("lens") >= 0)
                return qsTr("Refreshing %1 of %2").arg(backend.lensDone).arg(backend.lensTotal)
            return ""
        }
        // While it runs, how far the refresh is; it says more than the
        // channel count, which it would only repeat.
        if (backend.refreshTotal > 0 && backend.busy.indexOf("feed") >= 0)
            return qsTr("Refreshing %1 of %2").arg(backend.refreshDone).arg(backend.refreshTotal)
        const info = backend.feedInfo
        if (!info.channels)
            return ""
        let s = qsTr("Latest from %n channels", "", info.channels)
        if (info.errors && info.errors.length > 0)
            s += "  ·  " + qsTr("%n couldn't be fetched", "", info.errors.length)
        return s
    }

    // The same in few words, for the shelf, whose heading already says what it is.
    readonly property string brief: {
        if (backend.refreshTotal > 0 && backend.busy.indexOf("feed") >= 0)
            return subheading
        const info = backend.feedInfo
        if (!info.channels)
            return ""
        let s = qsTr("%n channels", "", info.channels)
        if (info.errors && info.errors.length > 0)
            s += "  ·  " + qsTr("%n couldn't be fetched", "", info.errors.length)
        return s
    }

    signal activated(var video, int index)
    // A video on a shelf: `list` is what comes after it.
    signal shelfActivated(var video, var list, int index)
    signal channelRequested(string id)
    signal resumed(var video)
    signal shortsToggled(bool shown)
    signal refreshRequested()
    signal newLensRequested()
    signal addChannelRequested()

    // One feed in full: "latest", or a lens's id.
    function showAll(which) {
        feed = which
        overview = false
        grid.forceActiveFocus()
    }
    function showShelves() {
        overview = true
        shelves.forceActiveFocus()
    }
    // The tab before or after, in the full feed: Latest, then each lens.
    function tabStep(by) {
        if (overview)
            return
        const ids = ["latest"].concat(backend.lenses.map(l => l.id))
        const at = Math.max(0, ids.indexOf(feed))
        feed = ids[Math.max(0, Math.min(ids.length - 1, at + by))]
    }
    function openSort() { overview ? shelves.openSort() : feedSort.open() }
    function openCurrent() { if (overview) shelves.openCurrent() }
    // Plays what the keyboard is on, on the shelves.
    function pickCurrent() { shelves.pickCurrent() }

    // A lens that was removed is not a feed any more.
    Connections {
        target: backend
        function onLensesChanged() {
            if (page.onLens && !backend.lenses.some(l => l.id === page.feed))
                page.feed = "latest"
        }
    }
    // (`onLens` may not have caught up with `feed` yet when this runs.)
    onFeedChanged: if (feed !== "latest") backend.openLens(feed, 1)
    // There is one open lens, and the lenses page may have opened another
    // meanwhile: coming back, the one this page's tab names is opened again
    // (from what the engine has; that is no reason to search).
    readonly property bool showing: visible && !overview
    onShowingChanged: if (showing && feed !== "latest" && backend.lensInfo.id !== feed) backend.openLens(feed, 0)

    ShelvesHome {
        id: shelves
        anchors.fill: parent
        visible: page.overview
        focus: page.overview
        thumbs: page.thumbs
        progress: page.progress
        status: page.brief
        arrangements: page.arrangements
        watched: page.watched
        onArranged: (key, setting) => page.arranged(key, setting)
        loading: page.loading
        noChannels: page.noChannels
        onActivated: (video, list, index) => page.shelfActivated(video, list, index)
        onResumed: video => page.resumed(video)
        onAllRequested: which => page.showAll(which)
        onChannelRequested: id => page.channelRequested(id)
        onRefreshRequested: page.refreshRequested()
        onAddChannelRequested: page.addChannelRequested()
        onNewLensRequested: page.newLensRequested()
    }

    Column {
        id: top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        spacing: Style.u * 4
        visible: !page.overview

        Column {
            width: parent.width
            spacing: Style.u
            FeedTabs {
                id: tabs
                objectName: "feed-tabs"
                width: parent.width
                lenses: backend.lenses
                current: page.feed
                status: page.subheading
                onBack: page.showShelves()
                onPicked: which => page.feed = which
                onNewLens: page.newLensRequested()

                SortMenu {
                    id: feedSort
                    objectName: "feed-sort"
                    setting: page.arrangements[page.feed] || ({})
                    sourceLabel: page.onLens ? qsTr("Best match") : qsTr("Newest")
                    onChanged: setting => page.arranged(page.feed, setting)
                }
                Act {
                    visible: !page.onLens && !page.noChannels
                    text: page.shorts ? qsTr("Shorts: on") : qsTr("Shorts: off")
                    quiet: true
                    tip: qsTr("Show or hide Shorts in the feed")
                    onClicked: page.shortsToggled(!page.shorts)
                }
                Act {
                    visible: page.onLens || !page.noChannels
                    text: qsTr("Refresh")
                    glyph: "qrc:/icons/refresh.svg"
                    enabled: !page.loading
                    onClicked: page.refreshRequested()
                }
            }
            Rule { width: parent.width }
        }
    }

    VideoGrid {
        id: grid
        objectName: "home-grid"
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: top.bottom
        anchors.bottom: parent.bottom
        anchors.topMargin: Style.u * 2
        visible: !page.overview
        focus: !page.overview
        source: page.onLens ? backend.lensVideos : backend.feed
        arrange: page.arrangements[page.feed] || ({})
        watched: page.watched
        thumbs: page.thumbs
        progress: page.progress
        estimateNote: page.onLens ? qsTr("Approximate: YouTube's search only says how long ago")
                                  : qsTr("Approximate: YouTube's feed for this channel was unavailable")
        onActivated: (video, index) => page.activated(video, index)
        onChannelRequested: id => page.channelRequested(id)
    }

    EmptyState {
        anchors.fill: grid
        visible: !page.overview && !page.loading && grid.count === 0
        text: page.onLens ? qsTr("Nothing fetched for this lens yet. Refresh runs its searches.")
            : page.noChannels ? qsTr("Add a channel, or drop a Google Takeout subscriptions.csv anywhere on this window.")
            : qsTr("Nothing here yet. Press R to refresh.")
        Act {
            visible: !page.onLens && page.noChannels
            text: qsTr("Add a channel")
            filled: true
            onClicked: page.addChannelRequested()
        }
    }
}
