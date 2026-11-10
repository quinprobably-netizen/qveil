import QtQuick
import Qveil
import "Format.js" as Format
import "Linkify.js" as Linkify

// The whole description, and the video's chapters as a list.
Flickable {
    id: tab

    property string description
    // [{ start, title }]
    property var chapters: []
    // The chapter that is playing, from 0.
    property int current: -1
    signal linkActivated(string link)
    signal chapterPicked(real seconds)

    contentHeight: column.height + Style.u * 6
    clip: true
    boundsBehavior: Flickable.StopAtBounds

    Column {
        id: column
        x: Style.u * 3
        y: Style.u * 3
        width: tab.width - Style.u * 6
        spacing: Style.u * 4

        Text {
            width: parent.width
            visible: tab.description !== ""
            text: Linkify.html(tab.description, Style.accent)
            textFormat: Text.StyledText
            wrapMode: Text.Wrap
            lineHeight: 1.25
            color: Style.fg
            linkColor: Style.accent
            font.family: Style.font
            font.pixelSize: Style.small
            onLinkActivated: link => tab.linkActivated(link)
            HoverHandler { cursorShape: parent.hoveredLink ? Qt.PointingHandCursor : Qt.ArrowCursor }
        }
        Text {
            width: parent.width
            visible: tab.description === "" && tab.chapters.length === 0
            text: qsTr("This video has no description.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.small
        }
        Column {
            width: parent.width
            visible: tab.chapters.length > 0
            spacing: 0
            SectionHead {
                width: parent.width
                text: qsTr("Chapters")
            }
            Repeater {
                model: tab.chapters
                delegate: Rectangle {
                    required property var modelData
                    required property int index
                    objectName: "chapter"
                    width: column.width
                    height: Style.control
                    color: area.containsMouse ? Style.hover : "transparent"
                    Text {
                        id: at
                        anchors.left: parent.left
                        anchors.leftMargin: Style.u
                        anchors.verticalCenter: parent.verticalCenter
                        width: 56
                        text: Format.clock(modelData.start)
                        textFormat: Text.PlainText
                        color: index === tab.current ? Style.accent : Style.faint
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    Text {
                        anchors.left: at.right
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: modelData.title
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: index === tab.current ? Style.fg : Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                        font.weight: (index === tab.current) ? Style.strong : Font.Normal
                    }
                    MouseArea {
                        id: area
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: tab.chapterPicked(modelData.start)
                    }
                }
            }
        }
    }
}
