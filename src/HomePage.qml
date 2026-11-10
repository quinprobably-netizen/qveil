import QtQuick
import Qveil

// Home: what was left unfinished, then the feed (the latest from the
// channels you follow, or one of your lenses).
FocusScope {
    id: page

    // Which feed is showing: "latest", or a lens's id.
    property string feed: "latest"
    property string thumbs: "frosted"
    property bool shorts: false
    // How far each video was watched: { id: fraction }.
    property var progress: ({})
    // Where focus belongs when the page is shown.
    readonly property Item content: grid
    readonly property alias grid: grid
    readonly property var model: grid.model
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

    signal activated(var video, int index)
    signal channelRequested(string id)
    signal resumed(var video)
    signal shortsToggled(bool shown)
    signal refreshRequested()
    signal newLensRequested()
    signal addChannelRequested()

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
    onVisibleChanged: if (visible && feed !== "latest" && backend.lensInfo.id !== feed) backend.openLens(feed, 0)

    Column {
        id: top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        spacing: Style.u * 4

        ResumeRow {
            objectName: "resume-row"
            width: parent.width
            entries: backend.resume
            days: backend.config.resumeDays || 0
            onPicked: video => page.resumed(video)
            onForgotten: id => backend.forgetResume(id)
        }
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
                onPicked: which => page.feed = which
                onNewLens: page.newLensRequested()

                Act {
                    visible: !page.onLens && !page.noChannels
                    text: page.shorts ? qsTr("Shorts on") : qsTr("Shorts off")
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
        focus: true
        model: page.onLens ? backend.lensVideos : backend.feed
        thumbs: page.thumbs
        progress: page.progress
        estimateNote: page.onLens ? qsTr("Approximate: YouTube's search only says how long ago")
                                  : qsTr("Approximate: YouTube's feed for this channel was unavailable")
        onActivated: (video, index) => page.activated(video, index)
        onChannelRequested: id => page.channelRequested(id)
    }

    EmptyState {
        anchors.fill: grid
        visible: !page.loading && grid.count === 0
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
