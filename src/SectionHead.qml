import QtQuick
import Qveil

// The head of a section: its name, and whatever is put inside at the far end
// of the same line.
Item {
    id: head

    property string text
    default property alias trailing: tail.data

    implicitHeight: Math.max(label.implicitHeight, tail.height, Style.u * 6)

    Text {
        id: label
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: head.text
        textFormat: Text.PlainText
        color: Style.fg
        font.family: Style.font
        font.pixelSize: Style.body + 1
        font.weight: Style.strong
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
