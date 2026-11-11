import QtQuick
import Qveil

// Home. It opens on the shelves (see ShelvesHome); the latest shelf's heading
// opens that feed in full: the grid. A lens's shelf leads to the Lenses page.
FocusScope {
    id: page

    // Whether the shelves are showing, and not the latest in full.
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
    readonly property bool noChannels: backend.feedInfo.channels === 0 && backend.subs.length === 0
    readonly property bool loading: backend.busy.indexOf("feed") >= 0 || backend.busy.indexOf("feed-cache") >= 0 || !backend.ready
    // What is loading, or what is there, in few words: the heading, over the
    // shelf and over the feed in full, already says what it is.
    readonly property string brief: {
        // While it runs, how far the refresh is.
        if (backend.refreshTotal > 0 && backend.busy.indexOf("feed") >= 0)
            return qsTr("Refreshing %1 of %2").arg(backend.refreshDone).arg(backend.refreshTotal)
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
    signal resumed(var video, var list, int index)
    signal restarted(var video, var list, int index)
    signal forgotten(var kept)
    signal shortsToggled(bool shown)
    signal refreshRequested()
    signal newLensRequested()
    signal addChannelRequested()
    // A lens's shelf is opened in full: the Lenses page has it.
    signal lensRequested(string id)

    // The latest from the channels, in full.
    function showAll() {
        overview = false
        grid.forceActiveFocus()
    }
    function showShelves() {
        overview = true
        shelves.forceActiveFocus()
    }
    function openSort() { overview ? shelves.openSort() : feedSort.open() }
    function openCurrent() { if (overview) shelves.openCurrent() }
    // Plays what the keyboard is on, on the shelves.
    function pickCurrent() { shelves.pickCurrent() }

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
        onResumed: (video, list, index) => page.resumed(video, list, index)
        onRestarted: (video, list, index) => page.restarted(video, list, index)
        onForgotten: kept => page.forgotten(kept)
        onAllRequested: which => which === "latest" ? page.showAll() : page.lensRequested(which)
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
            FeedHead {
                id: head
                objectName: "feed-head"
                width: parent.width
                title: qsTr("Latest from your channels")
                status: page.brief
                onBack: page.showShelves()

                SortMenu {
                    id: feedSort
                    objectName: "feed-sort"
                    setting: page.arrangements.latest || ({})
                    sourceLabel: qsTr("Newest")
                    onChanged: setting => page.arranged("latest", setting)
                }
                Act {
                    visible: !page.noChannels
                    text: page.shorts ? qsTr("Shorts: on") : qsTr("Shorts: off")
                    quiet: true
                    tip: qsTr("Show or hide Shorts in the feed")
                    onClicked: page.shortsToggled(!page.shorts)
                }
                Act {
                    visible: !page.noChannels
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
        source: backend.feed
        arrange: page.arrangements.latest || ({})
        watched: page.watched
        thumbs: page.thumbs
        progress: page.progress
        estimateNote: qsTr("Approximate: YouTube's feed for this channel was unavailable")
        onActivated: (video, index) => page.activated(video, index)
        onChannelRequested: id => page.channelRequested(id)
    }

    EmptyState {
        anchors.fill: grid
        visible: !page.overview && !page.loading && grid.count === 0
        text: page.noChannels ? qsTr("Add a channel, or drop a Google Takeout subscriptions.csv anywhere on this window.")
            : qsTr("Nothing here yet. Press R to refresh.")
        Act {
            visible: page.noChannels
            text: qsTr("Add a channel")
            filled: true
            onClicked: page.addChannelRequested()
        }
    }
}
