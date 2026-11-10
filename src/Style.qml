pragma Singleton
import QtQuick

// Everything the window looks like, in one place: the colours of the theme
// it sits in, the system's typeface, and the handful of sizes and fills that
// every part is drawn with. It is the language of the Omarchy panels, at
// window size: surfaces are the text colour at a low alpha, corners follow
// Hyprland, and there is one family of type.
QtObject {
    readonly property color bg: theme.background
    readonly property color fg: theme.foreground
    readonly property color accent: theme.accent
    readonly property color accentText: theme.accentForeground
    // A route that hides your address, and one that doesn't.
    readonly property color safe: theme.protectedColor
    readonly property color warn: "#e0a93b"
    // What lies over a picture so that text on it can be read.
    readonly property color scrim: "#cc08070b"

    // The text colour at an alpha: how every fill and line is made.
    function tint(alpha) { return Qt.rgba(fg.r, fg.g, fg.b, alpha) }
    readonly property color dim: tint(0.62)
    readonly property color faint: tint(0.40)
    readonly property color rest: tint(0.04)
    readonly property color hover: tint(0.08)
    readonly property color sel: tint(0.18)
    readonly property color line: tint(0.13)

    readonly property string font: theme.fontFamily
    readonly property int caption: 11
    readonly property int small: 12
    readonly property int body: 13
    readonly property int title: 16
    readonly property int head: 18

    readonly property int radius: theme.radius
    // The unit spacing is counted in.
    readonly property int u: 4
    readonly property int rail: 76
    readonly property int gutter: 24
    readonly property int control: 28
    // How long a fill or a veil takes to change.
    readonly property int quick: 120
}
