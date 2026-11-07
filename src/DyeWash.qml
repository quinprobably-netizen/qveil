import QtQuick

// qveil's signature: the theme's dyes swirled over whatever `source` shows.
// It is cached in a layer, so a still wash costs nothing per frame; it only
// re-renders while `strength` or `drift` animate.
ShaderEffect {
    id: wash

    property variant source
    property real strength: 1
    property real seed: 0
    property real drift: 0
    property real vignette: 0
    property real zoom: 1
    readonly property real aspect: width / Math.max(1, height)
    // The two dyes the swirl runs between. By default the accent and a deep
    // shade of it sunk toward the window's near-black; small bright items like
    // the wordmark use a lighter pair.
    property color accent: theme.accent
    property color deep: Qt.rgba(theme.accent.r * 0.22 + 0.03, theme.accent.g * 0.22 + 0.02,
                                 theme.accent.b * 0.22 + 0.05, 1)

    fragmentShader: "qrc:/shaders/dye.frag.qsb"
    layer.enabled: true
}
