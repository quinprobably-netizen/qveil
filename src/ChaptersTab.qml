import QtQuick
import Qveil
import "Format.js" as Format
import "Linkify.js" as Linkify

// The video's chapters as a list. (Its description is under the video, once.)
// Those its description doesn't name are looked up when this is first shown
// for a video, and never before: it costs a lookup of its own.
Flickable {
    id: tab

    property string description
    // [{ start, title }]
    property var chapters: []
    // The video's ID, when its chapters are to be looked up; empty when the
    // play's answer had them all.
    property string video
    // Whether the tab is the one showing.
    property bool active: false
    readonly property bool loaded: video === "" || backend.chapters.video === video
    readonly property bool loading: backend.busy.indexOf("chapters") >= 0
    // The video this tab last asked about.
    property string asked
    function load() {
        if (video !== "" && !loaded) {
            asked = video
            backend.loadChapters(video)
        }
    }
    onVideoChanged: if (active) load()
    onActiveChanged: if (active && !loading) load()
    // A lookup for another video was still running when this was shown, so
    // nothing was asked: it is now. (A lookup of this video that failed is
    // not asked again by itself.)
    onLoadingChanged: if (!loading && active && asked !== video) load()
    // The chapter that is playing, from 0.
    property int current: -1
    signal linkActivated(string link)
    signal chapterPicked(real seconds)
    // Whether the keyboard is in the sheet, and which chapter it is on.
    property bool keyed: false
    property int at: 0
    onChaptersChanged: at = Math.max(0, current)
    function step(by) {
        at = Math.max(0, Math.min(chapters.length - 1, at + by))
        // Keeps the row in view.
        const y = Style.u * 3 + Style.u * 6 + at * Style.control
        if (y < contentY)
            contentY = Math.max(0, y - Style.control)
        else if (y + Style.control > contentY + height)
            contentY = y + Style.control - height + Style.u * 2
    }
    function activate() {
        if (at >= 0 && at < chapters.length)
            chapterPicked(chapters[at].start)
    }

    contentHeight: column.height + Style.u * 6
    clip: true
    boundsBehavior: Flickable.StopAtBounds

    Column {
        id: column
        x: Style.u * 3
        y: Style.u * 3
        width: tab.width - Style.u * 6
        spacing: Style.u * 4

        BusyLine {
            width: parent.width
            visible: tab.loading && !tab.loaded
        }
        Text {
            objectName: "chapters-empty"
            width: parent.width
            visible: tab.video !== "" && tab.loaded && !tab.loading && tab.chapters.length === 0
            text: qsTr("This video has no chapters.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: Style.dim
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
                    // The keyboard's cursor.
                    border.width: tab.keyed && index === tab.at ? 2 : 0
                    border.color: Style.accent
                    Text {
                        id: stamp
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
                        anchors.left: stamp.right
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
