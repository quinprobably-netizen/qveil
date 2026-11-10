import QtQuick
import Qveil
import "Format.js" as Format

// The video and its controls. It is shown in the watch page, and again in
// the small window of picture in picture: each has one of its own, and only
// one of them is playing at a time.
FocusScope {
    id: surface

    // What is playing: the engine's play answer, or before that what the
    // card knew (id, title, channel).
    property var info: ({})
    // Opened on a click, before the engine has looked the video up.
    property bool pending: false
    property bool active: false
    // Whether the controls are showing.
    property bool chrome: true
    // True in the small window, which has room for less.
    property bool compact: false
    // True while something outside is in use, so the controls stay.
    property bool hold: false
    property real savedVolume: 80
    property alias mpv: mpv
    property string thumbs: "frosted"
    // The second it last started a video at (0 from the start), and the
    // second it was last sent to: what the window asked of the player.
    property real startedAt: 0
    property real lastSeek: -1
    readonly property var chapters: info.chapters || []
    // What the player says once it knows; until then, what the engine said.
    readonly property real length: mpv.duration > 0 ? mpv.duration : (info.duration || 0)

    signal closed()
    signal pipRequested()
    signal theatreToggled()
    signal fullscreenToggled()

    // Shows the player for `video` (a card's data) at once, while the engine
    // looks it up; `open` follows with the streams.
    function prepare(video) {
        report()
        if (mpv.loaded)
            mpv.stop()
        info = {
            id: video.id,
            title: video.title || "",
            channel: video.channel || "",
            channelId: video.channel_id || video.channelId || "",
            // The card's page, so the poster is the thumbnail it already has.
            batch: video.batch || 0
        }
        note.shown = false
        startedAt = 0
        lastSeek = -1
        pending = true
        active = true
        reveal()
    }

    function open(i) { openAt(i, undefined) }

    // `at`: where to start instead of where the answer says (a video picked
    // up from the other window carries on where that one was).
    function openAt(i, at) {
        pending = false
        info = Object.assign({ batch: info.batch || 0 }, i)
        active = true
        const resumed = at === undefined && !(i.start > 0) && i.resume > 0
        startedAt = at !== undefined ? at : (i.start > 0 ? i.start : (i.resume > 0 ? i.resume : 0))
        lastSeek = -1
        note.shown = resumed
        if (resumed)
            noteTimer.restart()
        mpv.volume = savedVolume
        // A video without a stream has nothing to load (and makes no sound).
        if (i.video)
            mpv.load(i.video, i.audio || "", startedAt)
        reveal()
    }

    // Stops the video and remembers where it was.
    function stop() {
        const was = active
        report()
        if (pending)
            backend.cancelPlay()
        pending = false
        mpv.stop()
        active = false
        info = ({})
        note.shown = false
        if (was)
            closed()
    }

    function seekTo(seconds) {
        lastSeek = seconds
        note.shown = false
        mpv.seek(seconds, false)
        reveal()
    }

    // To the start of the chapter after this one, or (back) of this one,
    // and from its first seconds of the one before.
    function chapterStep(forward) {
        if (chapters.length === 0)
            return
        const now = bar.chapter
        if (forward && now + 1 < chapters.length)
            seekTo(chapters[now + 1].start)
        else if (!forward)
            seekTo(chapters[Math.max(0, mpv.position - chapters[Math.max(0, now)].start < 3 ? now - 1 : now)].start)
    }

    function speedStep(faster) {
        const steps = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2]
        let at = 0
        for (let i = 0; i < steps.length; i++)
            if (Math.abs(steps[i] - mpv.speed) < Math.abs(steps[at] - mpv.speed))
                at = i
        mpv.speed = steps[Math.max(0, Math.min(steps.length - 1, at + (faster ? 1 : -1)))]
        reveal()
    }

    function reveal() {
        chrome = true
        hideTimer.restart()
    }

    // Tells the engine where the video is, so that it can be picked up
    // again. Nothing is said before the player knows the length.
    function report() {
        if (active && !pending && info.id && mpv.duration > 0)
            backend.noteResume(info, mpv.position, mpv.duration)
    }

    onHoldChanged: if (!hold) reveal()

    Rectangle {
        anchors.fill: parent
        color: "black"
    }

    MpvItem {
        id: mpv
        anchors.fill: parent
        backgroundColor: "black"
        onFailed: message => backend.reportError(message)
        onVolumeChanged: surface.savedVolume = volume
        onPausedChanged: {
            surface.reveal()
            if (paused)
                surface.report()
        }
        onEndedChanged: if (ended) surface.report()
    }

    // While it plays, every so often.
    Timer {
        interval: 15000
        repeat: true
        running: surface.active && !surface.pending && !mpv.paused && !mpv.ended
        onTriggered: surface.report()
    }

    // The video's thumbnail until its first frame: the engine has it from
    // the grid, so this is no new request for anything not already shown.
    Image {
        anchors.fill: parent
        fillMode: Image.PreserveAspectFit
        asynchronous: true
        source: surface.active ? Format.thumb(backend.thumbBase, surface.info.id, surface.info.batch) : ""
        opacity: surface.active && !mpv.started ? 0.5 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 180 } }
    }

    Column {
        anchors.centerIn: parent
        visible: surface.pending || (surface.active && mpv.buffering && !mpv.started)
        spacing: Style.u * 2
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: backend.config.route === "tor" ? qsTr("Opening the video over Tor…") : qsTr("Opening the video…")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.body
        }
        BusyLine {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 160
        }
    }

    // A lookup that fails says why in the window's error, and leaves nothing to show.
    Connections {
        target: backend
        function onTaskFailed(task) {
            if (task === "play" && surface.pending)
                surface.stop()
        }
        // mpv holds a port that nothing listens on once the engine is gone,
        // and a restarted engine has a new one and a new token, so a video
        // that was playing is lost too, not only one still being looked up.
        function onReadyChanged() {
            if (!backend.ready && surface.active)
                surface.stop()
        }
    }

    Timer {
        id: hideTimer
        interval: 2600
        onTriggered: {
            if (!mpv.paused && !mpv.ended && !bar.pressed && !controlsHover.hovered && !surface.hold && mpv.started)
                surface.chrome = false
        }
    }

    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: surface.chrome ? Qt.ArrowCursor : Qt.BlankCursor
        onPositionChanged: surface.reveal()
        onClicked: mpv.togglePause()
        onDoubleClicked: surface.fullscreenToggled()
    }

    // Where it was picked up, and the way back to the start.
    Rectangle {
        id: note
        property bool shown: false
        objectName: "resumed"
        readonly property string text: qsTr("Resumed at %1").arg(Format.clock(surface.startedAt))
        visible: shown && surface.active && !surface.compact
        width: noteRow.implicitWidth + Style.u * 3
        height: Style.control
        color: Style.scrim
        Row {
            id: noteRow
            anchors.left: parent.left
            anchors.leftMargin: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.u
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: note.text
                textFormat: Text.PlainText
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Act {
                objectName: "start-over"
                text: qsTr("Start over")
                onClicked: surface.seekTo(0)
            }
        }
        Timer {
            id: noteTimer
            interval: 8000
            onTriggered: note.shown = false
        }
    }

    Act {
        anchors.centerIn: parent
        visible: mpv.ended
        text: qsTr("Replay")
        glyph: "qrc:/icons/replay.svg"
        filled: true
        onClicked: mpv.togglePause()
    }

    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: controls.height + Style.u * 12
        opacity: surface.chrome && surface.active ? 1 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 220 } }
        gradient: Gradient {
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 1; color: "#e0000000" }
        }

        HoverHandler { id: controlsHover }

        Column {
            id: controls
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.leftMargin: Style.u * 3
            anchors.rightMargin: Style.u * 3
            anchors.bottomMargin: Style.u
            spacing: 0

            SeekBar {
                id: bar
                width: parent.width
                position: mpv.position
                length: surface.length
                cached: mpv.cached
                chapters: surface.chapters
                onMoved: seconds => surface.seekTo(seconds)
            }
            Item {
                width: parent.width
                height: Style.control + Style.u
                Row {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u * 2
                    Act {
                        glyph: mpv.paused || mpv.ended ? "qrc:/icons/play.svg" : "qrc:/icons/pause.svg"
                        onClicked: mpv.togglePause()
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: bar.time
                        textFormat: Text.PlainText
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    Text {
                        objectName: "seek-chapter"
                        anchors.verticalCenter: parent.verticalCenter
                        visible: text !== "" && !surface.compact
                        text: bar.chapterText
                        textFormat: Text.PlainText
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
                Row {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u
                    Act {
                        visible: !surface.compact
                        text: (Math.round(mpv.speed * 100) / 100) + "x"
                        tip: qsTr("Speed (< >)")
                        onClicked: mpv.speed = mpv.speed >= 2 ? 0.75 : mpv.speed < 1 ? 1 : mpv.speed + 0.25
                    }
                    Act {
                        glyph: mpv.muted || mpv.volume === 0 ? "qrc:/icons/mute.svg" : "qrc:/icons/volume.svg"
                        onClicked: mpv.muted = !mpv.muted
                    }
                    Bar {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: !surface.compact
                        width: 80
                        from: 0
                        to: 100
                        value: mpv.muted ? 0 : mpv.volume
                        onMoved: v => {
                            mpv.volume = v
                            mpv.muted = false
                        }
                    }
                    Act {
                        objectName: "pip-button"
                        glyph: "qrc:/icons/pip.svg"
                        tip: surface.compact ? qsTr("Back to qveil (I)") : qsTr("Picture in picture (I)")
                        onClicked: surface.pipRequested()
                    }
                    Act {
                        visible: !surface.compact
                        glyph: "qrc:/icons/theatre.svg"
                        tip: qsTr("Theatre (T)")
                        onClicked: surface.theatreToggled()
                    }
                    Act {
                        visible: !surface.compact
                        glyph: Window.window && Window.window.visibility === Window.FullScreen
                               ? "qrc:/icons/fullscreen-exit.svg" : "qrc:/icons/fullscreen.svg"
                        tip: qsTr("Fullscreen (F)")
                        onClicked: surface.fullscreenToggled()
                    }
                }
            }
        }
    }
}
