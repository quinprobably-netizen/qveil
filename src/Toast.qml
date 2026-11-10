import QtQuick
import Qveil

// A line that says something happened, and goes away by itself.
Rectangle {
    id: toast

    property string text
    function show(t) {
        text = t
        opacity = 1
        hide.restart()
    }

    width: label.implicitWidth + Style.u * 8
    height: Style.control + Style.u * 2
    radius: Style.radius
    color: Style.bg
    border.width: 1
    border.color: Style.line
    opacity: 0
    visible: opacity > 0
    Behavior on opacity { NumberAnimation { duration: 200 } }

    // The fill is drawn over the page's own colour, so the text can be read.
    Rectangle {
        anchors.fill: parent
        anchors.margins: 1
        radius: parent.radius
        color: Style.hover
    }
    Text {
        id: label
        anchors.centerIn: parent
        text: toast.text
        textFormat: Text.PlainText
        color: Style.fg
        font.family: Style.font
        font.pixelSize: Style.body
    }
    Timer {
        id: hide
        interval: 3000
        onTriggered: toast.opacity = 0
    }
}
