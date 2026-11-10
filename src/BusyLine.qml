import QtQuick
import Qveil

// A thin line along the top of the page while something is loading. It fills
// when how far along is known, and otherwise runs back and forth.
Item {
    id: line

    property bool indeterminate: true
    property real value: 0

    implicitHeight: 2
    clip: true

    Rectangle {
        visible: !line.indeterminate
        height: parent.height
        width: parent.width * Math.max(0, Math.min(1, line.value))
        color: Style.accent
        Behavior on width { NumberAnimation { duration: 200 } }
    }
    Rectangle {
        id: runner
        visible: line.indeterminate && line.visible
        height: parent.height
        width: parent.width * 0.3
        color: Style.accent
        XAnimator on x {
            running: runner.visible
            loops: Animation.Infinite
            from: -runner.width
            to: line.width
            duration: 1400
        }
    }
}
