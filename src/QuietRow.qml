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
    readonly property bool keyboardRow: ListView.isCurrentItem && ListView.view.navigating

    hoverEnabled: true
    // The mouse takes over from the keyboard, as it does in the video grid.
    onHoveredChanged: if (hovered) ListView.view.navigating = false
    Keys.onReturnPressed: clicked()
    Keys.onEnterPressed: clicked()

    background: Rectangle {
        radius: Math.min(4, theme.radius)
        color: row.keyboardRow ? "#202027" : row.hovered ? "#1a1a20" : "transparent"
    }
}
