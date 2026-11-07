import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

Popup {
    id: help

    anchors.centerIn: Overlay.overlay
    modal: true
    padding: 26
    width: 520

    background: Rectangle {
        color: "#16161b"
        radius: theme.radius
        border.color: "#2a2a32"
    }

    readonly property var shortcuts: [
        ["/", qsTr("Search")],
        ["1  2  3", qsTr("Feed, subscriptions, history")],
        ["← → ↑ ↓  Enter", qsTr("Move and play")],
        ["R", qsTr("Refresh the feed")],
        ["Space", qsTr("Play or pause")],
        ["← →", qsTr("Back or forward 5 seconds while playing")],
        ["J  L", qsTr("Back or forward 10 seconds")],
        ["↑ ↓", qsTr("Volume while playing")],
        ["M", qsTr("Mute")],
        ["F", qsTr("Fullscreen")],
        ["Esc", qsTr("Back")],
        ["?", qsTr("This help")],
        ["Q", qsTr("Quit")],
    ]

    ColumnLayout {
        width: parent.width
        spacing: 14

        Label {
            text: qsTr("Keyboard")
            font.pixelSize: 17
            font.weight: Font.DemiBold
        }

        GridLayout {
            columns: 2
            columnSpacing: 22
            rowSpacing: 8
            Repeater {
                model: help.shortcuts
                delegate: Label {
                    required property var modelData
                    required property int index
                    Layout.row: index
                    Layout.column: 0
                    text: modelData[0]
                    font.weight: Font.DemiBold
                    color: theme.accent
                }
            }
            Repeater {
                model: help.shortcuts
                delegate: Label {
                    required property var modelData
                    required property int index
                    Layout.row: index
                    Layout.column: 1
                    Layout.fillWidth: true
                    text: modelData[1]
                    color: "#d0d0d8"
                }
            }
        }
    }
}
