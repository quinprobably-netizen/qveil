import QtQuick
import Qveil

// One of a row of choices, as a word. The current one takes the selected
// fill and heavier type; the others are dim until pointed at.
Rectangle {
    id: tab

    property string text
    property url glyph
    property bool current: false
    // A tab (which page of something is showing) is marked by a line under
    // it; a choice among settings keeps the selected fill.
    property bool underlined: false
    readonly property bool hot: area.containsMouse || activeFocus
    signal clicked()

    implicitWidth: row.implicitWidth + Style.u * 6
    implicitHeight: Style.control
    radius: Style.radius
    color: current && !underlined ? Style.sel : hot ? Style.hover : "transparent"
    activeFocusOnTab: true
    Behavior on color { ColorAnimation { duration: Style.quick } }

    Keys.onReturnPressed: clicked()
    Keys.onEnterPressed: clicked()
    Keys.onSpacePressed: clicked()

    Row {
        id: row
        anchors.centerIn: parent
        spacing: Style.u * 2

        Glyph {
            anchors.verticalCenter: parent.verticalCenter
            visible: tab.glyph.toString() !== ""
            source: tab.glyph
            size: 14
            color: label.color
        }
        Text {
            id: label
            anchors.verticalCenter: parent.verticalCenter
            text: tab.text
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: tab.current || tab.hot ? Style.fg : Style.dim
            font.family: Style.font
            font.pixelSize: Style.body
            font.weight: (tab.current) ? Style.strong : Font.Normal
        }
    }

    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        visible: tab.underlined && tab.current
        height: 2
        color: Style.accent
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: tab.clicked()
    }
}
