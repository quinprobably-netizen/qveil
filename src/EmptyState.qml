import QtQuick
import Qveil

// What a page says instead of an empty grid: a line of text, and what to do
// about it, in the middle of the page.
Item {
    id: empty

    property string text
    // The actions under the text.
    default property alias actions: row.data

    Column {
        anchors.centerIn: parent
        width: Math.min(parent.width - Style.gutter * 4, 560)
        spacing: Style.u * 4

        Text {
            width: parent.width
            visible: text !== ""
            text: empty.text
            textFormat: Text.PlainText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            lineHeight: 1.3
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.body
        }
        Row {
            id: row
            anchors.horizontalCenter: parent.horizontalCenter
            spacing: Style.u * 2
        }
    }
}
