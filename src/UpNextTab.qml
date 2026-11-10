import QtQuick
import Qveil
import "Format.js" as Format

// What comes after this video in the list it was opened from (the feed, a
// lens, a search). It is already here, so showing it asks for nothing.
ListView {
    id: tab

    // A VideoModel, or null when the video wasn't opened from a list.
    property var list: null
    // Where in the list the video that is playing is.
    property int index: -1
    readonly property var entries: {
        const out = []
        if (list && index >= 0)
            for (let i = index + 1; i < list.count && out.length < 40; i++)
                out.push({ row: i, video: list.get(i) })
        return out
    }
    signal picked(var video, int row)

    model: entries
    clip: true
    boundsBehavior: Flickable.StopAtBounds

    delegate: Rectangle {
        required property var modelData
        readonly property string title: modelData.video.title || ""
        objectName: "next-entry"
        width: tab.width
        height: 52
        color: area.containsMouse ? Style.hover : "transparent"

        Column {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: Style.u * 3
            anchors.rightMargin: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Text {
                width: parent.width
                text: title
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.small
                font.bold: true
            }
            Text {
                width: parent.width
                text: [modelData.video.channel || "", modelData.video.duration ? Format.clock(modelData.video.duration) : ""]
                          .filter(s => s !== "").join("  ·  ")
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
        }
        Rule {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
        }
        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: tab.picked(modelData.video, modelData.row)
        }
    }

    Text {
        anchors.centerIn: parent
        width: parent.width - Style.u * 8
        visible: tab.count === 0
        text: qsTr("Nothing follows: this video wasn't opened from a list, or it was the last in it.")
        textFormat: Text.PlainText
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
}
