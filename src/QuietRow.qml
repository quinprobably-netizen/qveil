import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material

// A row of a QuietList, with a quiet fill for hover and keyboard focus. The
// style's own ItemDelegate background is a Material ripple, which on a row as
// wide as the window grows into a huge grey circle.
ItemDelegate {
    id: row

    // Whether the mouse or the keyboard is on this row, for rows that offer
    // more then (Unsubscribe).
    readonly property bool lit: hovered || keyboardRow
    readonly property color accent: theme.accent
    readonly property bool keyboardRow: ListView.isCurrentItem && ListView.view.navigating

    hoverEnabled: true
    // The mouse takes over from the keyboard, as it does in the video grid.
    onHoveredChanged: if (hovered) ListView.view.navigating = false
    Keys.onReturnPressed: clicked()
    Keys.onEnterPressed: clicked()

    background: Rectangle {
        radius: Math.min(4, theme.radius)
        // Steps the eye can tell from the page's #0d0d10 at a glance: the
        // earlier #1a1a20 and #202027 were barely lighter than it. A little of
        // the accent says which one the pointer is on, and the keyboard's is
        // one step further.
        color: row.keyboardRow ? Qt.tint("#2f2f3a", Qt.rgba(accent.r, accent.g, accent.b, 0.12))
                               : row.hovered ? Qt.tint("#24242d", Qt.rgba(accent.r, accent.g, accent.b, 0.08))
                                             : "transparent"
    }
}
