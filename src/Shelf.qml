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
    // Whether the keyboard is on this shelf, and on which of its videos.
    property bool current: false
    property int column: 0
    // Actions, after the note.
    default property alias actions: tail.data
    signal activated(var video, int index)
    signal channelRequested(string id)
    signal allRequested()
    // The pointer is on one of its cards.
    signal pointed()

    objectName: "shelf-" + key
    // As tall while it is still empty as it will be with its videos, so
    // that the page doesn't jump as the shelves fill.
    implicitHeight: head.height + Style.u * 2 + row.cellHeight

    onColumnChanged: if (current) row.positionViewAtIndex(column, ListView.Contain)

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
            Act {
                objectName: "shelf-all"
                visible: shelf.all
                text: qsTr("See all")
                quiet: true
                onClicked: shelf.allRequested()
            }
        }
    }

    ListView {
        id: row
        // What a card asks of what it is in.
        readonly property string thumbs: shelf.thumbs
        readonly property var progress: shelf.progress
        readonly property string estimateNote: shelf.estimateNote
        readonly property int cellWidth: 288
        // The cards have a line saying why they are there when their videos do.
        readonly property bool reasons: shelf.entries.length > 0 && (shelf.entries[0].why || []).length > 0
        readonly property int cellHeight: Math.round((cellWidth - Style.u * 4) * 9 / 16) + 104 + (reasons ? 18 : 0)
        signal channelRequested(string id)
        function pick(i) { shelf.activated(shelf.entries[i], i) }
        function pointed() { shelf.pointed() }
        onChannelRequested: id => shelf.channelRequested(id)

        anchors.top: head.bottom
        anchors.topMargin: Style.u * 2
        // The cards have a margin of their own: the first picture starts under the heading.
        x: -Style.u * 2
        width: parent.width + Style.u * 4
        height: cellHeight
        visible: shelf.entries.length > 0
        orientation: ListView.Horizontal
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: shelf.entries
        delegate: VideoCard {
            required property var modelData
            video: modelData
            view: row
            selected: shelf.current && shelf.column === index
        }
    }
    Text {
        id: none
        anchors.top: head.bottom
        anchors.topMargin: Style.u * 3
        width: parent.width
        visible: shelf.entries.length === 0 && text !== ""
        text: shelf.empty
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: Style.dim
        font.family: Style.font
        font.pixelSize: Style.body
    }
}
