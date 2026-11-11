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
    // (It is there from the start: the keyboard needs no first key to show where it is.)
    property bool navigating: true
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
    readonly property var list: arranged ? held : (source !== null ? source : model)
    function at(i) { return list.get(i) }
    signal activated(var video, int index)
    // When set, left (or h) at the start of a row is not a move but `leftEdge`.
    property bool leftExits: false
    signal leftEdge()
    signal channelRequested(string id)

    readonly property int gutter: Style.gutter - Style.u * 2
    readonly property int columns: Math.max(1, Math.floor((width - 2 * gutter) / 280))
    // Whether the cards have a line saying why they are there.
    // Whether the cards have a line saying why they are there: when they
    // have reasons, and not all the same one (which the page already says).
    readonly property bool reasons: {
        let first = null
        for (let i = 0; i < count; i++) {
            const why = (at(i).why || []).join("|")
            if (first === null)
                first = why
            else if (why !== first)
                return true
        }
        return false
    }
    readonly property bool loudReasons: false

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

    // What is arranged is held in a list of its own, which is changed where
    // it differs when it is arranged anew: the view keeps its place and the
    // keyboard stays on its video, as with the list as it came.
    VideoModel { id: held }
    // How the list was last arranged, to tell the user's own change from a change in the list.
    property string appliedAs
    onShownChanged: {
        held.assign(arranged ? shown : [])
        // Sorted or filtered anew by the user, it starts from its start;
        // changed under them (a video watched, more results arriving), it
        // stays where it is.
        const as = JSON.stringify(Arrange.of(arrange))
        if (as !== appliedAs) {
            appliedAs = as
            if (arranged) {
                currentIndex = count > 0 ? 0 : -1
                positionViewAtBeginning()
            }
        }
    }
    model: arranged ? held : source

    delegate: VideoCard {
        required property var model
        video: model
        view: grid
        selected: GridView.isCurrentItem && grid.activeFocus && grid.navigating
    }

    function pick(i) {
        currentIndex = i
        activated(at(i), i)
    }
    // The pointer is on a card: the keyboard's mark steps aside.
    // (When it has moved, that is: a pointer that only rests over the window
    // when a page opens is not using it.)
    property point pointerAt: Qt.point(-1, -1)
    function pointed(at) {
        if (pointerAt.x >= 0 && (at.x !== pointerAt.x || at.y !== pointerAt.y))
            navigating = false
        pointerAt = at
    }

    add: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.move } }
    populate: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.move } }

    // h j k l move like the arrows.
    Keys.onPressed: event => {
        navigating = true
        event.accepted = true
        if (leftExits && (event.key === Qt.Key_H || event.key === Qt.Key_Left) && (currentIndex < 0 || currentIndex % columns === 0))
            leftEdge()
        else if (event.key === Qt.Key_H)
            moveCurrentIndexLeft()
        else if (event.key === Qt.Key_L)
            moveCurrentIndexRight()
        else if (event.key === Qt.Key_K)
            moveCurrentIndexUp()
        else if (event.key === Qt.Key_J)
            moveCurrentIndexDown()
        else
            event.accepted = false
    }
    Keys.onReturnPressed: if (currentIndex >= 0) activated(at(currentIndex), currentIndex)
    Keys.onEnterPressed: if (currentIndex >= 0) activated(at(currentIndex), currentIndex)
}
