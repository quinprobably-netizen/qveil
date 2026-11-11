import QtQuick
import Qveil
import "Format.js" as Format
import "Linkify.js" as Linkify

// Watching: the video, what it is, and beside it a sheet with its
// description and chapters, its comments, and what comes next.
FocusScope {
    id: page
    objectName: "watch"

    // The list the video was opened from, where in it, and in words where it came from.
    property var list: null
    property int index: -1
    property string from: ""
    property string thumbs: "frosted"
    property real savedVolume: 80
    // The video takes the width, and the sheet steps aside.
    property bool theatre: false
    // The video takes the whole window.
    property bool full: false
    property bool hold: false
    // Whether the video has left for the small window.
    property bool away: false
    // The surface that plays the video: this page's own, or the small
    // window's in picture in picture.
    property var playing: null
    readonly property alias surface: surface
    readonly property alias sheet: sheet
    readonly property bool active: surface.active
    readonly property var info: surface.info
    // The sheet stands beside the video when the window is wide, and also
    // when the video is held down by the window's height anyway and the
    // width it leaves is enough: then the sheet costs the picture nothing (a
    // quarter tile). Only where neither holds (a narrow column) is it under.
    readonly property real stackedHeight: Math.min(Math.round((width - 2 * Style.gutter) * 9 / 16), Math.max(200, height - 190))
    readonly property bool narrow: !(width >= 1000
        || width - 2 * Style.gutter - Math.round(stackedHeight * 16 / 9) >= sheetWidth + Style.gutter)
    // Whether the keyboard is in the sheet, and not the video's.
    readonly property bool inSheet: sheet.keyed
    readonly property bool beside: !narrow && !theatre && !full
    // What is kept under the video for what it is: the title and the
    // channel's row (in theatre, the title only). The rest (the description)
    // is below, a scroll away; the picture takes what is left.
    readonly property int keptUnder: column.y + column.spacing + Style.head + 8
        + (theatre ? 0 : column.spacing + Style.control) + Style.gutter
    // How wide the sheet is when it stands beside the video.
    readonly property int sheetWidth: width >= 1180 ? 384 : 320
    readonly property bool subscribed: !!info.channelId && backend.subs.some(s => s.id === info.channelId)

    signal closed()
    signal channelRequested(string id)
    signal videoRequested(var video, var list, int index)
    signal pipRequested()
    signal fullscreenToggled()
    signal copied(string text)

    function prepare(video) {
        // A new video has the keyboard in the video, wherever it was last.
        surface.forceActiveFocus()
        // A lookup of the last video's comments or chapters is for nobody now.
        backend.closeComments()
        backend.closeChapters()
        expanded = false
        sheet.current = "next"
        ahead.reset(video.id)
        surface.prepare(video)
        flick.contentY = 0
    }
    function open(i) { surface.open(i) }
    // Takes over the video the small window's surface `from` is playing,
    // with its player (see PlayerSurface.adopt).
    function adopt(from) {
        if (!surface.adopt(from))
            return false
        surface.forceActiveFocus()
        backend.closeComments()
        backend.closeChapters()
        expanded = false
        sheet.current = "next"
        flick.contentY = 0
        return true
    }
    // The sheet is hidden in theatre and fullscreen: the keyboard can't stay in it.
    onTheatreChanged: if (theatre && inSheet) leaveSheet()
    onFullChanged: if (full && inSheet) leaveSheet()
    // Gives the keyboard to the sheet, on one of its tabs; and back to the video.
    function enterSheet(tab) {
        theatre = false
        if (full)
            return
        sheet.current = tab === "chapters" && !sheet.hasChapters ? "next" : tab
        sheet.forceActiveFocus()
        // Under the video, it is brought into view.
        if (narrow)
            flick.contentY = Math.max(0, Math.min(flick.contentHeight - flick.height, below.y + column.y - Style.u * 2))
    }
    function leaveSheet() {
        surface.forceActiveFocus()
        if (narrow)
            flick.contentY = 0
    }
    // The video after this one in the list it was opened from.
    function playNext() {
        if (list && index >= 0 && index + 1 < list.count)
            page.videoRequested(list.get(index + 1), list, index + 1)
    }
    function close() {
        // A lookup of comments or chapters that is still running is for nobody now.
        backend.closeComments()
        backend.closeChapters()
        ahead.reset()
        surface.stop()
    }

    // Where a link in a description or a comment leads: back into the app,
    // or (for another site) nowhere but the clipboard.
    function follow(link) {
        const m = /^qveil:(time|video|channel|copy)\/(.+)$/.exec(String(link))
        if (!m)
            return
        const what = decodeURIComponent(m[2])
        if (m[1] === "time")
            surface.seekTo(Number(what))
        else if (m[1] === "video")
            page.videoRequested({ id: what }, null, -1)
        else if (m[1] === "channel")
            page.channelRequested(what)
        else
            page.copied(what)
    }

    property bool expanded: false

    Flickable {
        id: flick
        anchors.fill: parent
        contentHeight: column.y + column.height + Style.gutter
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: !page.full

        Column {
            id: column
            x: Style.gutter
            y: Style.u * 5
            width: (page.beside ? page.width - page.sheetWidth - Style.gutter : page.width) - 2 * Style.gutter
            spacing: Style.u * 3

            // Where the video sits in the page. (In fullscreen it is laid
            // over the whole page instead: see `stage`.)
            Item {
                id: slot
                width: parent.width
                // 16:9, but never so tall that the title is pushed off the page.
                height: Math.min(Math.round(width * 9 / 16), Math.max(200, page.height - page.keptUnder))
            }
            Text {
                objectName: "watch-title"
                width: parent.width
                text: page.info.title || ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.head
                font.weight: Style.strong
            }
            Item {
                width: parent.width
                height: Style.control
                Row {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u * 3
                    Text {
                        objectName: "watch-channel"
                        anchors.verticalCenter: parent.verticalCenter
                        text: page.info.channel || ""
                        textFormat: Text.PlainText
                        color: channelArea.containsMouse ? Style.accent : Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.body
                        font.weight: Style.strong
                        MouseArea {
                            id: channelArea
                            anchors.fill: parent
                            enabled: !!page.info.channelId
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: page.channelRequested(page.info.channelId)
                        }
                    }
                    Act {
                        visible: !!page.info.channelId
                        text: page.subscribed ? qsTr("Subscribed") : qsTr("Subscribe")
                        glyph: page.subscribed ? "qrc:/icons/check.svg" : ""
                        filled: true
                        onClicked: page.subscribed ? backend.unsubscribe(page.info.channelId)
                                                   : backend.subscribe(page.info.channelId)
                    }
                }
                Text {
                    objectName: "watch-stats"
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    visible: text !== ""
                    text: Format.meta(page.info, page.info.published || 0)
                    textFormat: Text.PlainText
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
            SectionHead {
                width: parent.width
                visible: about.visible
                text: qsTr("About")
                Act {
                    text: page.expanded ? qsTr("Less") : qsTr("More")
                    quiet: true
                    visible: about.truncated || page.expanded
                    onClicked: page.expanded = !page.expanded
                }
            }
            Text {
                id: about
                objectName: "watch-about"
                width: parent.width
                visible: !!page.info.description
                text: Linkify.html(page.info.description || "", Style.accent)
                textFormat: Text.StyledText
                wrapMode: Text.Wrap
                maximumLineCount: page.expanded ? 2000 : 3
                elide: Text.ElideRight
                lineHeight: 1.25
                color: Style.dim
                linkColor: Style.accent
                font.family: Style.font
                font.pixelSize: Style.small
                onLinkActivated: link => page.follow(link)
            }
            // The sheet's place when it can't stand beside the video.
            Item {
                id: below
                width: parent.width
                height: page.narrow && !page.theatre && !page.full ? 460 : 0
            }
        }
    }

    SideSheet {
        id: sheet
        parent: page.beside ? page : below
        visible: !page.theatre && !page.full
        x: page.beside ? page.width - width - Style.gutter : 0
        y: page.beside ? Style.u * 5 : 0
        width: page.beside ? page.sheetWidth : below.width
        height: page.beside ? page.height - Style.u * 5 - Style.gutter : below.height
        commentCount: comments.count
        hasChapters: surface.chapters.length > 0 || !!page.info.chaptersLater

        chapters: ChaptersTab {
            anchors.fill: parent
            video: page.info.chaptersLater ? (page.info.id || "") : ""
            active: sheet.visible && sheet.current === "chapters" && page.active
            chapters: surface.chapters
            current: bar.chapter
            keyed: sheet.keyed
            onChapterPicked: seconds => surface.seekTo(seconds)
        }
        comments: CommentsTab {
            id: comments
            anchors.fill: parent
            keyed: sheet.keyed
            video: page.info.id || ""
            active: sheet.visible && sheet.current === "comments" && page.active
            onLinkActivated: link => page.follow(link)
        }
        next: UpNextTab {
            anchors.fill: parent
            keyed: sheet.keyed
            thumbs: page.thumbs
            list: page.list
            index: page.index
            from: page.from
            onPicked: (video, row) => page.videoRequested(video, page.list, row)
        }
    }

    // The video: in its place in the page, or over all of it in fullscreen.
    Item {
        id: stage
        parent: page.full ? page : slot
        anchors.fill: parent
        z: page.full ? 10 : 0

        PlayerSurface {
            id: surface
            objectName: "surface"
            anchors.fill: parent
            focus: true
            thumbs: page.thumbs
            hold: page.hold
            savedVolume: page.savedVolume
            onSavedVolumeChanged: page.savedVolume = savedVolume
            nextVideo: page.list && page.index >= 0 && page.index + 1 < page.list.count ? page.list.get(page.index + 1) : null
            onNextRequested: page.playNext()
            onClosed: page.closed()
            onPipRequested: page.pipRequested()
            onTheatreToggled: page.theatre = !page.theatre
            onFullscreenToggled: page.fullscreenToggled()
        }
        // Where the video went.
        Rectangle {
            anchors.fill: parent
            visible: page.away
            color: "black"
            Text {
                anchors.centerIn: parent
                text: qsTr("Playing in picture in picture")
                textFormat: Text.PlainText
                color: Style.dim
                font.family: Style.font
                font.pixelSize: Style.body
            }
        }
    }

    // "Get the next video ready": in the last 30 seconds of a video played
    // from a list, the next one in it is looked up once, so that Next plays
    // it at once. Only the clock starts it, never pointing at a card. The
    // clock is that of whichever surface plays the video: in picture in
    // picture, the small window's.
    QtObject {
        id: ahead
        readonly property var next: page.list && page.index >= 0 && page.index + 1 < page.list.count
            ? page.list.get(page.index + 1) : null
        readonly property var clock: page.playing || surface
        readonly property bool due: !!backend.config.prepareNext && !!next && !!next.id
            && clock.length > 0 && clock.length - clock.mpv.position <= 30
        // The video that was asked for, so it is asked for once.
        property string asked: ""
        onDueChanged: consider()
        function consider() {
            if (due && next.id !== asked) {
                asked = next.id
                backend.prepare(asked)
            }
        }
        // A new video is being opened. The lookup of the one that was asked
        // for is let go of, unless that is the video: the engine plays it
        // from that lookup, once it is done.
        function reset(opening) {
            if (asked !== "" && asked !== opening)
                backend.cancelPrepare()
            asked = ""
        }
    }
    Connections {
        target: backend
        // What was got ready went with the old route (the engine ended it),
        // so the next video is asked for again, on the new one. Once the
        // window has reopened what plays, which can reset it too.
        function onRouteChanged() {
            ahead.asked = ""
            Qt.callLater(ahead.consider)
        }
    }

    // The chapter that is playing, for the list in the sheet.
    SeekBar {
        id: bar
        visible: false
        position: surface.mpv.position
        length: surface.length
        chapters: surface.chapters
    }
}
