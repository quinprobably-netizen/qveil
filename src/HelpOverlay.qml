import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

Popup {
    id: help

    anchors.centerIn: Overlay.overlay
    modal: true
    padding: 26
    width: 660

    // A dark scrim. The style's own dim is a pale grey on a dark theme, which
    // reads as a rendering fault. Every modal popup sets this.
    Overlay.modal: Rectangle {
        color: "#b0000000"
        Behavior on opacity { NumberAnimation { duration: 150 } }
    }

    background: Rectangle {
        color: "#16161b"
        radius: theme.radius
        border.color: "#2a2a32"
    }

    // Two groups, because the same key does different things in each: the
    // arrows and Space move and play while browsing, and seek and pause while
    // playing. Within a group, every key appears once.
    readonly property var groups: [
        {
            title: qsTr("Browsing"),
            keys: [
                ["/", qsTr("Search")],
                ["1  2  3", qsTr("Feed, subscriptions, history")],
                ["← → ↑ ↓", qsTr("Move between videos")],
                ["Enter  Space", qsTr("Play the selected video")],
                ["R", qsTr("Refresh")],
                ["Esc", qsTr("Back")],
                ["?", qsTr("This help")],
                ["Q", qsTr("Quit")],
            ]
        },
        {
            title: qsTr("Playing"),
            keys: [
                ["Space", qsTr("Play or pause")],
                ["← →", qsTr("Back or forward 5 seconds")],
                ["J  L", qsTr("Back or forward 10 seconds")],
                ["↑ ↓", qsTr("Volume")],
                ["M", qsTr("Mute")],
                ["F", qsTr("Fullscreen")],
            ]
        },
    ]

    ColumnLayout {
        width: parent.width
        spacing: 14

        Label {
            text: qsTr("Keyboard")
            font.pixelSize: 17
            font.weight: Font.DemiBold
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 36

            Repeater {
                model: help.groups
                delegate: ColumnLayout {
                    id: group
                    required property var modelData
                    Layout.alignment: Qt.AlignTop
                    Layout.fillWidth: true
                    // Equal widths, whatever the text in each group.
                    Layout.preferredWidth: 1
                    spacing: 8

                    Label {
                        Layout.bottomMargin: 2
                        text: group.modelData.title
                        font.pixelSize: 13
                        font.weight: Font.DemiBold
                        color: "#8d8d98"
                    }

                    Repeater {
                        model: group.modelData.keys
                        delegate: RowLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: 14

                            Label {
                                Layout.preferredWidth: 96
                                text: modelData[0]
                                font.weight: Font.DemiBold
                                color: theme.accent
                            }
                            Label {
                                Layout.fillWidth: true
                                text: modelData[1]
                                wrapMode: Text.Wrap
                                color: "#d0d0d8"
                            }
                        }
                    }
                }
            }
        }
    }
}
