import QtQuick
import Qveil

// A few lines to type in: a lens's words. Enter submits (there are no line
// breaks to keep); Escape lets go of the keyboard.
Rectangle {
    id: area

    property alias text: input.text
    property string placeholder
    // The most characters it takes.
    property int maximumLength: 500
    // Whether the keyboard is in it.
    readonly property bool editing: input.activeFocus
    // What takes the keyboard.
    readonly property Item input: input
    signal submitted(string text)
    signal edited(string text)

    function edit() {
        input.forceActiveFocus()
        input.cursorPosition = input.length
    }

    implicitWidth: 400
    implicitHeight: Math.max(62, input.contentHeight + Style.u * 6)
    radius: Style.radius
    color: Style.rest
    border.width: 1
    border.color: editing ? Style.faint : Style.line

    TextEdit {
        id: input
        anchors.fill: parent
        anchors.margins: Style.u * 3
        wrapMode: TextEdit.Wrap
        textFormat: TextEdit.PlainText
        color: Style.fg
        selectionColor: Style.sel
        selectedTextColor: Style.fg
        selectByMouse: true
        font.family: Style.font
        font.pixelSize: 14
        // No more than fits, and no line breaks: a lens is one sentence.
        onTextChanged: {
            const kept = text.replace(/[\r\n]+/g, " ").slice(0, area.maximumLength)
            if (kept !== text) {
                const at = Math.min(cursorPosition, kept.length)
                text = kept
                cursorPosition = at
                return
            }
            if (activeFocus)
                area.edited(text)
        }
        Keys.onReturnPressed: area.submitted(text.trim())
        Keys.onEnterPressed: area.submitted(text.trim())
        Keys.onEscapePressed: focus = false

        Text {
            anchors.fill: parent
            visible: input.text === ""
            text: area.placeholder
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: Style.faint
            font: input.font
        }
    }
    TapHandler { onTapped: input.forceActiveFocus() }
}
