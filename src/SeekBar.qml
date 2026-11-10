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
    readonly property string time: Format.clock(position) + " / " + Format.clock(length)
    // "3/7 Reborrowing"
    readonly property string chapterText: chapter >= 0 ? (chapter + 1) + "/" + chapters.length + " " + chapters[chapter].title : ""
    signal moved(real seconds)

    Bar {
        id: bar
        objectName: "seek"
        width: parent.width
        from: 0
        to: Math.max(1, seek.length)
        value: seek.position
        buffered: seek.cached
        marks: seek.length > 0 ? seek.chapters.map(c => c.start / seek.length) : []
        onMoved: seconds => seek.moved(seconds)
    }
}
