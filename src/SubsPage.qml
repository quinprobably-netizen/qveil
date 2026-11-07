import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

FocusScope {
    id: page

    signal channelRequested(string id)

    function submit() {
        if (add.text.trim() === "")
            return
        backend.subscribe(add.text)
        add.text = ""
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: 28
        anchors.rightMargin: 28
        anchors.topMargin: 22
        spacing: 10

        Label {
            text: qsTr("Subscriptions")
            font.pixelSize: 26
            font.weight: Font.DemiBold
        }
        Label {
            Layout.fillWidth: true
            text: qsTr("Kept only on this computer. Each channel is fetched on its own, in random order, so YouTube never sees the whole list.")
            wrapMode: Text.Wrap
            font.pixelSize: 13
            color: "#8d8d98"
        }

        RowLayout {
            Layout.topMargin: 6
            spacing: 10
            TextField {
                id: add
                focus: true
                Layout.preferredWidth: 380
                placeholderText: qsTr("@handle, channel URL or ID")
                onAccepted: page.submit()
            }
            Button {
                text: qsTr("Subscribe")
                highlighted: true
                enabled: add.text.trim() !== ""
                onClicked: page.submit()
            }
        }

        Label {
            Layout.fillWidth: true
            text: qsTr("Moving from YouTube? Drop your Google Takeout subscriptions.csv, or a NewPipe or FreeTube export, anywhere on this window. It's read offline.")
            wrapMode: Text.Wrap
            font.pixelSize: 12
            color: "#6f6f7b"
        }

        Label {
            Layout.topMargin: 8
            text: qsTr("%n channels", "", backend.subs.length)
            font.pixelSize: 12
            font.weight: Font.DemiBold
            color: "#a2a2ad"
        }

        ListView {
            id: list
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: backend.subs
            boundsBehavior: Flickable.StopAtBounds
            ScrollBar.vertical: ScrollBar {}

            delegate: ItemDelegate {
                id: row
                required property var modelData
                width: ListView.view.width
                height: 50
                onClicked: page.channelRequested(modelData.id)

                contentItem: RowLayout {
                    spacing: 12
                    Rectangle {
                        Layout.preferredWidth: 30
                        Layout.preferredHeight: 30
                        radius: 15
                        color: "#22222a"
                        Label {
                            anchors.centerIn: parent
                            text: row.modelData.name.charAt(0).toUpperCase()
                            font.weight: Font.DemiBold
                            color: theme.accent
                        }
                    }
                    Label {
                        Layout.fillWidth: true
                        text: row.modelData.name
                        elide: Text.ElideRight
                        font.pixelSize: 14
                    }
                    Button {
                        flat: true
                        text: qsTr("Unsubscribe")
                        onClicked: backend.unsubscribe(row.modelData.id)
                    }
                }
            }
        }
    }
}
