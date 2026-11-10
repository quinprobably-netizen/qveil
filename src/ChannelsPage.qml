import QtQuick
import Qveil

// The channels you follow: add one, open one, let one go.
FocusScope {
    id: page

    // Where focus belongs when the page is shown.
    readonly property Item content: list
    signal channelRequested(string id)

    function focusAdd() { add.edit() }
    function submit(text) {
        backend.subscribe(text)
        add.text = ""
    }

    Column {
        id: top
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        width: Math.min(parent.width - 2 * Style.gutter, 720)
        spacing: Style.u * 3

        Text {
            text: qsTr("Channels")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.head
            font.weight: Style.strong
        }
        // What following a channel means, in the engine's words.
        Notice {
            objectName: "channels-note"
            width: parent.width
            text: backend.privacy.pages.subscriptions || ""
        }
        Row {
            spacing: Style.u * 2
            Field {
                id: add
                objectName: "channel-add"
                width: Math.min(380, top.width - 120)
                placeholder: qsTr("@handle, channel link or ID")
                onSubmitted: text => page.submit(text)
                onEscaped: list.forceActiveFocus()
            }
            Act {
                text: qsTr("Subscribe")
                filled: true
                enabled: add.text.trim() !== ""
                onClicked: page.submit(add.text.trim())
            }
        }
        Notice {
            width: parent.width
            color: Style.faint
            text: qsTr("Moving from YouTube? Drop your Google Takeout subscriptions.csv, or a NewPipe or FreeTube export, anywhere on this window.")
        }
        SectionHead {
            width: parent.width
            text: qsTr("%n channels", "", backend.subs.length)
        }
    }

    ListView {
        id: list
        anchors.left: top.left
        anchors.right: top.right
        anchors.top: top.bottom
        anchors.bottom: parent.bottom
        anchors.topMargin: Style.u
        focus: true
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: backend.subs
        Keys.onReturnPressed: if (currentItem) page.channelRequested(currentItem.modelData.id)

        delegate: Rectangle {
            id: row
            required property var modelData
            required property int index
            // Whether Unsubscribe has been pressed once, and waits for the second.
            property bool arming: false
            readonly property bool lit: area.containsMouse || (ListView.isCurrentItem && list.activeFocus)
            objectName: "channel-row"
            width: list.width
            height: Style.control + Style.u * 3
            color: lit ? Style.hover : "transparent"

            MouseArea {
                id: area
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: page.channelRequested(row.modelData.id)
                onExited: row.arming = false
            }
            Text {
                anchors.left: parent.left
                anchors.leftMargin: Style.u * 3
                anchors.right: leave.left
                anchors.verticalCenter: parent.verticalCenter
                text: row.modelData.name
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.body
            }
            Act {
                id: leave
                objectName: "unsubscribe"
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                opacity: row.lit || hot || row.arming ? 1 : 0
                text: row.arming ? qsTr("Unsubscribe?") : qsTr("Unsubscribe")
                quiet: !row.arming
                onClicked: {
                    if (row.arming)
                        backend.unsubscribe(row.modelData.id)
                    row.arming = !row.arming
                }
            }
            Rule {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
            }
        }
    }
}
