import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import "Format.js" as Format

FocusScope {
    id: page

    readonly property bool enabled_: !!backend.config.history
    // Where focus belongs when the page is shown.
    readonly property Item content: list
    signal playRequested(var video)

    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: 28
        anchors.rightMargin: 28
        anchors.topMargin: 22
        spacing: 10

        RowLayout {
            Layout.fillWidth: true
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3
                Label {
                    text: qsTr("History")
                    font.pixelSize: 26
                    font.weight: Font.DemiBold
                }
                Label {
                    Layout.fillWidth: true
                    text: page.enabled_ ? qsTr("Stored only on this computer.")
                                        : qsTr("Off. qveil doesn't remember what you watch.")
                    wrapMode: Text.Wrap
                    font.pixelSize: 13
                    color: "#8d8d98"
                }
            }
            Button {
                text: page.enabled_ ? qsTr("Turn off") : qsTr("Turn on")
                // Flat both ways: turning tracking on is no call to action.
                flat: true
                onClicked: backend.setConfig({ history: !page.enabled_ })
            }
            Button {
                visible: backend.history.length > 0
                text: qsTr("Clear")
                flat: true
                onClicked: backend.clearHistory()
            }
        }

        QuietList {
            id: list
            focus: true
            Layout.fillWidth: true
            Layout.fillHeight: true
            model: backend.history

            delegate: QuietRow {
                id: row
                required property var modelData
                width: ListView.view.width
                height: 52
                onClicked: page.playRequested(modelData)
                contentItem: RowLayout {
                    spacing: 12
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 1
                        Label {
                            Layout.fillWidth: true
                            text: row.modelData.title
                            elide: Text.ElideRight
                            font.pixelSize: 14
                        }
                        Label {
                            Layout.fillWidth: true
                            text: row.modelData.channel || ""
                            visible: text !== ""
                            elide: Text.ElideRight
                            font.pixelSize: 12
                            color: "#8d8d98"
                        }
                    }
                    Label {
                        text: Format.age(row.modelData.at)
                        font.pixelSize: 12
                        color: "#6f6f7b"
                    }
                }
            }
        }
    }
}
