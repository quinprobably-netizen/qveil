import QtCore
import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

ApplicationWindow {
    id: win
    width: 1280
    height: 820
    minimumWidth: 760
    minimumHeight: 500
    visible: true
    title: player.active && player.info.title ? player.info.title + " · veil" : "veil"
    color: "#0d0d10"

    Material.theme: Material.Dark
    Material.accent: theme.accent
    Material.background: "#0d0d10"
    Material.roundedScale: theme.radius

    property string page: "feed"
    property string returnPage: "feed"
    // Whether the first engine start has loaded the window yet.
    property bool started: false
    readonly property bool fullscreen: visibility === Window.FullScreen
    // Before the first subscription there is nothing to refresh or filter. Both
    // counts must say so, so a loading window doesn't flash the controls away.
    readonly property bool noSubs: backend.feedInfo.channels === 0 && backend.subs.length === 0
    // Whether keys are going into a text field (the search box, the add field
    // on the subscriptions page, the proxy field). Every Qt Quick text input
    // has a cursor position and nothing else that takes focus does.
    readonly property bool typing: !!activeFocusItem && typeof activeFocusItem.cursorPosition === "number"
    readonly property var pages: ({ feed: feedPage, search: searchPage, channel: channelPage,
                                    subs: subsPage, history: historyPage })

    Settings {
        id: prefs
        property bool veilThumbnails: true
        property bool showShorts: false
        property real volume: 80
    }

    function go(p) {
        if (player.active)
            player.close()
        if (p !== "channel")
            returnPage = p
        // What was typed in the top bar belongs to the search page only.
        if (p !== "search")
            search.text = ""
        page = p
        if (p === "subs")
            backend.loadSubs()
        if (p === "history")
            backend.loadHistory()
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
        if (page !== "channel")
            returnPage = page
        search.text = ""
        page = "channel"
        backend.openChannel(id)
        Qt.callLater(() => win.focusPage("channel"))
    }

    function refresh() {
        if (page === "channel")
            backend.openChannel(backend.channel.id)
        else if (page === "search")
            backend.search(backend.query)
        else
            backend.refreshFeed(true, prefs.showShorts)
    }

    function playCurrent() {
        const p = pages[page]
        if (p && p.grid && p.grid.currentIndex >= 0 && p.model.length > 0)
            backend.play(p.model[p.grid.currentIndex].id)
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
        else if (page === "channel")
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
            backend.refreshFeed(true, prefs.showShorts)
            backend.loadSubs()
        }
        function onPlayReady(info) { player.open(info) }
        function onNotice(text) { toast.show(text) }
    }

    Shortcut { sequences: ["/", "Ctrl+L"]; context: Qt.ApplicationShortcut; onActivated: { search.forceActiveFocus(); search.selectAll() } }
    Shortcut { sequence: "?"; context: Qt.ApplicationShortcut; onActivated: help.opened ? help.close() : help.open() }
    Shortcut { sequence: "Q"; context: Qt.ApplicationShortcut; onActivated: Qt.quit() }
    Shortcut { sequence: "Esc"; context: Qt.ApplicationShortcut; onActivated: win.goBack() }
    Shortcut { sequence: "Space"; context: Qt.ApplicationShortcut; onActivated: player.active ? player.mpv.togglePause() : win.playCurrent() }
    Shortcut { sequence: "1"; context: Qt.ApplicationShortcut; onActivated: win.go("feed") }
    Shortcut { sequence: "2"; context: Qt.ApplicationShortcut; onActivated: win.go("subs") }
    Shortcut { sequence: "3"; context: Qt.ApplicationShortcut; onActivated: win.go("history") }
    Shortcut { sequence: "R"; context: Qt.ApplicationShortcut; enabled: !player.active; onActivated: win.refresh() }
    Shortcut { sequence: "F"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: win.toggleFullscreen() }
    Shortcut { sequence: "M"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: player.mpv.muted = !player.mpv.muted }
    Shortcut { sequence: "Left"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.seek(-5, true); player.reveal() } }
    Shortcut { sequence: "Right"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.seek(5, true); player.reveal() } }
    Shortcut { sequence: "J"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.seek(-10, true); player.reveal() } }
    Shortcut { sequence: "L"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.seek(10, true); player.reveal() } }
    Shortcut { sequence: "Up"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.volume = player.mpv.volume + 5; player.reveal() } }
    Shortcut { sequence: "Down"; context: Qt.ApplicationShortcut; enabled: player.active; onActivated: { player.mpv.volume = player.mpv.volume - 5; player.reveal() } }

    // Right-to-left languages mirror the window; the Translator sets the
    // application's layout direction.
    Item {
        anchors.fill: parent
        LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
        LayoutMirroring.childrenInherit: true

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            // The top bar: wordmark, places, search, and the route.
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 60
                visible: !win.fullscreen
                color: "#111115"

                Rectangle {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    height: 1
                    color: "#1e1e25"
                }

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 22
                    anchors.rightMargin: 14
                    spacing: 6

                    Label {
                        text: "veil"
                        font.pixelSize: 22
                        font.weight: Font.Bold
                        font.letterSpacing: 0.5
                        Layout.rightMargin: 18
                    }

                    Repeater {
                        model: [
                            { key: "feed", label: qsTr("Feed") },
                            { key: "subs", label: qsTr("Subscriptions") },
                            { key: "history", label: qsTr("History") },
                        ]
                        delegate: TabButtonish {}
                    }

                    Item { Layout.fillWidth: true }

                    SearchBox {
                        id: search
                        Layout.preferredWidth: Math.min(420, win.width * 0.32)
                        onSubmitted: query => {
                            win.go("search")
                            backend.search(query)
                        }
                    }

                    RouteChip {
                        Layout.leftMargin: 8
                        onClicked: route.opened ? route.close() : route.open()
                        RoutePopup {
                            id: route
                            prefs: prefs
                            y: parent.height + 8
                            x: parent.width - width
                        }
                    }
                }
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                VideoPage {
                    id: feedPage
                    anchors.fill: parent
                    visible: win.page === "feed" && !player.active
                    focus: visible
                    veil: prefs.veilThumbnails
                    heading: qsTr("Feed")
                    subheading: {
                        const info = backend.feedInfo
                        if (!info.channels)
                            return ""
                        let s = qsTr("Latest from %n channels", "", info.channels)
                        if (info.errors && info.errors.length > 0)
                            s += "  ·  " + qsTr("%n couldn't be fetched", "", info.errors.length)
                        return s
                    }
                    model: backend.feed
                    estimateNote: qsTr("Approximate: YouTube's feed for this channel was unavailable")
                    loading: backend.busy.indexOf("feed") >= 0 || backend.busy.indexOf("feed-cache") >= 0 || !backend.ready
                    emptyText: backend.feedInfo.channels === 0
                               ? qsTr("Your feed is built from channels you subscribe to, on this computer only.\n\nSearch for a channel, press 2 to add one, or drop a Google Takeout subscriptions.csv here.")
                               : qsTr("Nothing here yet. Press R to refresh.")
                    onActivated: video => backend.play(video.id)
                    onChannelRequested: id => win.openChannel(id)
                    actions: [
                        CheckBox {
                            visible: !win.noSubs
                            text: qsTr("Shorts")
                            checked: prefs.showShorts
                            onToggled: {
                                prefs.showShorts = checked
                                backend.refreshFeed(false, checked)
                            }
                        },
                        Button {
                            visible: !win.noSubs
                            text: qsTr("Refresh")
                            flat: true
                            icon.source: "qrc:/icons/refresh.svg"
                            enabled: backend.busy.indexOf("feed") < 0
                            onClicked: backend.refreshFeed(true, prefs.showShorts)
                        }
                    ]
                }

                VideoPage {
                    id: searchPage
                    anchors.fill: parent
                    visible: win.page === "search" && !player.active
                    veil: prefs.veilThumbnails
                    heading: backend.query ? qsTr("“%1”").arg(backend.query) : qsTr("Search")
                    subheading: qsTr("Straight from YouTube's search, without an account, cookies or history.")
                    model: backend.results
                    loading: backend.busy.indexOf("search") >= 0
                    emptyText: backend.query ? qsTr("No results.") : ""
                    onActivated: video => backend.play(video.id)
                    onChannelRequested: id => win.openChannel(id)
                }

                VideoPage {
                    id: channelPage
                    anchors.fill: parent
                    visible: win.page === "channel" && !player.active
                    veil: prefs.veilThumbnails
                    heading: backend.channel.name || ""
                    subheading: backend.channel.videos && backend.channel.videos.length
                                ? qsTr("%n recent videos  ·  dates are approximate", "", backend.channel.videos.length) : ""
                    model: backend.channel.videos || []
                    loading: backend.busy.indexOf("channel") >= 0
                    emptyText: qsTr("No videos.")
                    onActivated: video => backend.play(video.id)
                    actions: [
                        Button {
                            visible: !!backend.channel.name
                            text: backend.channel.subscribed ? qsTr("Subscribed") : qsTr("Subscribe")
                            highlighted: !backend.channel.subscribed
                            flat: !!backend.channel.subscribed
                            onClicked: backend.channel.subscribed ? backend.unsubscribe(backend.channel.id)
                                                                   : backend.subscribe(backend.channel.id)
                        }
                    ]
                }

                SubsPage {
                    id: subsPage
                    anchors.fill: parent
                    visible: win.page === "subs" && !player.active
                    onChannelRequested: id => win.openChannel(id)
                }

                HistoryPage {
                    id: historyPage
                    anchors.fill: parent
                    visible: win.page === "history" && !player.active
                    onPlayRequested: id => backend.play(id)
                }

                // Shown while the engine resolves a video, which can take a
                // few seconds (longer over Tor).
                Rectangle {
                    anchors.fill: parent
                    visible: backend.busy.indexOf("play") >= 0 && !player.active
                    color: "#b30d0d10"
                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 14
                        BusyIndicator { Layout.alignment: Qt.AlignHCenter; running: parent.visible }
                        Label {
                            Layout.alignment: Qt.AlignHCenter
                            text: backend.config.route === "tor" ? qsTr("Opening the video over Tor…") : qsTr("Opening the video…")
                            color: "#c0c0ca"
                        }
                    }
                    MouseArea { anchors.fill: parent }
                }

                PlayerView {
                    id: player
                    anchors.fill: parent
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

                ProgressBar {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    indeterminate: true
                    visible: !player.active && backend.busy.some(t => t !== "play" && t !== "config")
                }
            }
        }

        // Errors stay until dismissed; they say what went wrong.
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: 18
            width: Math.min(parent.width - 40, errorText.implicitWidth + 80)
            height: errorText.implicitHeight + 24
            visible: backend.error !== ""
            radius: theme.radius
            color: "#2b1417"
            border.color: "#6b2a31"

            Label {
                id: errorText
                anchors.left: parent.left
                anchors.right: closeError.left
                anchors.leftMargin: 16
                anchors.verticalCenter: parent.verticalCenter
                text: backend.error
                wrapMode: Text.Wrap
                color: "#ffd9dc"
            }
            ToolButton {
                id: closeError
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                icon.source: "qrc:/icons/close.svg"
                onClicked: backend.clearError()
            }
        }

        Rectangle {
            id: toast
            property string text
            function show(t) {
                text = t
                opacity = 1
                toastTimer.restart()
            }
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            anchors.bottomMargin: backend.error !== "" ? 90 : 24
            width: toastLabel.implicitWidth + 36
            height: 40
            radius: Math.min(20, theme.radius)
            color: "#202027"
            border.color: "#2e2e37"
            opacity: 0
            visible: opacity > 0
            Behavior on opacity { NumberAnimation { duration: 200 } }
            Label {
                id: toastLabel
                anchors.centerIn: parent
                text: toast.text
            }
            Timer {
                id: toastTimer
                interval: 3000
                onTriggered: toast.opacity = 0
            }
        }

        // Dropping an export imports it, offline.
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
                color: "#cc0d0d10"
                border.width: 2
                border.color: theme.accent
                Label {
                    anchors.centerIn: parent
                    text: qsTr("Drop to import subscriptions (read offline)")
                    font.pixelSize: 18
                }
            }
        }
    }

    HelpOverlay { id: help }

    component TabButtonish: AbstractButton {
        id: tab
        required property var modelData
        readonly property bool current: win.page === modelData.key
                                        || (win.page === "channel" && win.returnPage === modelData.key)
        Layout.fillHeight: true
        leftPadding: 12
        rightPadding: 12
        onClicked: win.go(modelData.key)
        contentItem: Label {
            text: tab.modelData.label
            font.pixelSize: 14
            font.weight: tab.current ? Font.DemiBold : Font.Normal
            color: tab.current ? "#ffffff" : tab.hovered ? "#d0d0d8" : "#8d8d98"
            verticalAlignment: Text.AlignVCenter
        }
        background: Item {
            Rectangle {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.leftMargin: 10
                anchors.rightMargin: 10
                height: 2
                color: theme.accent
                visible: tab.current
            }
        }
    }

    // The route, always in view: amber while YouTube can see your IP, a
    // steady green once it can't. Neither follows the theme accent, which
    // tabs, rings and buttons use.
    component RouteChip: AbstractButton {
        id: chip
        readonly property string route: backend.config.route || "direct"
        readonly property color tone: route === "direct" ? "#e0a93b" : theme.protectedColor
        Layout.preferredHeight: 34
        leftPadding: 12
        rightPadding: 14
        hoverEnabled: true
        ToolTip.visible: hovered
        ToolTip.delay: 500
        ToolTip.text: route === "direct" ? qsTr("YouTube sees your IP address. Click to change.")
                                          : qsTr("Your IP is hidden from YouTube. Click for details.")
        contentItem: RowLayout {
            spacing: 8
            Rectangle {
                Layout.preferredWidth: 8
                Layout.preferredHeight: 8
                radius: 4
                color: chip.tone
            }
            Label {
                text: chip.route === "tor" ? qsTr("Tor") : chip.route === "proxy" ? qsTr("Proxy") : qsTr("Direct")
                font.pixelSize: 13
                font.weight: Font.DemiBold
                color: chip.tone
            }
        }
        background: Rectangle {
            radius: Math.min(17, theme.radius)
            color: chip.hovered ? "#1d1d24" : "transparent"
            border.color: Qt.rgba(chip.tone.r, chip.tone.g, chip.tone.b, 0.45)
        }
    }
}
