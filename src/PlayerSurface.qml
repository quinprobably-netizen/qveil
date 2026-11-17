import QtQuick
import Qveil
import "Format.js" as Format

// The video and its controls. It is shown in the watch page, and again in
// the small window of picture in picture. Only one of them plays at a time,
// and the player is handed from one to the other (see adopt).
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
    // Set when the video is played from its start in place of a kept place:
    // that place is forgotten once the first frame is up.
    property bool forgetWhenStarted: false
    // The last second the video was known to be at while it played (which
    // stays when the player has lost it: its streams are gone).
    property real lastPosition: 0
    // The second it last started a video at (0 from the start), and the
    // second it was last sent to: what the window asked of the player.
    property real startedAt: 0
    property real lastSeek: -1
    // The answer's, or (when it said there may be more, `chaptersLater`)
    // those looked up once the Chapters tab was opened.
    readonly property var chapters: info.chapters && info.chapters.length > 0 ? info.chapters
        : (info.chaptersLater && backend.chapters.video === info.id ? (backend.chapters.items || []) : [])
    // What the player says once it knows; until then, what the engine said.
    // (The length mpv gives a growing playlist is only as new as its last
    // reload of it, so a running stream is at least as long as where it is,
    // and as its live edge.)
    readonly property real length: mpv.duration > 0 ? Math.max(mpv.duration, live ? Math.max(mpv.position, liveEdge) : 0)
                                                    : (live && liveEdge > 0 ? liveEdge : (info.duration || 0))

    // A stream that is running (the answer's `live.state` is "now"), until
    // the engine says it has ended. Its seek bar spans the whole stream so far
    // and grows, and where it is isn't kept for resuming.
    readonly property bool live: !!info.live && info.live.state === "now"
    // How long one of its segments is, in seconds.
    readonly property real segment: live ? (info.live.segment || 5) : 0
    // How far back from the end a running stream is joined, in seconds: the
    // answer's `live.join`, which is where it starts (minus `start`). Three
    // segments when the engine says nothing.
    readonly property real join: live ? (info.live.join > 0 ? info.live.join : 3 * segment) : 0
    // The live edge on the player's timeline, in seconds: the end of the
    // newest segment the engine has, as it last said (`live/<session>/state`),
    // 0 until it has. Not mpv's duration, which is only as new as its last
    // reload of the playlist; it doesn't reload while it plays far behind with
    // its cache full, and minutes later that is still where it was rewound.
    // Not a count of segments either: the segments are not all `segment`
    // long on the player's clock (the playlist's lengths follow the media's,
    // and a stream that was cut for a while is up to half a minute off a
    // count over its five days), so only the engine knows.
    property real liveEdge: 0
    // How far behind the edge it plays, in seconds.
    readonly property real behind: liveEdge > 0 ? liveEdge - mpv.position : 0
    // Whether it plays at the live edge: within four segments of the end of
    // the newest one, which is where it is joined and where Go live goes
    // (`join` from the end, three segments), with a segment to spare for the
    // edge, which grows a segment at a time. Once there, it stays until it is six behind:
    // the player falls a little behind as it opens (the edge moves on while
    // it loads) and whenever it waits for a segment, and a clock and button
    // that blinked in and out with that would say nothing. Until the engine
    // has said where the edge is, and the first frame is up, it is there if
    // it was opened there; while it seeks or waits for a segment it stays
    // what it was (the position is the old one until it has got there).
    property bool atEdge: false
    function judgeEdge() {
        if (!live)
            atEdge = false
        else if (liveEdge <= 0 || !mpv.started)
            atEdge = startedAt < 0
        else if (!mpv.buffering)
            atEdge = behind <= (atEdge ? 6 : 4) * segment
    }
    onBehindChanged: judgeEdge()
    onLiveChanged: judgeEdge()
    onStartedAtChanged: judgeEdge()
    Connections {
        target: mpv
        function onStartedChanged() { surface.judgeEdge() }
        function onBufferingChanged() { surface.judgeEdge() }
    }
    // Set once a stream that was running when it was opened has ended: it
    // is a video like any other from then on (and the Comments tab returns).
    property bool liveEnded: false
    // Set once the engine has said it can't go on with a running stream (the
    // window shows why): it plays what it has, and isn't asked about again.
    property bool liveFailed: false
    // How often the engine is asked where a running stream's edge is, and
    // whether it has ended, in ms: 0 for once a segment, which keeps the
    // edge as new as the stream.
    property int liveCheckInterval: 0
    // Go live was asked for before the engine had said where the edge is.
    property bool goLiveDue: false

    // An announced stream that hasn't started (the answer said `waiting`):
    // the waiting room shows in place of the player, which loads nothing.
    readonly property bool waiting: active && !pending && !!info.waiting
    // What the room shows: when it starts (Unix seconds, 0 if not known),
    // "upcoming" while the engine waits ("gave_up" or "failed" once it has
    // stopped, with its words in waitMessage), or "" with no wait running.
    property real waitStarts: 0
    property string waitState: ""
    property string waitMessage: ""
    // "bot_check" while the engine's asks meet a bot check (it keeps asking),
    // or the code of what ended the wait; "" otherwise. Both may be got past
    // on another route, which the room offers (see routeRequested).
    property string waitNote: ""
    readonly property bool waitOffersRoute: waitState === "upcoming" ? waitNote === "bot_check" : backend.offersRouteFor(waitNote)
    // Set when the stream that was waited for has started, until its play
    // answer has opened it, at its start, not at its edge. (What shows
    // beside it stays the room's meanwhile: see SideSheet.waiting.)
    property bool fromStart: false

    // The stream has started: it is looked up once more, and plays from its start.
    function begin() {
        if (!waiting || waitState !== "upcoming")
            return
        const i = info
        prepareAs({ id: i.id, title: i.title || "", channel: i.channel || "", channelId: i.channelId || "",
                  published: i.published || 0, batch: i.batch || 0 }, true)
        // (It has started: asked as a stream that is running.)
        backend.play(i.id, i.channel || "", "now")
    }
    // Lets go of the wait, if there is one.
    function endWait() {
        if (waitState === "upcoming")
            backend.stopWait()
        waitState = ""
        waitMessage = ""
        waitNote = ""
        waitStarts = 0
    }
    Connections {
        target: backend
        function onWaitState(state, starts, note) {
            if (!surface.waiting || surface.waitState !== "upcoming")
                return
            if (state === "upcoming") {
                if (starts > 0)
                    surface.waitStarts = starts
                surface.waitNote = note
            } else if (state === "now")
                // (Not from inside the engine's answer, which this hangs up on.)
                Qt.callLater(surface.begin)
        }
        function onWaitFailed(message, code) {
            if (!surface.waiting || surface.waitState !== "upcoming")
                return
            surface.waitState = code === "gave_up" ? "gave_up" : "failed"
            surface.waitMessage = message
            surface.waitNote = code
        }
    }

    // To the live edge, where a running stream is joined: `join` seconds
    // from the end of the newest segment the engine has (its `edge`). If it
    // hasn't said yet, it is asked, and the seek follows its answer.
    function goLive() {
        if (!live)
            return
        if (liveEdge <= 0) {
            goLiveDue = true
            backend.checkLive(info.video)
            return
        }
        goLiveDue = false
        seekTo(Math.max(0, liveEdge - join))
        atEdge = true
    }

    // The channel's name and the age that were looked up apart (see
    // `openAt`) fill what the video has none of.
    Connections {
        target: backend
        function onChaptersChanged() {
            const found = backend.chapters
            if (!surface.active || !found.video || found.video !== surface.info.id)
                return
            const add = {}
            if (!surface.info.channel && found.channel)
                add.channel = found.channel
            if (!surface.info.published && found.published > 0)
                add.published = found.published
            if (Object.keys(add).length > 0)
                surface.info = Object.assign({}, surface.info, add)
        }
    }

    signal closed()
    signal pipRequested()
    // The waiting room's offer of another route was taken.
    signal routeRequested()
    signal theatreToggled()
    signal fullscreenToggled()

    // Shows the player for `video` (a card's data) at once, while the engine
    // looks it up; `open` follows with the streams.
    function prepare(video) { prepareAs(video, false) }
    // `starting`: it is the stream that was waited for, which has started (see begin).
    function prepareAs(video, starting) {
        report()
        endWait()
        fromStart = !!starting
        if (mpv.loaded)
            mpv.stop()
        info = {
            id: video.id,
            title: video.title || "",
            channel: video.channel || "",
            channelId: video.channel_id || video.channelId || "",
            published: video.published || 0,
            // The card's page, so the poster is the thumbnail it already has.
            batch: video.batch || 0
        }
        note.shown = false
        forgetWhenStarted = false
        lastPosition = 0
        startedAt = 0
        lastSeek = -1
        liveEnded = false
        liveFailed = false
        forgetEdge()
        pending = true
        active = true
        reveal()
    }

    function open(i) { openAt(i, undefined) }

    // `at`: where to start instead of where the answer says (a video picked
    // up from the other window carries on where that one was).
    function openAt(i, at) {
        endWait()
        // The stream that was waited for, now that it runs: from its beginning.
        if (at === undefined && fromStart && i.id === info.id)
            at = 0
        pending = false
        // The card's channel and age stay where the answer has none (a lookup
        // without the watch page has neither) and it is the same video.
        const card = info.id === i.id ? { channel: info.channel || "", published: info.published || 0 } : {}
        info = Object.assign({ batch: info.batch || 0 }, card, i)
        // (Once it is what the answer says: a stream that has started is
        // running by now, and never neither waited for nor running.)
        fromStart = false
        active = true
        liveEnded = false
        liveFailed = false
        forgetEdge()
        // (A running stream's start is negative: that many seconds before its end.)
        const resumed = at === undefined && !i.start && i.resume > 0
        startedAt = at !== undefined ? at : (i.start ? i.start : (i.resume > 0 ? i.resume : 0))
        lastSeek = -1
        lastPosition = Math.max(0, startedAt)
        note.shown = resumed
        if (resumed)
            noteTimer.restart()
        // What neither the card nor the answer had (a pasted link has no
        // card, and the lookup left the page out) is looked up apart.
        if (i.detailsLater && (!info.channel || !info.published))
            backend.loadChapters(i.id)
        mpv.volume = savedVolume
        // Announced, and not started: the engine is asked to say when it
        // does, from the time the answer gave. Nothing is loaded meanwhile.
        if (i.waiting) {
            waitStarts = i.waiting.starts > 0 ? i.waiting.starts : 0
            waitState = "upcoming"
            backend.wait(i.id, waitStarts)
            reveal()
            return
        }
        // A video without a stream has nothing to load (and makes no sound).
        if (i.video)
            mpv.load(i.video, i.audio || "", startedAt, !!i.live)
        reveal()
    }

    // Takes over the video `from` (the other window's surface) is playing,
    // with its player: it goes on here from where it is, and nothing is
    // looked up or opened again. False if there was no player to take.
    function adopt(from) {
        if (!mpv.takeOver(from.mpv))
            return false
        pending = false
        info = from.info
        startedAt = from.startedAt
        lastSeek = from.lastSeek
        lastPosition = from.lastPosition
        forgetWhenStarted = from.forgetWhenStarted
        liveEnded = from.liveEnded
        liveFailed = from.liveFailed
        liveEdge = from.liveEdge
        atEdge = from.atEdge
        note.shown = false
        active = true
        from.release()
        reveal()
        return true
    }
    // Where the edge of the stream that was showing is, for none.
    function forgetEdge() {
        liveEdge = 0
        goLiveDue = false
    }

    // Lets go of the video that another surface adopted: it is still
    // playing, so where it is isn't told from here.
    function release() { finish(false) }

    // Stops the video and remembers where it was.
    function stop() { finish(true) }
    function finish(stopping) {
        const was = active
        if (stopping)
            report()
        if (pending)
            backend.cancelPlay()
        endWait()
        fromStart = false
        pending = false
        mpv.stop()
        active = false
        // Nobody is waiting for what the engine says of the stream now. (One
        // that was handed over is still asked about, by the other surface.)
        if (live && stopping)
            backend.stopLiveCheck()
        info = ({})
        forgetEdge()
        note.shown = false
        forgetWhenStarted = false
        if (was)
            closed()
    }

    // (A seek of a running stream is judged anew once it has got there: is
    // it at the edge? See atEdge.)
    function seekTo(seconds) {
        lastSeek = seconds
        if (live)
            atEdge = false
        note.shown = false
        mpv.seek(seconds, false)
        reveal()
        afterSeek.restart()
    }
    // A step back or forward, by the keyboard.
    function nudge(seconds) {
        if (live)
            atEdge = false
        mpv.seek(seconds, true)
        reveal()
        afterSeek.restart()
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
    // again. Nothing is said before the player knows the length, nor while
    // a stream runs: it has moved on by the time it would be picked up.
    function report() {
        if (active && !pending && info.id && mpv.duration > 0 && !live)
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
        onPositionChanged: if (started && position > 0) surface.lastPosition = position
        onPausedChanged: {
            surface.reveal()
            if (paused)
                surface.report()
        }
        onEndedChanged: if (ended) surface.report()
        // The picture couldn't follow a handover: the video is opened again, where it was.
        onTakeOverFailed: if (surface.active && surface.info.video) surface.openAt(surface.info, surface.lastPosition)
        onStartedChanged: {
            if (started && surface.forgetWhenStarted) {
                surface.forgetWhenStarted = false
                backend.forgetResume(surface.info.id)
            }
        }
    }

    // After a seek, once the keys have rested and the player has got
    // there (over a slow route that takes a while, and until then the
    // position is still the old one).
    Timer {
        id: afterSeek
        interval: 700
        onTriggered: mpv.buffering ? restart() : surface.report()
    }
    // While it plays, every so often.
    Timer {
        objectName: "report-timer"
        interval: 15000
        repeat: true
        running: surface.active && !surface.pending && !mpv.paused && !mpv.ended
        onTriggered: surface.report()
    }

    // While a stream runs, once a segment from when it is opened: where is
    // its edge, and has it ended? The engine knows (its playlists get their
    // end), and says so at `live/<session>/state`.
    Timer {
        objectName: "live-check"
        interval: surface.liveCheckInterval > 0 ? surface.liveCheckInterval : Math.max(1000, surface.segment * 1000)
        repeat: true
        triggeredOnStart: true
        running: surface.active && !surface.pending && surface.live && !surface.liveFailed
        onTriggered: backend.checkLive(surface.info.video)
    }
    Connections {
        target: backend
        function onLiveChecked(playlist, state, edge) {
            if (!surface.active || !surface.live || playlist !== surface.info.video)
                return
            if (state === "failed") {
                surface.liveFailed = true
                return
            }
            if (state !== "ended") {
                if (edge > 0)
                    surface.liveEdge = edge
                if (surface.goLiveDue && surface.liveEdge > 0)
                    surface.goLive()
                return
            }
            // It plays on to its end with what it has (the playlists are
            // closed), and is a video like any other from here.
            const was = surface.info
            surface.info = Object.assign({}, was, {
                live: Object.assign({}, was.live, { state: "ended" }),
                streamed: true,
                published: was.published || was.live.started || 0
            })
            surface.liveEnded = true
        }
    }

    // The video's thumbnail until its first frame: the engine has it from
    // the grid, so this is no new request for anything not already shown.
    Image {
        anchors.fill: parent
        fillMode: Image.PreserveAspectFit
        asynchronous: true
        source: surface.active ? Format.thumb(backend.thumbBase, surface.info.id, surface.info.batch) : ""
        opacity: surface.active && !mpv.started && !surface.waiting ? 0.5 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 180 } }
    }

    WaitingRoom {
        anchors.fill: parent
        // (Over the area that pauses on a click, so that its offer can be
        // pressed; a click anywhere else in it goes through.)
        z: 1
        visible: surface.waiting
        info: surface.info
        starts: surface.waitStarts
        phase: surface.waitState || "upcoming"
        message: surface.waitMessage
        botCheck: surface.waitState === "upcoming" && surface.waitNote === "bot_check"
        offersRoute: surface.waitOffersRoute
        onRouteRequested: surface.routeRequested()
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
        onClicked: if (!surface.waiting) mpv.togglePause()
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

    // Once it has ended: Replay, and when a list goes on from here, the way on
    // to the next in it. (Nothing starts by itself.)
    // What comes after this video in its list, or null.
    property var nextVideo: null
    signal nextRequested()
    Rectangle {
        id: ended
        objectName: "ended"
        readonly property bool hasNext: !!surface.nextVideo && !surface.compact
        anchors.centerIn: parent
        // (Not over the next video while it is being looked up: the player still has the last one's end.)
        visible: mpv.ended && !surface.pending
        width: endRow.implicitWidth + (hasNext ? Style.u * 4 : 0)
        height: endRow.implicitHeight + (hasNext ? Style.u * 4 : 0)
        radius: Style.radius
        color: hasNext ? Style.scrim : "transparent"
        Row {
            id: endRow
            anchors.centerIn: parent
            spacing: Style.u * 2
            Act {
                objectName: "ended-replay"
                text: qsTr("Replay")
                glyph: "qrc:/icons/replay.svg"
                filled: !ended.hasNext
                onClicked: mpv.togglePause()
            }
            Act {
                id: nextAct
                objectName: "ended-next"
                readonly property string title: surface.nextVideo ? surface.nextVideo.title || "" : ""
                visible: ended.hasNext
                glyph: "qrc:/icons/next.svg"
                primary: true
                // (A long title is cut to what fits: the button is not wider than half the picture.)
                text: qsTr("Next: %1").arg(nextFits.elidedText)
                TextMetrics {
                    id: nextFits
                    font.family: Style.font
                    font.pixelSize: Style.body
                    font.weight: Style.strong
                    elide: Qt.ElideRight
                    elideWidth: Math.min(360, surface.width * 0.45)
                    text: nextAct.title
                }
                onClicked: surface.nextRequested()
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                visible: ended.hasNext
                text: "N"
                textFormat: Text.PlainText
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
                rightPadding: Style.u * 2
            }
        }
    }

    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: controls.height + Style.u * 12
        // (Not over the waiting room: there is nothing to play yet.)
        opacity: surface.chrome && surface.active && !surface.waiting ? 1 : 0
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
                live: surface.live
                atEdge: surface.atEdge
                started: surface.live ? (surface.info.live.started || 0) : 0
                cached: mpv.cached
                chapters: surface.chapters
                onMoved: seconds => surface.seekTo(seconds)
            }
            Item {
                width: parent.width
                height: Style.control + Style.u
                Row {
                    id: leftControls
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u * 2
                    Act {
                        id: playAct
                        glyph: mpv.paused || mpv.ended ? "qrc:/icons/play.svg" : "qrc:/icons/pause.svg"
                        onClicked: mpv.togglePause()
                    }
                    Text {
                        id: timeText
                        objectName: "seek-time"
                        anchors.verticalCenter: parent.verticalCenter
                        text: bar.time
                        // (A little further from the Go live chip than the row's spacing.)
                        rightPadding: goLiveAct.visible ? Style.u : 0
                        textFormat: Text.PlainText
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    // At a running stream's edge: that it is live, in the accent.
                    Text {
                        id: liveWord
                        objectName: "seek-live"
                        anchors.verticalCenter: parent.verticalCenter
                        visible: bar.edge
                        text: qsTr("\u25cf live")
                        textFormat: Text.PlainText
                        color: Style.accent
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    // Behind a running stream's edge: the way back to it, a
                    // chip in the accent's fill (as the cards' "● LIVE" is)
                    // with its key dimmed inside it.
                    Act {
                        id: goLiveAct
                        objectName: "go-live"
                        anchors.verticalCenter: parent.verticalCenter
                        visible: surface.live && !surface.atEdge
                        primary: true
                        radius: 0
                        text: qsTr("Go live")
                        trailing: surface.compact ? "" : "End"
                        onClicked: surface.goLive()
                    }
                    Text {
                        objectName: "seek-chapter"
                        // What is left beside the controls at the right; too little, and it steps aside.
                        readonly property real room: leftControls.parent.width - rightControls.width - playAct.width
                            - timeText.width - (goLiveAct.visible ? goLiveAct.width + leftControls.spacing : 0)
                            - (liveWord.visible ? liveWord.width + leftControls.spacing : 0)
                            - 2 * leftControls.spacing - Style.u * 4
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, Math.min(implicitWidth, room))
                        elide: Text.ElideRight
                        visible: text !== "" && !surface.compact && room >= 80
                        text: bar.chapterText
                        textFormat: Text.PlainText
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
                Row {
                    id: rightControls
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: Style.u
                    Act {
                        objectName: "seek-speed"
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
