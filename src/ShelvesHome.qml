import QtQuick
import QtQuick.Controls
import Qveil
import "Format.js" as Format

// Home, as it opens: the video you were in the middle of leads the page, and
// under it every feed is a shelf: the other unfinished videos, the latest
// from the channels you follow, and each of your lenses.
FocusScope {
    id: page
    objectName: "shelves-home"

    property string thumbs: "clear"
    // How far each unfinished video was watched: { id: fraction }.
    property var progress: ({})
    // A line about the channels' feed: how many, or how the refresh is going.
    property string status
    property bool loading: false
    property bool noChannels: false
    // How long positions are kept, in days.
    readonly property int days: backend.config.resumeDays || 0
    // The video that leads: the one most recently left unfinished.
    readonly property var first: backend.resume.length > 0 ? backend.resume[0] : null
    // The shelves, by key: "latest", "resume" (what else was left
    // unfinished, when there is more than the lead), then each lens's id. Kept as
    // it is while the same shelves are wanted, so that a list that is read
    // again doesn't rebuild the page.
    property var keys: ["latest"]
    // Where the keyboard is: which shelf, and (remembered per shelf) which video on it.
    property int row: 0
    property var columns: ({})
    readonly property int column: columns[keys[row]] || 0
    // The keyboard's mark shows once the keyboard is used, and hides under the pointer.
    property bool navigating: false
    // How many videos a shelf shows; the rest is a step away, in full.
    readonly property int most: 12
    // How many cards fit across, and how tall one is (as in Shelf).
    readonly property int across: Math.max(2, Math.floor((view.width + Style.u * 4) / 280))
    readonly property int cardHeight: Math.round((Math.floor((view.width + Style.u * 4) / across) - Style.u * 4) * 9 / 16) + 104
    // The latest take the rows the window has free under the lead and beside
    // the other shelves, so that a window with little else in it is not left
    // empty: one row that scrolls sideways when there is no room for more.
    readonly property int latestRows: {
        const shelf = Style.control + Style.u * 2 + cardHeight + 18 + view.spacing
        const lead = first !== null ? (view.width < 840 ? 200 : 264) + view.spacing : 0
        const free = view.height - view.topMargin - lead - (keys.length - 1) * shelf
                     - Style.control - Style.u * 2 - (Style.control + 2 * view.spacing)
        return Math.max(1, Math.min(6, Math.ceil(free / cardHeight)))
    }

    signal activated(var video, var list, int index)
    signal resumed(var video)
    signal allRequested(string feed)
    signal channelRequested(string id)
    signal refreshRequested()
    signal addChannelRequested()
    signal newLensRequested()

    // When each lens's shelf was last read, by what the lens had fetched
    // then: a shelf is read once, and again when its lens is fetched anew.
    // (Not a property: nothing is drawn from it.)
    readonly property var read: ({})
    function wantShelf(lens) {
        if (!lens.id || !lens.fetchedAt || read[lens.id] === lens.fetchedAt)
            return
        read[lens.id] = lens.fetchedAt
        backend.loadShelf(lens.id)
    }
    function rekey() {
        const next = ["latest"]
        if (backend.resume.length > 1)
            next.push("resume")
        for (const lens of backend.lenses)
            next.push(lens.id)
        if (next.join("|") !== keys.join("|")) {
            keys = next
            row = Math.max(0, Math.min(row, keys.length - 1))
        }
    }
    function lensOf(key) {
        for (const lens of backend.lenses)
            if (lens.id === key)
                return lens
        return ({})
    }
    // A shelf's videos.
    function entriesOf(key) {
        const out = []
        if (key === "resume") {
            for (const kept of backend.resume.slice(1, most + 1))
                out.push({ id: kept.id, title: kept.title || "", channel: kept.channel || "", duration: kept.dur,
                           why: [qsTr("%1 left").arg(Format.clock(Math.max(0, kept.dur - kept.pos)))] })
        } else if (key === "latest") {
            const n = latestRows > 1 ? latestRows * across : most
            for (let i = 0; i < backend.feed.count && i < n; i++)
                out.push(backend.feed.get(i))
        } else {
            return (backend.shelves[key] || []).slice(0, most)
        }
        return out
    }
    function pick(key, video, index) {
        if (key === "resume") {
            page.resumed(video)
        } else if (key === "latest") {
            // The shelf is the start of the feed: what comes next is the rest of it.
            page.activated(video, backend.feed, index)
        } else {
            const all = backend.shelves[key] || []
            page.activated(video, { count: all.length, get: i => all[i] }, index)
        }
    }
    function move(by) {
        navigating = true
        const key = keys[row]
        const last = entriesOf(key).length - 1
        const next = Object.assign({}, columns)
        next[key] = Math.max(0, Math.min(last, column + by))
        columns = next
    }
    function step(by) {
        navigating = true
        // Within the rows of the latest first; then to the shelf before or after.
        if (keys[row] === "latest" && latestRows > 1) {
            const to = column + by * across
            if (to >= 0 && to < entriesOf("latest").length) {
                const next = Object.assign({}, columns)
                next.latest = to
                columns = next
                Qt.callLater(ensure)
                return
            }
        }
        row = Math.max(0, Math.min(keys.length - 1, row + by))
        Qt.callLater(ensure)
    }
    // Brings what the keyboard is on into view.
    function ensure() {
        const shelf = view.itemAtIndex(row)
        if (!shelf) {
            view.positionViewAtIndex(row, ListView.Contain)
            return
        }
        const top = shelf.y + shelf.rowTop(column)
        const bottom = top + shelf.cellHeight
        if (row === 0 && top - shelf.y < shelf.cellHeight)
            view.positionViewAtBeginning()
        else if (bottom > view.contentY + view.height)
            view.contentY = bottom - view.height + Style.u * 2
        else if (top < view.contentY)
            view.contentY = shelf.y
    }
    function pickCurrent() {
        const key = keys[row]
        const entries = entriesOf(key)
        if (column < entries.length)
            pick(key, entries[column], column)
    }
    function startOver(kept) {
        // The place is forgotten first, so that the video starts at its start.
        backend.forgetResume(kept.id)
        page.resumed({ id: kept.id, title: kept.title || "", channel: kept.channel || "" })
    }

    Component.onCompleted: rekey()
    Connections {
        target: backend
        function onResumeChanged() { page.rekey() }
        function onLensesChanged() { page.rekey() }
    }

    Keys.onPressed: event => {
        navigating = true
        event.accepted = false
    }
    Keys.onLeftPressed: move(-1)
    Keys.onRightPressed: move(1)
    Keys.onUpPressed: step(-1)
    Keys.onDownPressed: step(1)
    Keys.onReturnPressed: pickCurrent()
    Keys.onEnterPressed: pickCurrent()

    ListView {
        id: view
        objectName: "shelves"
        anchors.fill: parent
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        topMargin: Style.u * 2
        bottomMargin: Style.gutter
        spacing: Style.u * 6
        clip: true
        focus: true
        // The keys are the page's: they move along a shelf and between
        // shelves. The list has no current item of its own, which it would
        // scroll to whenever it is given the keyboard.
        keyNavigationEnabled: false
        currentIndex: -1
        highlightFollowsCurrentItem: false
        boundsBehavior: Flickable.StopAtBounds
        model: page.keys

        ScrollBar.vertical: ScrollBar {
            id: bar
            // Only while the page moves or the bar is reached for: it lies over the pictures' edge.
            contentItem: Rectangle {
                implicitWidth: 4
                color: bar.pressed || bar.hovered ? Style.faint : Style.line
                opacity: bar.active ? 1 : 0
                Behavior on opacity { NumberAnimation { duration: 200 } }
            }
            background: Item {}
        }

        // The lead: what you were in the middle of.
        header: Item {
            width: view.width
            // (By whether there is a lead, not by whether it is showing: a
            // page that is hidden for a while must come back as it was.)
            height: page.first !== null ? lead.height + view.spacing : 0

            Rectangle {
                id: lead
                objectName: "lead"
                readonly property var kept: page.first || ({})
                // The picture comes out of this colour.
                readonly property color panel: Qt.tint(Style.bg, Style.tint(0.05))
                readonly property bool mirrored: Qt.application.layoutDirection === Qt.RightToLeft
                width: parent.width
                height: view.width < 840 ? 200 : 264
                visible: page.first !== null
                color: panel
                clip: true

                MouseArea {
                    id: leadArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: page.resumed({ id: lead.kept.id, title: lead.kept.title || "", channel: lead.kept.channel || "" })
                }
                Item {
                    id: picture
                    objectName: "lead-picture"
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: Math.round(height * 16 / 9)
                    clip: true
                    Frost {
                        anchors.fill: parent
                        source: page.first !== null ? Format.thumb(backend.thumbBase, lead.kept.id, 0) : ""
                        clear: leadArea.containsMouse
                        mode: page.thumbs
                    }
                    Rectangle {
                        anchors.left: parent.left
                        anchors.top: parent.top
                        anchors.bottom: parent.bottom
                        width: 96
                        gradient: Gradient {
                            orientation: Gradient.Horizontal
                            GradientStop { position: lead.mirrored ? 1 : 0; color: lead.panel }
                            GradientStop { position: lead.mirrored ? 0 : 1; color: Qt.rgba(lead.panel.r, lead.panel.g, lead.panel.b, 0) }
                        }
                    }
                    // How far it was watched.
                    Rectangle {
                        anchors.left: parent.left
                        anchors.bottom: parent.bottom
                        width: parent.width * Math.min(1, (lead.kept.pos || 0) / Math.max(1, lead.kept.dur || 1))
                        height: 3
                        color: Style.accent
                    }
                }
                Column {
                    anchors.left: parent.left
                    anchors.leftMargin: Style.u * 8
                    anchors.right: picture.left
                    anchors.rightMargin: -Style.u * 8
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u * 3

                    Text {
                        text: qsTr("Continue watching")
                        textFormat: Text.PlainText
                        color: Style.accent
                        font.family: Style.font
                        font.pixelSize: Style.caption
                        font.capitalization: Font.AllUppercase
                        font.letterSpacing: 1.6
                        font.weight: Style.strong
                    }
                    Text {
                        objectName: "lead-title"
                        width: parent.width
                        text: lead.kept.title || ""
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        lineHeight: 1.05
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: lead.height < 240 ? Style.head + 2 : Style.lead
                        font.weight: Style.strong
                    }
                    Text {
                        objectName: "lead-meta"
                        width: parent.width
                        text: [lead.kept.channel || "",
                               qsTr("%1 left").arg(Format.clock(Math.max(0, (lead.kept.dur || 0) - (lead.kept.pos || 0))))]
                                  .filter(s => s !== "").join("  ·  ")
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.body
                    }
                    Row {
                        topPadding: Style.u
                        spacing: Style.u * 2
                        Act {
                            objectName: "lead-resume"
                            implicitHeight: 36
                            text: qsTr("Resume")
                            primary: true
                            onClicked: page.resumed({ id: lead.kept.id, title: lead.kept.title || "", channel: lead.kept.channel || "" })
                        }
                        Act {
                            objectName: "lead-over"
                            implicitHeight: 36
                            text: qsTr("Start over")
                            filled: true
                            onClicked: page.startOver(lead.kept)
                        }
                        Act {
                            objectName: "lead-forget"
                            implicitHeight: 36
                            text: qsTr("Forget")
                            quiet: true
                            tip: qsTr("Forget where you stopped in this video")
                            onClicked: backend.forgetResume(lead.kept.id)
                        }
                        Text {
                            objectName: "lead-kept"
                            anchors.verticalCenter: parent.verticalCenter
                            leftPadding: Style.u * 2
                            text: qsTr("kept for %n days", "", page.days)
                            textFormat: Text.PlainText
                            color: Style.faint
                            font.family: Style.font
                            font.pixelSize: Style.small
                        }
                    }
                }
            }
        }

        // After the shelves, the way to one more of them.
        footer: Item {
            width: view.width
            height: more.height + view.spacing
            Row {
                id: more
                y: view.spacing
                spacing: Style.u * 3
                Act {
                    objectName: "home-new-lens"
                    text: qsTr("New lens")
                    glyph: "qrc:/icons/plus.svg"
                    filled: true
                    enabled: backend.lenses.length < 12
                    onClicked: page.newLensRequested()
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: qsTr("A lens is a feed you describe in words. Each one is a shelf here.")
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
        }

        delegate: Shelf {
            id: shelf
            required property string modelData
            required property int index
            readonly property bool isLens: modelData !== "resume" && modelData !== "latest"
            readonly property var lens: isLens ? page.lensOf(modelData) : ({})
            // A lens's shelf is read from what the engine keeps for it, when
            // the shelf comes into being and again when the lens was fetched anew.
            readonly property var fetchedAt: lens.fetchedAt
            onFetchedAtChanged: page.wantShelf(lens)
            Component.onCompleted: page.wantShelf(lens)

            width: view.width
            height: implicitHeight
            key: modelData
            title: modelData === "resume" ? qsTr("Also unfinished")
                 : modelData === "latest" ? qsTr("Latest from your channels") : (lens.text || "")
            note: modelData === "latest" ? page.status : isLens ? qsTr("Lens") : ""
            all: modelData !== "resume"
            rows: modelData === "latest" ? page.latestRows : 1
            entries: page.entriesOf(modelData)
            empty: modelData === "latest"
                   ? (page.loading ? "" : page.noChannels
                      ? qsTr("Add a channel, or drop a Google Takeout subscriptions.csv anywhere on this window.")
                      : qsTr("Nothing here yet. Press R to refresh."))
                   : isLens && !lens.fetchedAt ? qsTr("Nothing fetched for this lens yet. Open it to run its searches.")
                   : isLens ? qsTr("Nothing fits this lens right now.") : ""
            thumbs: page.thumbs
            progress: page.progress
            estimateNote: isLens ? qsTr("Approximate: YouTube's search only says how long ago")
                                 : qsTr("Approximate: YouTube's feed for this channel was unavailable")
            current: page.navigating && page.activeFocus && page.row === index
            column: page.columns[modelData] || 0
            onActivated: (video, at) => page.pick(modelData, video, at)
            onChannelRequested: id => page.channelRequested(id)
            onAllRequested: page.allRequested(modelData)
            onPointed: page.navigating = false

            Act {
                visible: shelf.modelData === "latest" && page.noChannels
                text: qsTr("Add a channel")
                filled: true
                onClicked: page.addChannelRequested()
            }
            Act {
                visible: shelf.modelData === "latest" && !page.noChannels
                text: qsTr("Refresh")
                glyph: "qrc:/icons/refresh.svg"
                quiet: true
                enabled: !page.loading
                onClicked: page.refreshRequested()
            }
        }
    }
}
