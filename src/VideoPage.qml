import QtQuick
import Qveil

// A heading, optional actions at its end, and a grid of videos: a search's
// results, a channel's uploads.
FocusScope {
    id: page

    property string heading
    property string subheading
    // A VideoModel.
    property var model
    property string emptyText
    property bool loading: false
    default property alias actions: actionRow.data
    readonly property alias grid: grid
    // How the list is sorted and filtered, what the order it came in is
    // called, and what is known to have been watched.
    property var arrange: ({})
    property string sourceLabel: qsTr("Newest")
    property var watched: ({})
    signal arranged(var setting)
    function openSort() { pageSort.open() }
    // Where focus belongs when the page is shown.
    readonly property Item content: grid
    property alias thumbs: grid.thumbs
    property alias progress: grid.progress
    // Explains in a tooltip over a card's date why it is approximate: each
    // page knows why.
    property alias estimateNote: grid.estimateNote
    // What goes between the heading and the grid.
    property alias extras: extra.data

    signal activated(var video, int index)
    signal channelRequested(string id)

    Column {
        id: top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        spacing: Style.u * 2

        Item {
            width: parent.width
            height: Math.max(words.height, actionRow.height)
            Column {
                id: words
                anchors.left: parent.left
                anchors.right: actionRow.left
                anchors.rightMargin: Style.u * 3
                spacing: 3
                Text {
                    width: parent.width
                    text: page.heading
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.head
                    font.weight: Style.strong
                }
                Text {
                    width: parent.width
                    visible: text !== ""
                    text: page.subheading
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
            Row {
                id: actionRow
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.u
                SortMenu {
                    id: pageSort
                    objectName: "page-sort"
                    setting: page.arrange
                    sourceLabel: page.sourceLabel
                    onChanged: setting => page.arranged(setting)
                }
            }
        }
        Column {
            id: extra
            width: parent.width
            spacing: Style.u * 2
        }
        Rule { width: parent.width }
    }

    VideoGrid {
        id: grid
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: top.bottom
        anchors.bottom: parent.bottom
        anchors.topMargin: Style.u * 2
        focus: true
        source: page.model
        arrange: page.arrange
        watched: page.watched
        onActivated: (video, index) => page.activated(video, index)
        onChannelRequested: id => page.channelRequested(id)
    }

    EmptyState {
        anchors.fill: grid
        visible: !page.loading && page.model.count === 0 && page.emptyText !== ""
        text: page.emptyText
    }
}
