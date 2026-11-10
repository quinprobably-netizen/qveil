pragma Singleton
import QtQuick

// Everything the window looks like, in one place: the colours of the theme
// it sits in, the desktop's interface typeface, and the handful of sizes and
// fills that every part is drawn with. Pictures lead and type is sized to be
// read from a seat, as on a video site; surfaces are the text colour at a
// low alpha, corners follow Hyprland, and nothing is rounded into a bubble.
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

    readonly property string font: theme.uiFamily
    readonly property int caption: 12
    readonly property int small: 13
    readonly property int body: 14
    // A video's title on its card.
    readonly property int card: 15
    readonly property int title: 18
    readonly property int head: 22
    // The one large size: the video that leads the home page.
    readonly property int lead: 32
    // What "bold" is: a weight the face has, since a synthesized bold smears.
    readonly property int strong: Font.DemiBold

    readonly property int radius: theme.radius
    // The unit spacing is counted in.
    readonly property int u: 4
    readonly property int rail: 56
    readonly property int gutter: 24
    readonly property int control: 28
    // How long a fill or a veil takes to change.
    readonly property int quick: 120
}
