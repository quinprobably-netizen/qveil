import QtQuick
import Qveil

// The head of a section: a small dim label, a rule running on from it, and
// whatever is put inside at the far end of the same line.
Item {
    id: head

    property string text
    default property alias trailing: tail.data

    implicitHeight: Math.max(label.implicitHeight, tail.height, Style.u * 5)

    Text {
        id: label
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: head.text
        textFormat: Text.PlainText
        color: Style.dim
        font.family: Style.font
        font.pixelSize: Style.small
    }
    Rule {
        anchors.left: label.right
        anchors.leftMargin: Style.u * 3
        anchors.right: tail.left
        anchors.rightMargin: tail.width > 0 ? Style.u * 3 : 0
        anchors.verticalCenter: parent.verticalCenter
    }
    Row {
        id: tail
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u
    }
}
