import QtQuick
import Qveil

// A line to type in. Enter submits what is there; Escape first clears it and
// then lets go of the keyboard.
Rectangle {
    id: field

    property alias text: input.text
    property string placeholder
    // Shown at the far end while the field is empty: the key that jumps here.
    property string hint
    property url glyph
    property alias maximumLength: input.maximumLength
    // Whether the keyboard is in the field.
    readonly property bool editing: input.activeFocus
    signal submitted(string text)
    signal edited(string text)
    // Escape was pressed in the empty field: whoever has the field decides where the keys go.
    signal escaped()

    function edit() {
        input.forceActiveFocus()
        input.selectAll()
    }

    implicitWidth: 240
    implicitHeight: Style.control + Style.u
    radius: Style.radius
    color: Style.rest
    border.width: 1
    border.color: editing ? Style.faint : Style.line

    Glyph {
        id: icon
        anchors.left: parent.left
        anchors.leftMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        visible: field.glyph.toString() !== ""
        source: field.glyph
        size: 15
        color: Style.faint
    }

    TextInput {
        id: input
        anchors.left: icon.visible ? icon.right : parent.left
        anchors.leftMargin: icon.visible ? Style.u * 2 : Style.u * 3
        anchors.right: key.visible ? key.left : parent.right
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        clip: true
        color: Style.fg
        selectionColor: Style.sel
        selectedTextColor: Style.fg
        selectByMouse: true
        font.family: Style.font
        font.pixelSize: Style.body
        onTextEdited: field.edited(text)
        onAccepted: {
            const typed = text.trim()
            if (typed !== "")
                field.submitted(typed)
        }
        Keys.onEscapePressed: {
            if (text !== "") {
                text = ""
                field.edited("")
            } else {
                focus = false
                field.escaped()
            }
        }

        Text {
            anchors.fill: parent
            visible: input.text === ""
            text: field.placeholder
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.faint
            font: input.font
        }
    }

    Text {
        id: key
        anchors.right: parent.right
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        visible: field.hint !== "" && input.text === "" && !field.editing
        text: field.hint
        textFormat: Text.PlainText
        color: Style.dim
        font.family: Style.font
        font.pixelSize: Style.caption
    }

    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.NoButton
        cursorShape: Qt.IBeamCursor
    }
    TapHandler { onTapped: input.forceActiveFocus() }
    // (A popup closed by the press that put the keyboard here takes it back to
    // the page as it goes, a moment later: the window is told, to send it back.)
    Connections {
        target: input
        function onActiveFocusChanged() {
            if (input.activeFocus && input.Window.window && input.Window.window.tappedField !== undefined) {
                input.Window.window.tappedField = input
                input.Window.window.tappedAt = Date.now()
            }
        }
    }
}
