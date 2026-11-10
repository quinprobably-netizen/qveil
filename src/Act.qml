import QtQuick
import Qveil

// A word you can press: "Refresh", "Subscribe", "Delete". It rests as plain
// text (or on the rest fill, when `filled`), takes the hover fill under the
// pointer or the keyboard focus, and gives a little when pressed.
Rectangle {
    id: act

    property string text
    property url glyph
    property bool filled: false
    // A quiet one is dim until it is pointed at.
    property bool quiet: false
    // The one thing to do here: in the accent.
    property bool primary: false
    // A mark after the word (a control that opens something has one).
    property string trailing
    // Whether the keyboard's cursor is on it.
    property bool marked: false
    property string tip
    readonly property bool hot: enabled && (area.containsMouse || activeFocus)
    signal clicked()

    implicitWidth: row.implicitWidth + Style.u * 5
    implicitHeight: Style.control
    radius: Style.radius
    color: primary ? (hot ? Qt.lighter(Style.accent, 1.12) : Style.accent) : hot ? Style.hover : filled ? Style.rest : "transparent"
    opacity: enabled ? 1 : 0.4
    border.width: marked ? 2 : 0
    border.color: primary ? Style.fg : Style.accent
    scale: area.pressed ? 0.97 : 1
    activeFocusOnTab: true
    Behavior on color { ColorAnimation { duration: Style.quick } }
    Behavior on scale { NumberAnimation { duration: 80 } }

    Keys.onReturnPressed: clicked()
    Keys.onEnterPressed: clicked()
    Keys.onSpacePressed: clicked()

    Row {
        id: row
        anchors.centerIn: parent
        spacing: Style.u * 2

        Glyph {
            anchors.verticalCenter: parent.verticalCenter
            visible: act.glyph.toString() !== ""
            source: act.glyph
            size: 15
            color: label.color
        }
        Text {
            id: label
            anchors.verticalCenter: parent.verticalCenter
            visible: text !== ""
            text: act.text
            textFormat: Text.PlainText
            color: act.primary ? Style.accentText : act.quiet && !act.hot ? Style.dim : Style.fg
            font.family: Style.font
            font.pixelSize: Style.body
            font.weight: act.primary ? Style.strong : Font.Normal
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            visible: text !== ""
            text: act.trailing
            textFormat: Text.PlainText
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.small
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: act.clicked()
    }

    Tip {
        text: act.tip
        shown: area.containsMouse
    }
}
