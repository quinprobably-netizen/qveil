import QtQuick
import QtQuick.Effects
import Qveil

// A thumbnail under the veil: blurred and muted in its own colours until it
// is looked at, so a wall of thumbnails doesn't shout. `clear` lifts it.
Item {
    id: frost

    property url source
    // Whether this one is being looked at: pointed at, or under the keyboard.
    property bool clear: false
    // "frosted", or "clear" for thumbnails shown as they are.
    property string mode: "frosted"
    readonly property bool ready: img.status === Image.Ready
    // Whether the veil is wanted at all.
    readonly property bool veiled: mode !== "clear"
    // Whether it can be drawn: without a GPU there is nothing to blur with,
    // and the picture is shown plainly.
    readonly property bool effect: veiled && GraphicsInfo.api !== GraphicsInfo.Software
    // How much of the veil is on: 1 veiled, 0 clear.
    // It comes in when it is there, not all at once.
    opacity: ready ? 1 : 0
    Behavior on opacity { NumberAnimation { duration: Style.move * 1.5 } }
    property real veil: clear ? 0 : 1
    Behavior on veil { NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }

    Image {
        id: img
        anchors.fill: parent
        source: frost.source
        fillMode: Image.PreserveAspectCrop
        asynchronous: true
        // The effect draws it when there is one.
        visible: !frost.effect
    }
    Loader {
        anchors.fill: parent
        active: frost.effect
        sourceComponent: MultiEffect {
            source: img
            // Keep the blur inside the frame instead of bleeding past its edges.
            autoPaddingEnabled: false
            blurEnabled: true
            blurMax: 48
            blur: frost.veil
            saturation: -0.35 * frost.veil
            brightness: -0.08 * frost.veil
        }
    }
}
