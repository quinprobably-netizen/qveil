import QtCore
import QtQuick
import QtQuick.Controls
import Qveil

ApplicationWindow {
    id: win
    width: 1280
    height: 820
    minimumWidth: 760
    minimumHeight: 500
    visible: true
    title: watch.active && watch.info.title ? watch.info.title + " \u00b7 qveil" : "qveil"
    color: Style.bg

    // The place that is showing: home, lenses, channels, channel, history,
    // search or settings.
    property string page: "home"
    property string returnPage: "home"
    // Whether the first engine start has loaded the window yet.
    property bool started: false
    readonly property bool fullscreen: visibility === Window.FullScreen
    // Whether a video has the window.
    readonly property bool watching: watch.active
    // Whether the video is in the small window of picture in picture.
    readonly property bool pip: pipWindow.surface.active
    readonly property alias pipWindow: pipWindow
    // The player, wherever it is shown: in the page, or in the small window.
    readonly property var player: pip ? pipWindow.surface : watch.surface
    // Whether anything is playing, in either.
    readonly property bool playing: watching || pip
    // Before the first subscription there is nothing to refresh or filter. Both
    // counts must say so, so a loading window doesn't flash the controls away.
    readonly property bool noSubs: backend.feedInfo.channels === 0 && backend.subs.length === 0
    // Whether keys are going into a text field. Every Qt Quick text input
    // has a cursor position and nothing else that takes focus does. While
    // this is true, no letter, digit or bracket is a shortcut.
    // A window wider than 1280 is drawn larger, everything in it by the one
    // factor, up to half again as large at 1920: the size follows the tile it
    // is in, with no step anywhere.
    readonly property real zoom: Math.max(1, Math.min(1.5, 1 + (width - 1280) / 1280))
    // What opens over the window is drawn as large as what is in it.
    function scaleOverlay() {
        Overlay.overlay.transformOrigin = Item.TopLeft
        Overlay.overlay.scale = zoom
    }
    onZoomChanged: scaleOverlay()
    Component.onCompleted: scaleOverlay()
    // Whether the arrow keys are the video's: while it has the page, or in
    // its small window. While it plays in the small window and this one is
    // browsed, they move through what is browsed, as they always do.
    // (A window counts as active while its parent window is, so that is
    // not the question; whether something in it has the keyboard is.)
    readonly property bool steering: ((watching && !watch.inSheet) || (pip && pipWindow.activeFocusItem !== null))
        && !route.opened && !help.opened && openMenu === null
    // Where a channel's page was opened from.
    property string channelFrom: "home"
    readonly property bool typing: !!activeFocusItem && typeof activeFocusItem.cursorPosition === "number"
    // A field the pointer just put the keyboard in, and when.
    property Item tappedField: null
    property real tappedAt: 0
    // When a popup has gone, the keyboard goes back to what is showing; or to
    // the field whose tap closed it (Qt takes the keyboard back to the page
    // as the popup goes, after the tap gave it to the field).
    function afterPopup() {
        if (tappedField && tappedField.visible && Date.now() - tappedAt < 1000) {
            tappedField.forceActiveFocus()
            return
        }
        if (typing)
            return
        if (watching)
            watch.forceActiveFocus()
        else
            focusPage(page)
    }
    // While the help or a menu is open, the keys are theirs (Esc and ? aside).
    readonly property bool quiet: help.opened || openMenu !== null
    function tabStep(by) {
        if (steering)
            player.chapterStep(by > 0)
        else if (watching && watch.inSheet)
            watch.sheet.turn(by)
        else if (!watching && page === "lenses")
            lensesPage.step(by)
        else if (!watching && page === "home")
            feedPage.tabStep(by)
    }
    readonly property var pages: ({ home: feedPage, search: searchPage, channel: channelPage,
                                    channels: subsPage, history: historyPage,
                                    lenses: lensesPage, settings: settingsPage })
    // The bar marks where you are; a channel's page belongs to where you came from.
    readonly property string place: page === "search" || (page === "channel" && channelFrom === "search") ? ""
        : page === "channel" ? returnPage : page

    Settings {
        id: prefs
        // "frosted" (veiled until looked at) or "clear".
        property string thumbnails: "clear"
        property bool showShorts: false
        property real volume: 80
        // Whether it has been said that positions are kept.
        property bool resumeTold: false
        // How each feed and shelf is sorted and filtered: { key: setting }, as JSON.
        property string arrangements: "{}"
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
    // How each feed and shelf is sorted and filtered, by its key (see Arrange.js).
    readonly property var arrangements: {
        try {
            return JSON.parse(prefs.arrangements) || {}
        } catch (e) {
            return {}
        }
    }
    function arrange(key, setting) {
        const next = Object.assign({}, arrangements)
        next[key] = setting
        prefs.arrangements = JSON.stringify(next)
    }
    // What qveil knows to have been watched: what has a kept position and,
    // while the history is kept, what is in it. { id: true }
    readonly property var watchedIds: {
        const ids = {}
        for (const kept of backend.resume)
            ids[kept.id] = true
        if (backend.config.history)
            for (const seen of backend.history)
                ids[seen.id] = true
        return ids
    }
    // Topic feeds are searched for when they are turned on and once when the
    // window starts; the engine decides which are due (once a day each).
    readonly property bool topicsOn: backend.ready && !!backend.config.topicFeeds
    onTopicsOnChanged: topicsOn ? backend.refreshTopics() : backend.dropTopics()
    // Hiding what was watched needs the history, where it is kept.
    readonly property bool historyKept: backend.ready && !!backend.config.history
    onHistoryKeptChanged: if (historyKept) backend.loadHistory()

    // The list the video that is playing was opened from, and where in it:
    // what comes next is the rest of that list.
    property var playList: null
    property int playIndex: -1
    // What comes next is the list as it was when the video was played: the
    // list itself may re-sort, filter or refill under it while it plays.
    VideoModel { id: playedFrom }

    // Leaving the watch page: a video that is playing goes on in the small
    // window, from where it is; one that is paused or over is put away
    // (where it was is kept).
    function leaveWatch() {
        const s = watch.surface
        if (s.active && !s.pending && !s.mpv.paused && !s.mpv.ended)
            togglePip()
        else
            watch.close()
    }
    function go(p) {
        if (!pages[p])
            return
        if (watch.active)
            leaveWatch()
        if (p !== "channel" && p !== "search")
            returnPage = p
        // What was typed in the search field belongs to the search page only.
        if (p !== "search") {
            search.text = ""
            backend.read("")
        }
        // Home is the shelves; a feed in full is a step into it.
        if (p === "home")
            feedPage.overview = true
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
        if (watch.active)
            leaveWatch()
        if (page !== "channel" && page !== "search")
            returnPage = page
        if (page !== "channel")
            channelFrom = page
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
        else if (page === "lenses")
            lensesPage.refresh()
        else if (page === "home" && feedPage.onLens && !feedPage.overview)
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
        if (page === "home" && feedPage.overview) {
            feedPage.pickCurrent()
            return
        }
        const p = pages[page]
        const list = p && p.grid ? p.grid.list : null
        if (list && p.grid.currentIndex >= 0 && list.count > 0)
            win.play(list.get(p.grid.currentIndex), list, p.grid.currentIndex)
    }

    // The player opens at once, with what the card knows, while the engine
    // looks the video up. `list` and `index` say where it was opened from.
    function play(video, list, index) {
        openingAs = null
        if (list && list !== playedFrom) {
            const rows = []
            for (let i = 0; i < list.count; i++)
                rows.push(list.get(i))
            playedFrom.assign(rows)
        }
        playList = list ? playedFrom : null
        playIndex = list ? index : -1
        // While a video is in the small window, the next one plays there
        // too, and the main window stays where it is.
        if (pip) {
            pipWindow.surface.prepare(video)
        } else {
            watch.prepare(video)
            watch.forceActiveFocus()
        }
        backend.play(video.id)
    }

    // How the video that is being opened is to begin, when that is not as the
    // engine's answer says: { id, at, forget }. `at` is the second it starts
    // at, and `forget` says that the place the engine kept for it is forgotten
    // once the video has begun, not before (a video that doesn't open keeps it).
    property var openingAs: null
    function startOver(video, list, index) {
        play(video, list, index)
        openingAs = { id: video.id, at: 0, forget: true }
    }

    // The route changed under the video that is showing, and the engine has
    // ended the streams it had over the old one. It is looked up again and
    // opened by the new route, at the place it had got to, where it was (in
    // the small window too). One that had played to its end has nothing to
    // carry on from, and is put away.
    function reopen() {
        const from = player
        if (!from.active)
            return
        const info = from.info
        const over = from.length > 0 && from.lastPosition >= from.length - 2
        if (!from.pending && over) {
            if (pip)
                pipWindow.end()
            else
                watch.close()
            return
        }
        // (One that was still being looked up begins as it was going to.)
        const as = from.pending ? openingAs : { id: info.id, at: from.mpv.started ? from.lastPosition : from.startedAt, forget: false }
        play(info, playList, playIndex)
        openingAs = as
        // The popup the route was chosen in keeps the keyboard.
        if (route.opened)
            route.forceActiveFocus()
    }

    // A place that was forgotten (what the engine had kept for the video) can
    // be had back while the line that says so is up: it is noted again as it
    // was. The forgetting itself is done at once, so quitting leaves it done.
    property var forgot: null
    function forgotPlace(kept) {
        forgot = kept
        toast.offer(qsTr("Forgot where you stopped"), qsTr("Undo"))
    }
    function restorePlace() {
        if (forgot)
            backend.noteResume(forgot, forgot.pos, forgot.dur)
        forgot = null
    }

    // Picture in picture: the video leaves the page for a small window of
    // its own, or comes back from it. The player goes with it, so it plays
    // on from where it is with nothing opened again: over Tor, opening it
    // anew takes seconds. Should the player not come along, the video is
    // opened again in the other window, from where it was.
    function togglePip() {
        const from = pip ? pipWindow.surface : watch.surface
        if (!from.active || from.pending)
            return
        const info = from.info
        // Before the first frame, where it was going to start is where it is.
        const at = from.mpv.started ? from.mpv.position : from.startedAt
        if (pip) {
            if (!watch.adopt(pipWindow.surface)) {
                pipWindow.end()
                watch.prepare(info)
                watch.surface.openAt(info, at)
            }
            watch.forceActiveFocus()
            win.requestActivate()
        } else {
            // A lookup of comments that is still running is for nobody now.
            backend.closeComments()
            if (!pipWindow.adopt(watch.surface)) {
                watch.close()
                pipWindow.take(info, at)
            }
        }
    }

    // Quits. A video that is playing says where it is first (a quit would lose
    // up to the last fifteen seconds of it), and the window goes once the
    // engine has answered, or after a short while if it doesn't.
    function quit() {
        player.report()
        if (backend.busy.indexOf("resume-note") < 0)
            Qt.quit()
        else
            quitting.restart()
    }
    Timer {
        id: quitting
        interval: 1500
        onTriggered: Qt.quit()
    }

    // Puts `text` on the clipboard. (Only a text item can.)
    function copy(text) {
        clipboard.text = text
        clipboard.selectAll()
        clipboard.copy()
        clipboard.text = ""
    }

    function openRoute() {
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

    // What the stage shows: a page, or the video. Whatever comes in arrives briefly.
    readonly property string showing: watching ? "watch" : page
    onShowingChanged: if (Style.move > 0) arrive.restart()
    ParallelAnimation {
        id: arrive
        NumberAnimation { target: stage; property: "opacity"; from: 0; to: 1; duration: Style.move; easing.type: Easing.OutCubic }
        NumberAnimation { target: settle; property: "y"; from: 8; to: 0; duration: Style.move; easing.type: Easing.OutCubic }
    }

    // A menu that is open (see SortMenu): Escape closes it first.
    property var openMenu: null

    function goBack() {
        if (openMenu) {
            openMenu.close()
            return
        }
        // In the small window (when the keyboard is in it), Escape brings the
        // video back; in this one it goes back, as it always does.
        if (pip && pipWindow.activeFocusItem !== null)
            togglePip()
        else if (help.opened)
            help.close()
        else if (route.opened)
            route.close()
        else if (fullscreen)
            showNormal()
        else if (watch.active && watch.inSheet)
            watch.leaveSheet()
        else if (watch.active && watch.theatre)
            watch.theatre = false
        else if (watch.active)
            leaveWatch()
        else if (typing)
            focusPage(page)
        else if (page === "settings" && settingsPage.inRows)
            settingsPage.leaveRows()
        else if (page === "home" && !feedPage.overview)
            feedPage.showShelves()
        else if (page === "channel" && channelFrom === "search") {
            // Back to the results it was opened from, which are still
            // there: nothing is searched for again.
            page = "search"
            search.text = backend.query
            backend.read(backend.query)
            Qt.callLater(() => win.focusPage("search"))
        } else if (page === "channel" || page === "search")
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
        // The engine ends the streams of the old route, so what was playing
        // is opened again by the new one.
        function onRouteChanged() { win.reopen() }
        function onBusyChanged() {
            if (quitting.running && backend.busy.indexOf("resume-note") < 0) {
                quitting.stop()
                Qt.quit()
            }
        }
        function onPlayReady(info) {
            const surface = win.pip ? pipWindow.surface : watch.surface
            const as = win.openingAs && win.openingAs.id === info.id ? win.openingAs : null
            win.openingAs = null
            if (as) {
                surface.forgetWhenStarted = as.forget
                surface.openAt(info, as.at)
            } else {
                surface.open(info)
            }
        }
        // A lens that was just added is shown, wherever it was added from.
        function onLensAdded(id) {
            if (win.page !== "lenses")
                win.go("lenses")
        }
        function onNotice(text) { toast.show(text) }
        // The first time a position is kept, the window says so, once.
        function onPositionKept() {
            if (!prefs.resumeTold) {
                prefs.resumeTold = true
                resumeNotice.shown = true
            }
        }
    }

    // Escape and the keys that lead to a text field work while typing; every
    // other key is only a shortcut when it isn't going into one.
    Shortcut { sequences: ["/", "Ctrl+L"]; context: Qt.ApplicationShortcut; enabled: !win.typing || sequence === "Ctrl+L"; onActivated: search.edit() }
    Shortcut { sequence: "Esc"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: win.goBack() }
    Shortcut { sequence: "?"; context: Qt.ApplicationShortcut; enabled: !win.typing; onActivated: help.opened ? help.close() : help.open() }
    Shortcut { sequence: "Q"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.quit() }
    Shortcut { sequence: "Space"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.playing ? player.mpv.togglePause() : win.playCurrent() }
    Shortcut { sequence: "1"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.go("home") }
    Shortcut { sequence: "2"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.go("lenses") }
    Shortcut { sequence: "3"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.go("channels") }
    Shortcut { sequence: "4"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.go("history") }
    Shortcut { sequence: "5"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.go("settings") }
    Shortcut { sequence: "R"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && !win.watching; onActivated: win.refresh() }
    Shortcut { sequence: "P"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.toggleRoute() }
    Shortcut { sequence: "F"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.watching; onActivated: win.toggleFullscreen() }
    Shortcut { sequence: "M"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.playing; onActivated: player.mpv.muted = !player.mpv.muted }
    Shortcut { sequence: "Left"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: player.nudge(-5) }
    Shortcut { sequence: "Right"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: player.nudge(5) }
    Shortcut { sequence: "J"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: player.nudge(-10) }
    Shortcut { sequence: "L"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: player.nudge(10) }
    Shortcut { sequence: "Up"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: { player.mpv.volume = player.mpv.volume + 5; player.reveal() } }
    Shortcut { sequence: "I"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.playing; onActivated: win.togglePip() }
    Shortcut { sequence: "T"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.watching; onActivated: watch.theatre = !watch.theatre }
    Shortcut { sequence: "C"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.watching; onActivated: watch.enterSheet("comments") }
    Shortcut { sequence: "S"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && !win.watching; onActivated: { const p = win.pages[win.page]; if (p && p.openSort) p.openSort() } }
    Shortcut { sequence: "O"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && !win.watching && win.page === "home"; onActivated: feedPage.openCurrent() }
    Shortcut { sequence: "N"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.watching; onActivated: watch.playNext() }
    // [ and ]: the chapter before or after while the video has the keyboard;
    // otherwise the tab before or after (lenses, the full feed, the sheet).
    Shortcut { sequence: "["; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.tabStep(-1) }
    Shortcut { sequence: "]"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet; onActivated: win.tabStep(1) }
    // K pauses and plays, as it does on YouTube, while the video has the keyboard.
    Shortcut { sequence: "K"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: player.mpv.togglePause() }
    Shortcut { sequence: "<"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.playing; onActivated: player.speedStep(false) }
    Shortcut { sequence: ">"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.playing; onActivated: player.speedStep(true) }
    Shortcut { sequence: "Down"; context: Qt.ApplicationShortcut; enabled: !win.typing && !win.quiet && win.steering; onActivated: { player.mpv.volume = player.mpv.volume - 5; player.reveal() } }

    // Mirrors for right-to-left languages. A top-level window ignores the
    // application's layout direction.
    Item {
        id: root
        width: win.width / win.zoom
        height: win.height / win.zoom
        scale: win.zoom
        transformOrigin: Item.TopLeft
        LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
        LayoutMirroring.childrenInherit: true

        Item {
            id: content
            anchors.fill: parent

            // The places, the search field and the route, across the top.
            TopBar {
                id: bar
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                // In fullscreen the video has the whole window.
                visible: !(win.watching && win.fullscreen)
                current: win.watching ? "" : win.place
                route: backend.config.route || "direct"
                routeSummary: backend.privacy.summary
                onPicked: place => win.go(place)
                onRouteClicked: win.toggleRoute()

                FindField {
                    id: search
                    objectName: "search"
                    // In a narrow tile it is folded into its key, and opens
                    // over the places while it is used.
                    readonly property bool open: !bar.folded || editing || text !== ""
                    anchors.horizontalCenter: bar.folded ? undefined : parent.horizontalCenter
                    anchors.right: bar.folded ? parent.right : undefined
                    anchors.verticalCenter: parent.verticalCenter
                    width: !bar.folded ? Math.min(460, parent.width) : open ? bar.openWidth : 40
                    Behavior on width { enabled: bar.folded; NumberAnimation { duration: Style.move; easing.type: Easing.OutCubic } }
                    z: 3
                    clip: true
                    // (So that the places don't show through it while it is over them.)
                    Rectangle {
                        z: -1
                        anchors.fill: parent
                        visible: bar.folded
                        color: Style.bg
                    }
                    MouseArea {
                        anchors.fill: parent
                        visible: bar.folded && !search.open
                        cursorShape: Qt.PointingHandCursor
                        onClicked: search.edit()
                    }
                    onEscaped: win.focusPage(win.page)
                    onSubmitted: query => {
                        win.go("search")
                        search.text = query
                        backend.search(query)
                    }
                }
            }
            // The route opens under where it is shown.
            RoutePopup {
                id: route
                objectName: "routePopup"
                parent: bar.routeAnchor
                x: Qt.application.layoutDirection === Qt.RightToLeft ? 0 : parent.width - width
                y: parent.height
                above: bar.height
                windowWidth: root.width
                windowHeight: root.height
                onSettingsRequested: win.go("settings")
                // The keyboard goes back to what is showing (unless a click
                // that closed it put it in a field).
                onClosed: win.afterPopup()
            }

            // Errors stay until dismissed, under the bar.
            ErrorBar {
                id: errorBar
                objectName: "errorBar"
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: bar.visible ? bar.bottom : parent.top
                z: 2
                onRouteRequested: win.openRoute()
            }

            // Under the bar, how what is typed in the search field was read.
            Item {
                id: top
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: errorBar.visible ? errorBar.bottom : bar.visible ? bar.bottom : parent.top
                anchors.leftMargin: Style.gutter
                anchors.rightMargin: Style.gutter
                height: tags.visible ? tags.height + Style.u * 3 : 0

                ReadingTags {
                    id: tags
                    objectName: "reading-tags"
                    y: Style.u * 3
                    width: parent.width
                    // Only for what is in the field now, and only when there is a rule to show.
                    visible: backend.reading.rules === true && backend.reading.text === search.text.trim()
                    tags: backend.reading.tags || []
                }
            }

            // What plays in the small window, along the foot of this one.
            NowPlayingBar {
                id: nowPlaying
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                visible: win.pip
                surface: pipWindow.surface
                onBackRequested: win.togglePip()
                onCloseRequested: pipWindow.end()
            }

            Item {
                id: stage
                objectName: "stage"
                // What comes in fades and settles into place; what left is gone at once.
                transform: Translate { id: settle }
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: top.bottom
                anchors.bottom: nowPlaying.visible ? nowPlaying.top : parent.bottom

                HomePage {
                    id: feedPage
                    objectName: "feedPage"
                    anchors.fill: parent
                    visible: win.page === "home" && !win.watching
                    focus: visible
                    thumbs: win.thumbs
                    shorts: prefs.showShorts
                    progress: win.watched
                    arrangements: win.arrangements
                    watched: win.watchedIds
                    onArranged: (key, setting) => win.arrange(key, setting)
                    onActivated: (video, index) => win.play(video, feedPage.model, index)
                    onShelfActivated: (video, list, index) => win.play(video, list, index)
                    onChannelRequested: id => win.openChannel(id)
                    onResumed: (video, list, index) => win.play(video, list, index)
                    onRestarted: (video, list, index) => win.startOver(video, list, index)
                    onForgotten: kept => win.forgotPlace(kept)
                    onShortsToggled: shown => {
                        prefs.showShorts = shown
                        backend.refreshFeed(false, shown)
                    }
                    onRefreshRequested: win.refresh()
                    onNewLensRequested: {
                        win.go("lenses")
                        lensesPage.startNew()
                    }
                    onAddChannelRequested: win.addChannel()
                }

                VideoPage {
                    id: searchPage
                    objectName: "searchPage"
                    anchors.fill: parent
                    visible: win.page === "search" && !win.watching
                    thumbs: win.thumbs
                    progress: win.watched
                    heading: backend.query ? qsTr("\u201c%1\u201d").arg(backend.query) : qsTr("Search")
                    subheading: backend.privacy.pages.search
                    model: backend.results
                    arrange: win.arrangements.search || ({})
                    sourceLabel: qsTr("Best match")
                    watched: win.watchedIds
                    onArranged: setting => win.arrange("search", setting)
                    estimateNote: qsTr("Approximate: YouTube's search only says how long ago")
                    loading: backend.busy.indexOf("search") >= 0
                    emptyText: backend.query ? qsTr("No results.") : ""
                    onActivated: (video, index) => win.play(video, searchPage.grid.list, index)
                    onChannelRequested: id => win.openChannel(id)

                    // A search worth repeating becomes a feed.
                    Act {
                        objectName: "keep-lens"
                        visible: backend.query !== ""
                        text: qsTr("Keep as a lens")
                        glyph: "qrc:/icons/lens.svg"
                        tip: qsTr("Saves this search as a feed you can come back to")
                        onClicked: backend.addLens(backend.query)
                    }
                }

                VideoPage {
                    id: channelPage
                    anchors.fill: parent
                    visible: win.page === "channel" && !win.watching
                    thumbs: win.thumbs
                    progress: win.watched
                    heading: backend.channel.name || ""
                    subheading: backend.channelVideos.count
                                ? qsTr("%n recent videos", "", backend.channelVideos.count) : ""
                    model: backend.channelVideos
                    arrange: win.arrangements.channel || ({})
                    watched: win.watchedIds
                    onArranged: setting => win.arrange("channel", setting)
                    estimateNote: qsTr("Approximate: YouTube's channel page only says how long ago")
                    loading: backend.busy.indexOf("channel") >= 0
                    emptyText: qsTr("No videos.")
                    onActivated: (video, index) => win.play(video, channelPage.grid.list, index)

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

                ChannelsPage {
                    id: subsPage
                    anchors.fill: parent
                    visible: win.page === "channels" && !win.watching
                    thumbs: win.thumbs
                    progress: win.watched
                    onChannelRequested: id => win.openChannel(id)
                    onActivated: (video, list, index) => win.play(video, list, index)
                }

                HistoryPage {
                    id: historyPage
                    anchors.fill: parent
                    thumbs: win.thumbs
                    visible: win.page === "history" && !win.watching
                    onPlayRequested: (video, list, index) => win.play(video, list, index)
                }

                WatchPage {
                    id: watch
                    anchors.fill: parent
                    visible: active
                    list: win.playList
                    index: win.playIndex
                    thumbs: win.thumbs
                    full: win.fullscreen
                    hold: route.opened || help.opened
                    savedVolume: prefs.volume
                    onSavedVolumeChanged: prefs.volume = savedVolume
                    onClosed: {
                        if (win.fullscreen)
                            win.showNormal()
                        theatre = false
                        // (Not out of a popup that is open: it gives the keyboard back when it closes.)
                        Qt.callLater(() => { if (!route.opened && !help.opened) win.focusPage(win.page) })
                    }
                    onChannelRequested: id => win.openChannel(id)
                    onVideoRequested: (video, list, index) => win.play(video, list, index)
                    onFullscreenToggled: win.toggleFullscreen()
                    onPipRequested: win.togglePip()
                    onCopied: text => {
                        win.copy(text)
                        toast.show(qsTr("Link copied. qveil doesn't open browsers."))
                    }
                }

                LensesPage {
                    id: lensesPage
                    anchors.fill: parent
                    visible: win.page === "lenses" && !win.watching
                    thumbs: win.thumbs
                    progress: win.watched
                    arrangements: win.arrangements
                    watched: win.watchedIds
                    onArranged: (key, setting) => win.arrange(key, setting)
                    onActivated: (video, index) => win.play(video, lensesPage.grid.list, index)
                    onChannelRequested: id => win.openChannel(id)
                }

                SettingsPage {
                    id: settingsPage
                    anchors.fill: parent
                    visible: win.page === "settings" && !win.watching
                    thumbs: win.thumbs
                    shorts: prefs.showShorts
                    onThumbsPicked: mode => win.thumbs = mode
                    onShortsPicked: shown => {
                        prefs.showShorts = shown
                        backend.refreshFeed(false, shown)
                    }
                }

                // A thin line along the top of the page while anything is
                // loading. It fills as a feed refresh gets through its
                // channels, and only then: anything else running at the same
                // time makes it as long as that is.
                BusyLine {
                    objectName: "progress"
                    readonly property var tasks: backend.busy.filter(t => ["play", "config", "read", "resume", "resume-note", "lenses"].indexOf(t) < 0 && t.indexOf("shelf-") !== 0 && t !== "topics-list" && t !== "history")
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    indeterminate: !(backend.refreshTotal > 0 && tasks.length === 1 && tasks[0] === "feed")
                    value: backend.refreshTotal > 0 ? backend.refreshDone / backend.refreshTotal : 0
                    visible: !win.watching && tasks.length > 0
                }
            }
        }

        PipWindow {
            id: pipWindow
            thumbs: win.thumbs
            savedVolume: prefs.volume
            onSavedVolumeChanged: prefs.volume = savedVolume
            onBackRequested: win.togglePip()
        }

        TextEdit {
            id: clipboard
            visible: false
        }

        Toast {
            id: toast
            objectName: "toast"
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: Style.gutter
            z: 5
            onActed: win.restorePlace()
        }

        ResumeNotice {
            id: resumeNotice
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: toast.visible ? toast.top : parent.bottom
            anchors.bottomMargin: toast.visible ? Style.u * 2 : Style.gutter
            width: Math.min(560, parent.width - 2 * Style.gutter)
            z: 5
            onSettingsRequested: win.go("settings")
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

    HelpOverlay {
        id: help
        parent: root
        windowWidth: root.width
        windowHeight: root.height
        // The keyboard goes back to what is showing.
        onClosed: win.afterPopup()
    }
    // The video is rendered for the pixels it covers.
    Binding {
        target: watch.surface.mpv
        property: "renderScale"
        value: win.zoom
    }
}
