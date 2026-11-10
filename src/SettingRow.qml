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
    default property alias control: slot.data

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
    }
    Row {
        id: slot
        anchors.right: parent.right
        anchors.rightMargin: Style.u * 4
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u * 3
    }
}
