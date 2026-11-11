import QtQuick
import Qveil

// A row of videos that scrolls sideways, under a heading: one feed of the
// home page.
Item {
    id: shelf

    // What the shelf is: "resume", "latest", or a lens's id.
    property string key
    property string title
    // A dim line at the far end of the heading.
    property string note
    // The videos: objects with the engine's JSON keys.
    property var entries: []
    // Said in place of the videos when there are none.
    property string empty
    // Whether the heading leads to the feed in full.
    property bool all: true
    property string thumbs: "clear"
    property var progress: ({})
    property string estimateNote
    // Whether its videos are still coming: their places are shown meanwhile.
    property bool loading: false
    // Whether the keyboard is on this shelf, and on which of its videos.
    property bool current: false
    property int column: 0
    // How it is sorted and filtered (see Arrange.js), and what the order it
    // came in is called.
    property var arrange: ({})
    property string sourceLabel: qsTr("Newest")
    signal arranged(var setting)
    // Actions, after the note.
    default property alias actions: tail.data
    // How many cards fit across: whole ones, sized to the width.
    readonly property int across: Math.max(2, Math.floor((width + Style.u * 4) / 280))
    // How many rows it may take. More than one, and it doesn't scroll sideways.
    property int rows: 1
    readonly property bool wrapped: rows > 1
    // (While it is still empty it is as tall as it will be, so that the page doesn't jump as it fills.)
    readonly property int shownRows: !wrapped ? 1
        : entries.length > 0 ? Math.min(rows, Math.ceil(entries.length / across)) : rows
    readonly property alias cellHeight: row.cellHeight
    // Where the row of the video at `index` starts, from the shelf's top.
    function rowTop(index) {
        return head.height + Style.u * 2 + (wrapped ? Math.floor(index / across) * row.cellHeight : 0)
    }
    function openSort() { sortMenu.open() }
    signal activated(var video, int index)
    signal channelRequested(string id)
    signal allRequested()
    // The pointer is on one of its cards.
    signal pointed(point at)

    objectName: "shelf-" + key
    // As tall while it is still empty as it will be with its videos, so
    // that the page doesn't jump as the shelves fill.
    implicitHeight: head.height + Style.u * 2 + row.cellHeight * shownRows

    onColumnChanged: if (current && !wrapped) reveal(column)

    // Glides the row so that the video at `index` is in view.
    function reveal(index) {
        const x = index * row.cellWidth
        if (x < row.contentX)
            glideTo(x)
        else if (x + row.cellWidth > row.contentX + row.width)
            glideTo(x + row.cellWidth - row.width)
    }
    // A row's width on, or back; always to where a card starts.
    function turn(by) {
        const from = glide.running ? glide.to : row.contentX
        glideTo(Math.round((from + by * across * row.cellWidth) / row.cellWidth) * row.cellWidth)
    }
    function glideTo(x) {
        const to = Math.max(0, Math.min(row.contentWidth - row.width, x))
        glide.stop()
        if (Style.move === 0) {
            row.contentX = to
            return
        }
        glide.to = to
        glide.start()
    }
    NumberAnimation {
        id: glide
        target: row
        property: "contentX"
        duration: Style.move * 1.6
        easing.type: Easing.OutCubic
    }
    HoverHandler { id: over }
    VideoModel { id: held }
    onEntriesChanged: held.assign(entries)
    // Sorted or filtered anew, the row starts from its start.
    readonly property string arrangedAs: JSON.stringify(arrange)
    onArrangedAsChanged: {
        glide.stop()
        row.contentX = 0
    }
    Component.onCompleted: held.assign(entries)

    Item {
        id: head
        width: parent.width
        height: Style.control

        Text {
            id: name
            objectName: "shelf-title"
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            // As wide as its words, so that the heading is what is pressed.
            width: Math.min(implicitWidth, head.width - tail.width - Style.u * 4)
            text: shelf.title
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: nameArea.containsMouse ? Style.accent : Style.fg
            font.family: Style.font
            font.pixelSize: Style.title
            font.weight: Style.strong

            MouseArea {
                id: nameArea
                anchors.fill: parent
                enabled: shelf.all
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: shelf.allRequested()
            }
        }
        Row {
            id: tail
            anchors.right: parent.right
            // The last word stands at the edge, not its button's padding inside it.
            anchors.rightMargin: -Style.actPad
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.u

            Text {
                anchors.verticalCenter: parent.verticalCenter
                rightPadding: Style.u * 2
                visible: text !== ""
                text: shelf.note
                textFormat: Text.PlainText
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
            SortMenu {
                id: sortMenu
                objectName: "shelf-sort"
                setting: shelf.arrange
                sourceLabel: shelf.sourceLabel
                onChanged: setting => shelf.arranged(setting)
            }
            Act {
                objectName: "shelf-all"
                visible: shelf.all
                text: qsTr("See all")
                quiet: true
                onClicked: shelf.allRequested()
            }
        }
    }

    // One row that scrolls sideways, or (when there are more rows than one)
    // rows that fill the width and don't scroll.
    GridView {
        id: row
        objectName: "shelf-row"
        // What a card asks of what it is in.
        readonly property string thumbs: shelf.thumbs
        readonly property var progress: shelf.progress
        readonly property string estimateNote: shelf.estimateNote
        // The cards have a line saying why they are there when their videos do.
        // (When they have reasons, and not all the same one; what is left
        // of a video is always worth saying, and keeps the accent.)
        readonly property bool loudReasons: shelf.key === "resume"
        readonly property bool reasons: {
            const all = shelf.entries.map(v => (v.why || []).join("|"))
            if (loudReasons)
                return all.some(w => w !== "")
            return all.some(w => w !== all[0])
        }
        signal channelRequested(string id)
        function pick(i) { shelf.activated(shelf.entries[i], i) }
        function pointed(at) { shelf.pointed(at) }
        onChannelRequested: id => shelf.channelRequested(id)

        anchors.top: head.bottom
        anchors.topMargin: Style.u * 2
        // The cards have a margin of their own: the first picture starts
        // under the heading, and the last one ends at the page's edge.
        x: -Style.u * 2
        // (Exactly as wide as its cards, so that it always rests on a whole one.)
        width: cellWidth * shelf.across
        height: cellHeight * shelf.shownRows
        cellWidth: Math.floor((parent.width + Style.u * 4) / shelf.across)
        // Let go of, it settles on a whole card.
        snapMode: GridView.SnapToRow
        cellHeight: Math.round((cellWidth - Style.u * 4) * 9 / 16) + 104 + (reasons ? 18 : 0)
        flow: shelf.wrapped ? GridView.FlowLeftToRight : GridView.FlowTopToBottom
        interactive: !shelf.wrapped
        visible: shelf.entries.length > 0
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        keyNavigationEnabled: false
        // Held in a list that is changed where it differs, so that the row
        // keeps its place when its videos change.
        model: held
        delegate: VideoCard {
            required property var model
            video: model
            view: row
            selected: shelf.current && shelf.column === index
        }
    }
    // The places of what is coming, breathing while they are empty.
    Grid {
        anchors.top: head.bottom
        anchors.topMargin: Style.u * 2
        x: row.x
        columns: shelf.across
        visible: shelf.loading && shelf.entries.length === 0
        Repeater {
            model: parent.visible ? shelf.across * (shelf.wrapped ? shelf.rows : 1) : 0
            delegate: Item {
                objectName: "card-placeholder"
                width: row.cellWidth
                height: row.cellHeight
                Rectangle {
                    id: place
                    x: Style.u * 2
                    y: Style.u * 2
                    width: parent.width - Style.u * 4
                    height: Math.round(width * 9 / 16)
                    color: Style.rest
                    SequentialAnimation on opacity {
                        running: place.visible && Style.motion
                        loops: Animation.Infinite
                        NumberAnimation { from: 1; to: 0.45; duration: 550; easing.type: Easing.InOutSine }
                        NumberAnimation { from: 0.45; to: 1; duration: 550; easing.type: Easing.InOutSine }
                    }
                }
                Rectangle {
                    x: Style.u * 2
                    anchors.top: place.bottom
                    anchors.topMargin: Style.u * 3
                    width: place.width * 0.7
                    height: Style.card
                    color: Style.rest
                }
            }
        }
    }
    // More of the row, a row at a time: at its edges while it is pointed at.
    component Turn: Rectangle {
        id: turnButton
        property int by: 1
        property bool can: false
        y: row.y + Style.u * 2
        width: 44
        height: Math.round((row.cellWidth - Style.u * 4) * 9 / 16)
        opacity: over.hovered && !shelf.wrapped && can ? 1 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: Style.quick } }
        gradient: Gradient {
            orientation: Gradient.Horizontal
            GradientStop { position: turnButton.by > 0 ? 1 : 0; color: Qt.rgba(Style.bg.r, Style.bg.g, Style.bg.b, 0.92) }
            GradientStop { position: turnButton.by > 0 ? 0 : 1; color: Qt.rgba(Style.bg.r, Style.bg.g, Style.bg.b, 0) }
        }
        Glyph {
            anchors.centerIn: parent
            source: "qrc:/icons/back.svg"
            rotation: turnButton.by > 0 ? 180 : 0
            size: 20
            color: Style.fg
        }
        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: shelf.turn(turnButton.by)
        }
    }
    Turn {
        objectName: "shelf-prev"
        by: -1
        can: row.contentX > 1
        anchors.left: parent.left
    }
    Turn {
        objectName: "shelf-next"
        can: row.contentX + row.width < row.contentWidth - 1
        anchors.right: parent.right
    }
    Text {
        id: none
        anchors.top: head.bottom
        anchors.topMargin: Style.u * 3
        width: parent.width
        visible: shelf.entries.length === 0 && text !== "" && !shelf.loading
        text: shelf.empty
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: Style.dim
        font.family: Style.font
        font.pixelSize: Style.body
    }
}
