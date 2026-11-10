import QtQuick
import Qveil

// One setting: what it is and what it means on one side, its control on the
// other. What it means for privacy is the engine's sentence, passed in.
Rectangle {
    id: row

    property string name
    property string note
    // The note's name, for whoever looks for it.
    property string noteName
    // A second line, fainter: what another choice would mean.
    property string would
    property string wouldName
    default property alias control: slot.data
    // Whether the keyboard is on the row; and its choice changed by it.
    property bool marked: false
    function turn(by) {
        for (let i = 0; i < slot.children.length; i++)
            if (slot.children[i].turn)
                slot.children[i].turn(by)
    }
    border.width: marked ? 2 : 0
    border.color: Style.accent
    // What goes under the notes.
    property alias under: below.data

    implicitHeight: Math.max(words.height, slot.height) + Style.u * 7
    color: Style.rest
    radius: Style.radius

    Column {
        id: words
        anchors.left: parent.left
        anchors.leftMargin: Style.u * 4
        anchors.right: slot.left
        anchors.rightMargin: Style.u * 6
        anchors.verticalCenter: parent.verticalCenter
        spacing: 3

        Text {
            objectName: "setting-name"
            width: Math.min(implicitWidth, parent.width)
            text: row.name
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.card
            font.weight: Style.strong
        }
        Notice {
            objectName: row.noteName
            width: parent.width
            text: row.note
        }
        Row {
            id: below
            topPadding: children.length > 0 && visibleChildren.length > 0 ? Style.u : 0
        }
        Notice {
            objectName: row.wouldName
            width: parent.width
            text: row.would
            color: Style.faint
        }
    }
    Row {
        id: slot
        anchors.right: parent.right
        anchors.rightMargin: Style.u * 4
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u * 3
    }
}
