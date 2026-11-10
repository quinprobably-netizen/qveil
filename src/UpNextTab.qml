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
    // "frosted" or "clear": how thumbnails are shown (see Frost).
    property string thumbs: "clear"
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
        id: entry
        required property var modelData
        readonly property string title: modelData.video.title || ""
        objectName: "next-entry"
        width: tab.width
        height: 92
        color: area.containsMouse ? Style.hover : "transparent"

        Item {
            id: picture
            x: Style.u * 2
            anchors.verticalCenter: parent.verticalCenter
            width: 136
            height: 76
            clip: true
            Rectangle {
                anchors.fill: parent
                color: Style.rest
            }
            // The engine has these from the list the video was opened from.
            Frost {
                objectName: "frost"
                anchors.fill: parent
                source: Format.thumb(backend.thumbBase, entry.modelData.video.id, entry.modelData.video.batch || 0)
                clear: area.containsMouse
                mode: tab.thumbs
            }
            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                visible: clock.text !== ""
                width: clock.implicitWidth + Style.u * 2
                height: clock.implicitHeight + 2
                color: Style.scrim
                Text {
                    id: clock
                    anchors.centerIn: parent
                    text: entry.modelData.video.duration ? Format.clock(entry.modelData.video.duration) : ""
                    textFormat: Text.PlainText
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
            }
        }
        Column {
            anchors.left: picture.right
            anchors.right: parent.right
            anchors.leftMargin: Style.u * 3
            anchors.rightMargin: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            spacing: 3
            Text {
                width: parent.width
                text: entry.title
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.body
                font.weight: Style.strong
            }
            Text {
                width: parent.width
                visible: text !== ""
                text: entry.modelData.video.channel || ""
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.dim
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Text {
                width: parent.width
                visible: text !== ""
                text: Format.meta(entry.modelData.video, entry.modelData.video.date || entry.modelData.video.published || 0)
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
        }
        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: tab.picked(entry.modelData.video, entry.modelData.row)
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
