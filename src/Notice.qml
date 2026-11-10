import QtQuick
import Qveil

// A line of explanation under a setting or a heading. What it says about
// privacy is always the engine's sentence, passed in as it came.
Text {
    visible: text !== ""
    textFormat: Text.PlainText
    wrapMode: Text.Wrap
    lineHeight: 1.25
    color: Style.dim
    font.family: Style.font
    font.pixelSize: Style.small
}
