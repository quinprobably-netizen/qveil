import QtQuick
import Qveil

// A line of explanation that appears beside what it explains once the
// pointer has rested there. It is a child of that item, and sits under it.
Rectangle {
    id: tip

    property string text
    // Whether the pointer is on what this explains.
    property bool shown: false
    readonly property bool up: shown && text !== "" && waited

    property bool waited: false
    onShownChanged: {
        waited = false
        if (shown)
            delay.restart()
        else
            delay.stop()
    }
    Timer {
        id: delay
        interval: 500
        onTriggered: tip.waited = true
    }

    visible: up
    z: 100
    y: parent ? parent.height + Style.u : 0
    width: label.implicitWidth + Style.u * 4
    height: label.implicitHeight + Style.u * 2
    radius: Style.radius
    color: Style.bg
    border.width: 1
    border.color: Style.line

    Text {
        id: label
        anchors.centerIn: parent
        text: tip.text
        textFormat: Text.PlainText
        color: Style.fg
        font.family: Style.font
        font.pixelSize: Style.small
    }
}
