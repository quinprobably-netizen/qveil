import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// What a page says instead of an empty grid: a line of text, and what to do
// about it, in the middle of the page. The first run and a switched-off
// history sit it on a soft cloud of the veil's dye (`wash`).
Item {
    id: empty

    property string text
    property bool wash: false
    // The buttons under the text.
    property alias actions: actionRow.data

    Rectangle {
        id: washSource
        anchors.fill: cloud
        color: "#9a9aa0"
        visible: false
        layer.enabled: true
    }
    DyeWash {
        id: cloud
        anchors.centerIn: column
        width: column.width + 260
        height: column.height + 200
        visible: empty.wash && empty.visible
        source: washSource
        seed: 11.3
        vignette: 1
        opacity: 0.55
    }

    ColumnLayout {
        id: column
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 600)
        spacing: 0

        Label {
            Layout.fillWidth: true
            visible: text !== ""
            text: empty.text
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: 15
            lineHeight: 1.3
            color: "#8d8d98"
        }

        RowLayout {
            id: actionRow
            Layout.alignment: Qt.AlignHCenter
            // Room for the actions only when the page has any.
            Layout.topMargin: children.length > 0 && empty.text !== "" ? 20 : 0
        }
    }
}
