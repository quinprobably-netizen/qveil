import QtQuick
import Qveil

// A line that says something happened, and goes away by itself.
Rectangle {
    id: toast

    property string text
    // What the line offers to do ("Undo"), if anything, and what is sent when
    // it is pressed.
    property string action
    signal acted()
    function show(t) {
        action = ""
        text = t
        reveal()
    }
    // A line with something to press, which stays a little longer.
    function offer(t, what) {
        action = what
        text = t
        reveal()
    }
    function reveal() {
        opacity = 1
        hide.interval = action !== "" ? 7000 : 3000
        hide.restart()
    }

    width: row.implicitWidth + Style.u * 8
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
    Row {
        id: row
        anchors.centerIn: parent
        spacing: Style.u * 3
        Text {
            id: label
            anchors.verticalCenter: parent.verticalCenter
            text: toast.text
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.body
        }
        Act {
            objectName: "toast-undo"
            anchors.verticalCenter: parent.verticalCenter
            visible: toast.action !== "" && toast.opacity > 0
            text: toast.action
            filled: true
            onClicked: {
                hide.stop()
                toast.opacity = 0
                toast.acted()
            }
        }
    }
    Timer {
        id: hide
        interval: 3000
        onTriggered: toast.opacity = 0
    }
}
