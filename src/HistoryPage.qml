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
            // The list below may be hidden (history off), and then nothing
            // takes the rest of the height: the header would drift to the middle.
            Layout.alignment: Qt.AlignTop
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
                    // When it is off, the engine's words about that are on the page itself.
                    visible: page.enabled_ && text !== ""
                    text: backend.privacy.pages.history_on
                    wrapMode: Text.Wrap
                    font.pixelSize: 13
                    color: "#8d8d98"
                }
            }
            Button {
                visible: page.enabled_
                text: qsTr("Turn off")
                flat: true
                onClicked: backend.setConfig({ history: false })
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
            visible: page.enabled_
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

    // Off is the first run's empty state: the same cloud of dye, and one flat
    // button, since turning tracking on is no call to action.
    EmptyState {
        anchors.fill: parent
        visible: !page.enabled_
        wash: true
        text: backend.privacy.pages.history_off
        actions: [
            Button {
                text: qsTr("Turn on")
                flat: true
                onClicked: backend.setConfig({ history: true })
            }
        ]
    }
}
