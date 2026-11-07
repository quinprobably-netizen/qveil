import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// A heading, optional actions on the right, and a grid of videos.
FocusScope {
    id: page

    property string heading
    property string subheading
    // A VideoModel.
    property var model
    property string emptyText
    property bool loading: false
    property alias actions: actionRow.data
    // What to offer instead of an empty grid, under `emptyText`.
    property alias emptyActions: emptyState.actions
    property alias grid: grid
    // Where focus belongs when the page is shown.
    readonly property Item content: grid
    // Whether the empty state sits on a cloud of the veil's dye.
    property bool washEmpty: false
    property alias veil: grid.veil
    // Explains in a tooltip over a card's date why it is approximate: each
    // page knows why.
    property alias estimateNote: grid.estimateNote

    signal activated(var video)
    signal channelRequested(string id)

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 28
            Layout.rightMargin: 28
            Layout.topMargin: 22
            Layout.bottomMargin: 8
            spacing: 12

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 3

                Label {
                    Layout.fillWidth: true
                    text: page.heading
                    elide: Text.ElideRight
                    font.pixelSize: 26
                    font.weight: Font.DemiBold
                }
                Label {
                    Layout.fillWidth: true
                    visible: text !== ""
                    text: page.subheading
                    wrapMode: Text.Wrap
                    font.pixelSize: 13
                    color: "#8d8d98"
                }
            }

            RowLayout {
                id: actionRow
                spacing: 8
            }
        }

        VideoGrid {
            id: grid
            Layout.fillWidth: true
            Layout.fillHeight: true
            focus: true
            model: page.model
            onActivated: video => page.activated(video)
            onChannelRequested: id => page.channelRequested(id)
        }
    }

    EmptyState {
        id: emptyState
        anchors.fill: parent
        visible: !page.loading && page.model.count === 0 && page.emptyText !== ""
        // The first-run welcome sits on a cloud of the veil's dye.
        wash: page.washEmpty
        text: page.emptyText
    }
}
