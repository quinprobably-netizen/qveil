import QtQuick
import Qveil
import "Format.js" as Format

// Where the video is, on a bar cut into its chapters, with the time and the
// chapter's name under it.
Column {
    id: seek

    // The player's position, length and how far ahead it has loaded, in seconds.
    property real position: 0
    property real length: 0
    property real cached: 0
    // [{ start, title }]
    property var chapters: []
    readonly property alias pressed: bar.pressed
    // The chapter the position is in, counted from 0, or -1 without chapters.
    readonly property int chapter: {
        let found = -1
        for (let i = 0; i < chapters.length; i++)
            if (chapters[i].start <= position + 0.5)
                found = i
        return found
    }
    // A stream that is running, and whether it plays at its live edge: there
    // the clock says how long it has run, and that it is live, and the bar is
    // full. How long it has run is told from when it started (`started`, Unix
    // seconds, from the play answer's `live`), the same as "started … ago"
    // under the title: the player's timeline is only what can be rewound,
    // which on a long stream is shorter than the stream.
    property bool live: false
    property bool atEdge: false
    property real started: 0
    readonly property bool edge: live && atEdge
    property real now: Date.now() / 1000
    Timer {
        interval: 1000
        repeat: true
        running: seek.edge && seek.started > 0 && seek.visible
        triggeredOnStart: true
        onTriggered: seek.now = Date.now() / 1000
    }
    // (At the edge, the word that it is live is said beside it, in the
    // accent: see PlayerSurface.)
    readonly property string time: edge ? Format.clock(started > 0 ? now - started : length)
                                        : Format.clock(position) + " / " + Format.clock(length)
    // "3/7 Reborrowing"
    readonly property string chapterText: chapter >= 0 ? (chapter + 1) + "/" + chapters.length + " " + chapters[chapter].title : ""
    signal moved(real seconds)

    Bar {
        id: bar
        objectName: "seek"
        width: parent.width
        from: 0
        to: Math.max(1, seek.length)
        value: seek.edge ? seek.length : seek.position
        buffered: seek.cached
        marks: seek.length > 0 ? seek.chapters.map(c => c.start / seek.length) : []
        onMoved: seconds => seek.moved(seconds)
    }
}
