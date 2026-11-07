import QtQuick

// The name, filled with the same dye as the veil. The text itself is the
// dye's source: the shader keeps its alpha, so the letters' edges (the i's
// dot, the e's eye) stay as crisp as plain text, with no mask pass to soften
// them.
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
        // Mid-grey: under the dye it gives bright letters without clipping.
        color: "#b4b4b4"
        visible: false
        layer.enabled: true
    }
    DyeWash {
        anchors.fill: parent
        source: word
        seed: 3.1
        // Broad swirls between the accent and a pale tint of it (never the deep
        // shade), so the letters stay legible and bright at this size.
        zoom: 0.3
        deep: theme.accent
        accent: Qt.rgba(theme.accent.r * 0.4 + 0.6, theme.accent.g * 0.4 + 0.6, theme.accent.b * 0.4 + 0.6, 1)
    }
}
