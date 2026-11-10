import QtQuick
import Qveil

// A bar that shows how far along something is and can be dragged: the seek
// bar, the volume. `marks` cuts it into segments (a video's chapters): each
// is the fraction of the way along where a segment starts.
Item {
    id: bar

    property real from: 0
    property real to: 1
    property real value: 0
    // How far what is loaded ahead reaches, in the same units as `value`.
    property real buffered: 0
    property var marks: [0]
    readonly property bool pressed: area.pressed
    signal moved(real value)

    readonly property real span: Math.max(1e-9, to - from)
    readonly property real fraction: Math.max(0, Math.min(1, (value - from) / span))
    readonly property var cuts: {
        const m = (marks || []).filter(x => x >= 0 && x < 1).sort((a, b) => a - b)
        return m.length > 0 && m[0] === 0 ? m : [0].concat(m)
    }
    // The gap between two segments.
    readonly property int gap: 2

    implicitWidth: 200
    implicitHeight: Style.u * 4

    Repeater {
        model: bar.cuts.length
        delegate: Rectangle {
            id: segment
            objectName: "segment"
            required property int index
            readonly property real start: bar.cuts[index]
            readonly property real end: index + 1 < bar.cuts.length ? bar.cuts[index + 1] : 1
            readonly property bool last: index + 1 === bar.cuts.length
            x: Math.round(start * bar.width)
            width: Math.max(1, Math.round(end * bar.width) - x - (last ? 0 : bar.gap))
            height: area.containsMouse || area.pressed ? 6 : 4
            anchors.verticalCenter: parent ? parent.verticalCenter : undefined
            color: Style.tint(0.24)
            Behavior on height { NumberAnimation { duration: 80 } }

            // What is loaded ahead, then what has played, each as far as it
            // reaches into this segment.
            Rectangle {
                height: parent.height
                width: parent.width * bar.reach((bar.buffered - bar.from) / bar.span, segment.start, segment.end)
                color: Style.tint(0.22)
            }
            Rectangle {
                height: parent.height
                width: parent.width * bar.reach(bar.fraction, segment.start, segment.end)
                color: Style.accent
            }
        }
    }

    // How much of the stretch from `start` to `end` lies before `at`.
    function reach(at, start, end) {
        return Math.max(0, Math.min(1, (at - start) / Math.max(1e-9, end - start)))
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        function tell(mouse) {
            const f = Math.max(0, Math.min(1, mouse.x / Math.max(1, width)))
            bar.moved(bar.from + f * (bar.to - bar.from))
        }
        onPressed: mouse => tell(mouse)
        onPositionChanged: mouse => { if (pressed) tell(mouse) }
    }
}
