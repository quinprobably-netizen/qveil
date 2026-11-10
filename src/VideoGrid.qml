import QtQuick
import QtQuick.Controls
import Qveil
import "Arrange.js" as Arrange

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
    // The list to show, when the grid is to sort and filter it itself (see
    // Arrange.js): a list model, and how it is set. Otherwise `model` is set.
    property var source: null
    property var arrange: ({})
    // What is known to have been watched: { id: true }.
    property var watched: ({})
    readonly property bool arranged: source !== null && !Arrange.plain(arrange)
    // (Named for its count, so that it is made again when the list fills.)
    readonly property var shown: arranged && source.count >= 0 ? Arrange.apply(Arrange.rows(source), arrange, watched) : []
    // What is showing, as something with count and get: what comes next
    // after a video is what comes next here.
    readonly property var list: arranged ? Arrange.listed(shown) : (source !== null ? source : model)
    function at(i) { return list.get(i) }
    signal activated(var video, int index)
    signal channelRequested(string id)

    readonly property int gutter: Style.gutter - Style.u * 2
    readonly property int columns: Math.max(1, Math.floor((width - 2 * gutter) / 280))
    // Whether the cards have a line saying why they are there.
    readonly property bool reasons: count > 0 && ((at(0).why || []).length > 0)

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

    model: arranged ? shown : source

    delegate: VideoCard {
        required property var model
        // (A row of a list model has the roles itself; an array's has the object.)
        video: model.modelData !== undefined ? model.modelData : model
        view: grid
        selected: GridView.isCurrentItem && grid.activeFocus && grid.navigating
    }

    function pick(i) {
        currentIndex = i
        activated(at(i), i)
    }
    // The pointer is on a card: the keyboard's mark steps aside.
    function pointed() { navigating = false }

    Keys.onPressed: event => {
        navigating = true
        event.accepted = false
    }
    Keys.onReturnPressed: if (currentIndex >= 0) activated(at(currentIndex), currentIndex)
    Keys.onEnterPressed: if (currentIndex >= 0) activated(at(currentIndex), currentIndex)
}
