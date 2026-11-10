import QtCore
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import Qveil

ApplicationWindow {
    id: win
    width: 1280
    height: 820
    minimumWidth: 760
    minimumHeight: 500
    visible: true
    title: player.active && player.info.title ? player.info.title + " \u00b7 qveil" : "qveil"
    color: Style.bg

    // The pages that are not redrawn yet still use Material controls.
    Material.theme: Material.Dark
    Material.accent: theme.accent
    Material.background: Style.bg
    Material.roundedScale: theme.radius

    // The place that is showing: home, lenses, channels, channel, history,
    // search or settings.
    property string page: "home"
    property string returnPage: "home"
    // Whether the first engine start has loaded the window yet.
    property bool started: false
    readonly property bool fullscreen: visibility === Window.FullScreen
    // Whether a video has the window.
    readonly property bool watching: player.active
    // Before the first subscription there is nothing to refresh or filter. Both
    // counts must say so, so a loading window doesn't flash the controls away.
    readonly property bool noSubs: backend.feedInfo.channels === 0 && backend.subs.length === 0
    // Whether keys are going into a text field. Every Qt Quick text input
    // has a cursor position and nothing else that takes focus does. While
    // this is true, no letter, digit or bracket is a shortcut.
    readonly property bool typing: !!activeFocusItem && typeof activeFocusItem.cursorPosition === "number"
    readonly property var pages: ({ home: feedPage, search: searchPage, channel: channelPage,
                                    channels: subsPage, history: historyPage,
                                    lenses: lensesPage, settings: settingsPage })
    // The rail marks where you are; a channel's page belongs to where you came from.
    readonly property string place: page === "channel" || page === "search" ? returnPage : page

    Settings {
        id: prefs
        // "frosted" (veiled until looked at) or "clear".
        property string thumbnails: "frosted"
        property bool showShorts: false
        property real volume: 80
        // Before 0.4 this was a switch, called the veil.
        Component.onCompleted: {
            const old = value("veilThumbnails")
            if (old === false || old === "false") {
                thumbnails = "clear"
                setValue("veilThumbnails", true)
            }
        }
    }
    // How thumbnails are shown: "frosted" or "clear".
    property alias thumbs: prefs.thumbnails
    // How far each unfinished video was watched: { id: fraction }.
    readonly property var watched: {
        const map = {}
        for (const kept of backend.resume)
            map[kept.id] = kept.dur > 0 ? kept.pos / kept.dur : 0
        return map
    }
    // The list the video that is playing was opened from, and where in it:
    // what comes next is the rest of that list.
    property var playList: null
    property int playIndex: -1

    function go(p) {
        if (!pages[p])
            return
        if (player.active)
            player.close()
        if (p !== "channel" && p !== "search")
            returnPage = p
        // What was typed in the search field belongs to the search page only.
        if (p !== "search") {
            search.text = ""
            backend.read("")
        }
        page = p
        if (p === "channels")
            backend.loadSubs()
        if (p === "history")
            backend.loadHistory()
        if (p === "lenses")
            backend.loadLenses()
        Qt.callLater(() => win.focusPage(p))
    }

    // Focus goes to what the page is about (its grid or list), never to
    // whatever was clicked last: a focus scope would hand it back to a text
    // field or a button the page had before.
    function focusPage(p) {
        const view = pages[p]
        const target = view.content ? view.content : view
        target.forceActiveFocus()
    }

    function openChannel(id) {
        if (!id)
            return
        if (player.active)
            player.close()
        if (page !== "channel" && page !== "search")
            returnPage = page
        search.text = ""
        backend.read("")
        page = "channel"
        backend.openChannel(id)
        Qt.callLater(() => win.focusPage("channel"))
    }

    // What the Refresh button and R do. Like startup, this fetches only the
    // channels that have gone stale: pressing it again must not repeat the
    // burst of requests for exactly your channels.
    function refresh() {
        if (page === "channel")
            backend.openChannel(backend.channel.id)
        else if (page === "search")
            backend.search(backend.query)
        else if (page === "home" && feedPage.onLens)
            backend.openLens(feedPage.feed, 2)
        else if (page === "home")
            backend.refreshFeed(true, prefs.showShorts)
    }

    // The first run's way in: Channels, with the add field ready to type in.
    function addChannel() {
        go("channels")
        // go() puts focus on the list on the next turn; the field comes after.
        Qt.callLater(() => subsPage.focusAdd())
    }

    function playCurrent() {
        const p = pages[page]
        if (p && p.grid && p.grid.currentIndex >= 0 && p.model.count > 0)
            win.play(p.model.get(p.grid.currentIndex), p.model, p.grid.currentIndex)
    }

    // The player opens at once, with what the card knows, while the engine
    // looks the video up. `list` and `index` say where it was opened from.
    function play(video, list, index) {
        playList = list || null
        playIndex = list ? index : -1
        player.prepare(video)
        backend.play(video.id)
    }

    // Over a video the route popup hangs from the rail, which is hidden
    // while the video has the window: bring the controls back first.
    function openRoute() {
        if (player.active)
            player.reveal()
        route.open()
    }

    function toggleRoute() {
        if (route.opened)
            route.close()
        else
            win.openRoute()
    }

    function toggleFullscreen() {
        if (fullscreen)
            showNormal()
        else
            showFullScreen()
    }

    function goBack() {
        if (help.opened)
            help.close()
        else if (route.opened)
            route.close()
        else if (fullscreen)
            showNormal()
        else if (player.active)
            player.close()
        else if (typing)
            focusPage(page)
        else if (page === "channel" || page === "search")
            go(returnPage)
    }

    Connections {
        target: backend
        // Only the first start loads things here: if the engine has to be
        // restarted, Backend reloads what the window shows by itself, and a
        // full refresh would be unasked for.
        function onReadyChanged() {
            if (!backend.ready || win.started)
                return
            win.started = true
            backend.refreshFeed(false, prefs.showShorts)
            // Only the channels that have gone stale: opening the window must
            // not refetch what was fetched minutes ago.
            backend.refreshFeed(true, prefs.showShorts)
            backend.loadSubs()
            backend.loadResume()
            backend.loadLenses()
        }
        // The engine ends the streams of the old route, so the player has
        // nothing left to play, and must not stay up as if it had.
        function onRouteChanged() {
            if (player.active)
                player.close()
        }
        function onPlayReady(info) { player.open(info) }
        function onNotice(text) { toast.show(text) }
    }

    // Escape and the keys that lead to a text field work while typing; every
    // other key is only a shortcut when it isn't going into one.
    Shortcut { sequences: ["/", "Ctrl+L"]; context: Qt.ApplicationShortcut; enabled: !win.typing || sequence === "Ctrl+L"; onActivated: search.edit() }
    Shortcut { sequence: "Esc"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.goBack() }
    Shortcut { sequence: "?"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: help.opened ? help.close() : help.open() }
    Shortcut { sequence: "Q"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: Qt.quit() }
    Shortcut { sequence: "Space"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: player.active ? player.mpv.togglePause() : win.playCurrent() }
    Shortcut { sequence: "1"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.go("home") }
    Shortcut { sequence: "2"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.go("lenses") }
    Shortcut { sequence: "3"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.go("channels") }
    Shortcut { sequence: "4"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.go("history") }
    Shortcut { sequence: "5"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.go("settings") }
    Shortcut { sequence: "R"; context: Qt.ApplicationShortcut; enabled: !win.typing && !player.active; onActivated: win.refresh() }
    Shortcut { sequence: "P"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.toggleRoute() }
    Shortcut { sequence: "F"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: win.toggleFullscreen() }
    Shortcut { sequence: "M"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: player.mpv.muted = !player.mpv.muted }
    Shortcut { sequence: "Left"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.seek(-5, true); player.reveal() } }
    Shortcut { sequence: "Right"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.seek(5, true); player.reveal() } }
    Shortcut { sequence: "J"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.seek(-10, true); player.reveal() } }
    Shortcut { sequence: "L"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.seek(10, true); player.reveal() } }
    Shortcut { sequence: "Up"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.volume = player.mpv.volume + 5; player.reveal() } }
    Shortcut { sequence: "Down"; context: Qt.ApplicationShortcut; enabled: !win.typing && player.active; onActivated: { player.mpv.volume = player.mpv.volume - 5; player.reveal() } }

    // Mirrors for right-to-left languages. A top-level window ignores the
    // application's layout direction.
    Item {
        anchors.fill: parent
        LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
        LayoutMirroring.childrenInherit: true

        // The player owns the whole window while a video is open, so it is
        // the bottom layer: the rail and the pages above it are hidden then.
        PlayerView {
            id: player
            anchors.fill: parent
            topInset: errorBar.visible ? errorBar.height : 0
            // The controls stay while something outside the player is in use.
            hold: win.typing || route.opened || help.opened
            savedVolume: prefs.volume
            onSavedVolumeChanged: prefs.volume = savedVolume
            onClosed: {
                if (win.fullscreen)
                    win.showNormal()
                Qt.callLater(() => win.focusPage(win.page))
            }
            onChannelRequested: id => win.openChannel(id)
            onFullscreenToggled: win.toggleFullscreen()
        }

        Rail {
            id: rail
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            visible: !player.active
            current: win.place
            route: backend.config.route || "direct"
            routeSummary: backend.privacy.summary
            onPicked: place => win.go(place)
            onRouteClicked: win.toggleRoute()
        }

        // The route popup opens beside the foot of the rail, where the
        // route is shown.
        Item {
            anchors.left: rail.right
            anchors.bottom: parent.bottom
            anchors.bottomMargin: Style.u * 3
            width: 1
            height: 1
            RoutePopup {
                id: route
                objectName: "routePopup"
                prefs: prefs
                x: Style.u * 2
                y: -height
            }
        }

        Item {
            id: content
            anchors.left: rail.visible ? rail.right : parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.bottom: parent.bottom

            // Errors stay until dismissed. Over a video they sit on its top edge.
            ErrorBar {
                id: errorBar
                objectName: "errorBar"
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                z: 2
                onRouteRequested: win.openRoute()
            }

            // The search field, and under it how what is typed was read.
            Column {
                id: top
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: errorBar.visible ? errorBar.bottom : parent.top
                anchors.leftMargin: Style.gutter
                anchors.rightMargin: Style.gutter
                anchors.topMargin: Style.u * 5
                spacing: Style.u * 2
                visible: !player.active

                FindField {
                    id: search
                    objectName: "search"
                    width: Math.min(520, parent.width)
                    onSubmitted: query => {
                        win.go("search")
                        search.text = query
                        backend.search(query)
                    }
                }
                ReadingTags {
                    objectName: "reading-tags"
                    width: parent.width
                    // Only for what is in the field now, and only when there is a rule to show.
                    visible: backend.reading.rules === true && backend.reading.text === search.text.trim()
                    tags: backend.reading.tags || []
                }
            }

            Item {
                id: stage
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: top.visible ? top.bottom : parent.top
                anchors.bottom: parent.bottom
                anchors.topMargin: top.visible ? Style.u * 2 : 0

                HomePage {
                    id: feedPage
                    objectName: "feedPage"
                    anchors.fill: parent
                    visible: win.page === "home" && !player.active
                    focus: visible
                    thumbs: win.thumbs
                    shorts: prefs.showShorts
                    progress: win.watched
                    onActivated: (video, index) => win.play(video, feedPage.model, index)
                    onChannelRequested: id => win.openChannel(id)
                    onResumed: video => win.play(video)
                    onShortsToggled: shown => {
                        prefs.showShorts = shown
                        backend.refreshFeed(false, shown)
                    }
                    onRefreshRequested: win.refresh()
                    onNewLensRequested: win.go("lenses")
                    onAddChannelRequested: win.addChannel()
                }

                VideoPage {
                    id: searchPage
                    objectName: "searchPage"
                    anchors.fill: parent
                    visible: win.page === "search" && !player.active
                    thumbs: win.thumbs
                    progress: win.watched
                    heading: backend.query ? qsTr("\u201c%1\u201d").arg(backend.query) : qsTr("Search")
                    subheading: backend.privacy.pages.search
                    model: backend.results
                    estimateNote: qsTr("Approximate: YouTube's search only says how long ago")
                    loading: backend.busy.indexOf("search") >= 0
                    emptyText: backend.query ? qsTr("No results.") : ""
                    onActivated: (video, index) => win.play(video, backend.results, index)
                    onChannelRequested: id => win.openChannel(id)
                }

                VideoPage {
                    id: channelPage
                    anchors.fill: parent
                    visible: win.page === "channel" && !player.active
                    thumbs: win.thumbs
                    progress: win.watched
                    heading: backend.channel.name || ""
                    subheading: backend.channelVideos.count
                                ? qsTr("%n recent videos", "", backend.channelVideos.count) : ""
                    model: backend.channelVideos
                    estimateNote: qsTr("Approximate: YouTube's channel page only says how long ago")
                    loading: backend.busy.indexOf("channel") >= 0
                    emptyText: qsTr("No videos.")
                    onActivated: (video, index) => win.play(video, backend.channelVideos, index)

                    // Subscribe is the call to action; Subscribed is the same
                    // word switched on, with a check.
                    Act {
                        readonly property bool isOn: !!backend.channel.subscribed
                        visible: !!backend.channel.name
                        text: isOn ? qsTr("Subscribed") : qsTr("Subscribe")
                        glyph: isOn ? "qrc:/icons/check.svg" : ""
                        filled: true
                        onClicked: isOn ? backend.unsubscribe(backend.channel.id)
                                        : backend.subscribe(backend.channel.id)
                    }
                }

                SubsPage {
                    id: subsPage
                    anchors.fill: parent
                    visible: win.page === "channels" && !player.active
                    onChannelRequested: id => win.openChannel(id)
                }

                HistoryPage {
                    id: historyPage
                    anchors.fill: parent
                    visible: win.page === "history" && !player.active
                    onPlayRequested: video => win.play(video)
                }

                // Lenses and settings are drawn by the tasks that add them.
                Item {
                    id: lensesPage
                    objectName: "page-lenses"
                    anchors.fill: parent
                    visible: win.page === "lenses" && !player.active
                }
                Item {
                    id: settingsPage
                    objectName: "page-settings"
                    anchors.fill: parent
                    visible: win.page === "settings" && !player.active
                }

                // A thin line along the top of the page while anything is
                // loading. It fills as a feed refresh gets through its
                // channels, and only then: anything else running at the same
                // time makes it as long as that is.
                BusyLine {
                    objectName: "progress"
                    readonly property var tasks: backend.busy.filter(t => ["play", "config", "read", "resume", "resume-note", "lenses"].indexOf(t) < 0)
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    indeterminate: !(backend.refreshTotal > 0 && tasks.length === 1 && tasks[0] === "feed")
                    value: backend.refreshTotal > 0 ? backend.refreshDone / backend.refreshTotal : 0
                    visible: !player.active && tasks.length > 0
                }
            }
        }

        Toast {
            id: toast
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: Style.gutter
            z: 5
        }

        // Dropping an export imports it.
        DropArea {
            id: drop
            anchors.fill: parent
            onDropped: d => {
                if (d.hasUrls)
                    backend.importFile(d.urls[0])
            }
            Rectangle {
                anchors.fill: parent
                visible: drop.containsDrag
                color: Style.scrim
                border.width: 2
                border.color: Style.accent
                Text {
                    anchors.centerIn: parent
                    text: qsTr("Drop to import subscriptions")
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.head
                }
            }
        }
    }

    HelpOverlay { id: help }
}
