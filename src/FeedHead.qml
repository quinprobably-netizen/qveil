import QtQuick
import Qveil

// What Home's feed in full is called: the way back to the shelves and its
// name, and at the far end the things that act on it. (The lenses are not
// here: they have their own page.)
Item {
    id: head

    // What the feed is.
    property string title
    // A line about what is loading or went wrong, before the actions.
    property string status
    default property alias actions: tail.data
    // Back to the shelves.
    signal back()

    implicitHeight: Style.control

    Row {
        id: row
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u

        Act {
            id: home
            objectName: "feed-back"
            glyph: "qrc:/icons/back.svg"
            tip: qsTr("Back to home")
            quiet: true
            onClicked: head.back()
        }
        Text {
            id: name
            objectName: "feed-title"
            anchors.verticalCenter: parent.verticalCenter
            width: Math.min(implicitWidth, Math.max(0, head.width - home.width - tail.width - note.width - Style.u * 8))
            text: head.title
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.title
            font.weight: Style.strong
        }
    }
    Text {
        id: note
        anchors.right: tail.left
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        text: head.status
        textFormat: Text.PlainText
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
    Row {
        id: tail
        anchors.right: parent.right
        // The last word stands at the edge, not its button's padding inside it.
        anchors.rightMargin: -Style.actPad
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u
    }
}
