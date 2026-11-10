import QtQuick
import Qveil

// Said once, the first time a position is kept: that it is, in the engine's
// words, and where that is set. It stays until it is answered.
Rectangle {
    id: notice
    objectName: "resume-notice"

    property bool shown: false
    signal settingsRequested()

    visible: shown
    height: column.height + Style.u * 6
    radius: Style.radius
    color: Style.bg
    border.width: 1
    border.color: Style.line

    // The fill is drawn over the page's own colour, so the text can be read.
    Rectangle {
        anchors.fill: parent
        anchors.margins: 1
        radius: parent.radius
        color: Style.hover
    }
    Column {
        id: column
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: Style.u * 4
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u * 2

        Text {
            objectName: "resume-notice-text"
            width: parent.width
            text: backend.privacy.pages.resume || ""
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            lineHeight: 1.25
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.small
        }
        Row {
            anchors.right: parent.right
            spacing: Style.u
            Act {
                objectName: "resume-notice-settings"
                text: qsTr("Settings")
                filled: true
                onClicked: {
                    notice.shown = false
                    notice.settingsRequested()
                }
            }
            Act {
                objectName: "resume-notice-ok"
                text: qsTr("OK")
                onClicked: notice.shown = false
            }
        }
    }
}
