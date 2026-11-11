pragma Singleton
import QtQuick

// Everything the window looks like, in one place: the colours of the theme
// it sits in, its one typeface, and the handful of sizes and
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

    // The one typeface: JetBrains Mono, in the Nerd Font build Omarchy ships,
    // whatever the desktop's interface font is. Where it isn't installed the
    // desktop's interface font stands in.
    readonly property string typeface: "JetBrainsMono Nerd Font"
    readonly property string font: Qt.fontFamilies().indexOf(typeface) >= 0 ? typeface : theme.uiFamily
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
    // JetBrains Mono's SemiBold, or its Bold where only Regular and Bold are
    // installed (Omarchy's ttf-jetbrains-mono-nerd-basic): Qt takes the
    // nearest real face and doesn't embolden it.
    readonly property int strong: Font.DemiBold

    readonly property int radius: theme.radius
    // The unit spacing is counted in.
    readonly property int u: 4
    // How tall the bar across the top is.
    readonly property int bar: 56
    readonly property int gutter: 24
    readonly property int control: 28
    // How wide the column down the side of a page is (the channels, the
    // settings' categories), in a page `width` wide.
    function rail(width) { return width < 700 ? 200 : 260 }
    // How far a button's word is from its edge, to either side (see Act). A
    // row of them at the end of a line is pulled out by this much, so that
    // the last word stands at the edge of what is under it.
    readonly property real actPad: u * 5 / 2
    // Whether things move at all: the desktop's own setting.
    readonly property bool motion: theme.animations
    // How long a fill or a veil takes to change, and how long something
    // takes to arrive or to glide to its place. Brief, or not at all.
    readonly property int quick: motion ? 120 : 0
    readonly property int move: motion ? 180 : 0
}
