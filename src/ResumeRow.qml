import QtQuick
import Qveil
import "Format.js" as Format

// The videos that were left part-way, newest first, with how much of each is
// left. Clicking one picks it up where it was stopped.
Column {
    id: row

    // [{ id, pos, dur, title, channel }]
    property var entries: []
    // How long positions are kept, in days.
    property int days: 7
    // How many fit across.
    readonly property int across: Math.max(1, Math.min(3, Math.floor(width / 300)))
    readonly property string note: qsTr("kept %n days, then deleted", "", days)
    signal picked(var video)
    signal forgotten(string id)

    visible: entries.length > 0
    spacing: Style.u * 2

    SectionHead {
        width: parent.width
        text: qsTr("Pick up where you stopped")
        Text {
            text: row.note
            textFormat: Text.PlainText
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.small
        }
    }
    Row {
        width: parent.width
        spacing: Style.u * 4

        Repeater {
            model: row.entries.slice(0, row.across)
            delegate: Rectangle {
                id: entry
                objectName: "resume-entry"
                required property var modelData
                readonly property string title: modelData.title || ""
                readonly property string remaining: qsTr("%1 left").arg(Format.clock(Math.max(0, modelData.dur - modelData.pos)))

                width: (row.width - (row.across - 1) * Style.u * 4) / row.across
                height: 86
                color: area.containsMouse ? Style.hover : Style.rest
                Behavior on color { ColorAnimation { duration: Style.quick } }

                MouseArea {
                    id: area
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: row.picked({ id: entry.modelData.id, title: entry.title, channel: entry.modelData.channel || "" })
                }
                Item {
                    id: picture
                    x: 7
                    y: 7
                    width: 128
                    height: 72
                    clip: true
                    Frost {
                        anchors.fill: parent
                        source: Format.thumb(backend.thumbBase, entry.modelData.id, 0)
                        clear: area.containsMouse
                        mode: win.thumbs
                    }
                    Rectangle {
                        anchors.left: parent.left
                        anchors.bottom: parent.bottom
                        width: parent.width * Math.min(1, entry.modelData.pos / Math.max(1, entry.modelData.dur))
                        height: 2
                        color: Style.accent
                    }
                }
                Column {
                    anchors.left: picture.right
                    anchors.leftMargin: Style.u * 3
                    anchors.right: parent.right
                    anchors.rightMargin: Style.u * 2
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
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
                        font.bold: true
                    }
                    Text {
                        width: parent.width
                        text: entry.modelData.channel || ""
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    Text {
                        text: entry.remaining
                        textFormat: Text.PlainText
                        color: Style.faint
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
                Act {
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    visible: area.containsMouse || hot
                    text: qsTr("Forget")
                    quiet: true
                    onClicked: row.forgotten(entry.modelData.id)
                }
            }
        }
    }
}
