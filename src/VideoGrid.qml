import QtQuick
import QtQuick.Controls

GridView {
    id: grid

    property bool veil: true
    property string estimateNote
    // The selection ring shows once the keyboard is used, and hides on hover.
    property bool navigating: false
    signal activated(var video)
    signal channelRequested(string id)

    readonly property int columns: Math.max(1, Math.floor((width - 40) / 290))

    leftMargin: 20
    rightMargin: 20
    bottomMargin: 28
    cellWidth: Math.floor((width - 40) / columns)
    cellHeight: Math.round((cellWidth - 16) * 9 / 16) + 102
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    keyNavigationEnabled: true
    highlightFollowsCurrentItem: false
    cacheBuffer: 600

    ScrollBar.vertical: ScrollBar {}

    delegate: VideoCard {}

    Keys.onPressed: event => {
        navigating = true
        event.accepted = false
    }
    Keys.onReturnPressed: if (currentIndex >= 0) activated(model.get(currentIndex))
    Keys.onEnterPressed: if (currentIndex >= 0) activated(model.get(currentIndex))
}
