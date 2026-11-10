import QtQuick
import QtQuick.Controls
import Qveil

// A grid of videos that fits as many columns as the width allows.
GridView {
    id: grid

    // "frosted" or "clear": how thumbnails are shown (see Frost).
    property string thumbs: "frosted"
    // Said when a card's date is pointed at and is only approximate.
    property string estimateNote
    // How far each video was watched: { id: fraction }.
    property var progress: ({})
    // The selection shows once the keyboard is used, and hides on hover.
    property bool navigating: false
    signal activated(var video, int index)
    signal channelRequested(string id)

    readonly property int gutter: Style.gutter - Style.u * 2
    readonly property int columns: Math.max(1, Math.floor((width - 2 * gutter) / 280))
    // Whether the cards have a line saying why they are there.
    readonly property bool reasons: count > 0 && ((model.get(0).why || []).length > 0)

    leftMargin: gutter
    rightMargin: gutter
    bottomMargin: Style.gutter
    cellWidth: Math.floor((width - 2 * gutter) / columns)
    cellHeight: Math.round((cellWidth - Style.u * 4) * 9 / 16) + 110 + (reasons ? 18 : 0)
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    keyNavigationEnabled: true
    highlightFollowsCurrentItem: false
    cacheBuffer: 600

    ScrollBar.vertical: ScrollBar {
        id: bar
        contentItem: Rectangle {
            implicitWidth: 4
            color: bar.pressed || bar.hovered ? Style.faint : Style.line
        }
        background: Item {}
    }

    delegate: VideoCard {
        required property var model
        video: model
        view: grid
        selected: GridView.isCurrentItem && grid.activeFocus && grid.navigating
    }

    function pick(i) {
        currentIndex = i
        activated(model.get(i), i)
    }
    // The pointer is on a card: the keyboard's mark steps aside.
    function pointed() { navigating = false }

    Keys.onPressed: event => {
        navigating = true
        event.accepted = false
    }
    Keys.onReturnPressed: if (currentIndex >= 0) activated(model.get(currentIndex), currentIndex)
    Keys.onEnterPressed: if (currentIndex >= 0) activated(model.get(currentIndex), currentIndex)
}
