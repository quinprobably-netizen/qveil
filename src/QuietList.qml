import QtQuick
import QtQuick.Controls

// A vertical list of QuietRows. It is moved like the video grid: the arrows
// choose a row, and Enter opens it.
ListView {
    id: list

    // The keyboard's row is marked once the keyboard is used, and unmarked when
    // the mouse takes over, like the video grid's selection ring.
    property bool navigating: false

    clip: true
    boundsBehavior: Flickable.StopAtBounds
    keyNavigationEnabled: true
    ScrollBar.vertical: ScrollBar {}

    Keys.onPressed: event => {
        navigating = true
        event.accepted = false
    }
}
