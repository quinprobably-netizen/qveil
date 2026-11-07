import QtQuick
import QtQuick.Effects

// The name, filled with the same dye as the veil.
Item {
    id: mark

    property alias text: word.text
    property int pixelSize: 22

    implicitWidth: word.implicitWidth
    implicitHeight: word.implicitHeight

    Text {
        id: word
        font.pixelSize: mark.pixelSize
        font.weight: Font.Bold
        font.letterSpacing: 1.5
        color: "white"
        visible: false
        layer.enabled: true
    }
    // Mid-grey under the dye: bright enough for the letters, without clipping.
    Rectangle {
        id: light
        anchors.fill: parent
        color: "#b4b4b4"
        visible: false
        layer.enabled: true
    }
    DyeWash {
        id: fill
        anchors.fill: parent
        source: light
        seed: 3.1
        // Broad swirls between the accent and a pale tint of it (never the deep
        // shade), so the letters stay legible and bright at this size.
        zoom: 0.3
        deep: theme.accent
        accent: Qt.rgba(theme.accent.r * 0.4 + 0.6, theme.accent.g * 0.4 + 0.6, theme.accent.b * 0.4 + 0.6, 1)
        visible: false
    }
    MultiEffect {
        anchors.fill: parent
        source: fill
        maskEnabled: true
        maskSource: word
    }
}
