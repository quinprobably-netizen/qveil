import QtQuick
import Qveil

// What is playing in the small window, along the foot of the main one, with
// the way to bring it back.
Rectangle {
    id: bar
    objectName: "now-playing"

    // The player in the small window.
    property var surface: null
    readonly property var info: surface ? surface.info : ({})
    readonly property string title: info.title || ""
    signal backRequested()
    signal closeRequested()

    implicitHeight: Style.control + Style.u * 4
    color: Style.bg

    Rule {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
    }
    Rectangle {
        anchors.fill: parent
        color: Style.rest
    }
    Row {
        id: words
        anchors.left: parent.left
        anchors.leftMargin: Style.gutter
        anchors.right: acts.left
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u * 3
        clip: true
        Glyph {
            anchors.verticalCenter: parent.verticalCenter
            source: "qrc:/icons/pip.svg"
            size: 15
            color: Style.accent
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: bar.title
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.body
            font.bold: true
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: bar.info.channel || ""
            textFormat: Text.PlainText
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.small
        }
    }
    Row {
        id: acts
        anchors.right: parent.right
        anchors.rightMargin: Style.gutter - Style.u * 2
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u
        Act {
            glyph: bar.surface && (bar.surface.mpv.paused || bar.surface.mpv.ended) ? "qrc:/icons/play.svg" : "qrc:/icons/pause.svg"
            onClicked: bar.surface.mpv.togglePause()
        }
        Act {
            objectName: "now-back"
            text: qsTr("Back to qveil")
            filled: true
            onClicked: bar.backRequested()
        }
        Act {
            objectName: "now-close"
            text: qsTr("Close")
            onClicked: bar.closeRequested()
        }
    }
}
