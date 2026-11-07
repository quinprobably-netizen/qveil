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
    property alias emptyActions: emptyActionRow.data
    property alias grid: grid
    // Where focus belongs when the page is shown.
    readonly property Item content: grid
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

    // A soft cloud of the veil's dye behind the empty state, where the page wants one.
    property bool washEmpty: false
    Rectangle {
        id: washSource
        anchors.fill: emptyWash
        color: "#9a9aa0"
        visible: false
        layer.enabled: true
    }
    DyeWash {
        id: emptyWash
        anchors.centerIn: emptyState
        width: emptyState.width + 260
        height: emptyState.height + 200
        visible: page.washEmpty && emptyState.visible
        source: washSource
        seed: 11.3
        vignette: 1
        opacity: 0.55
    }

    ColumnLayout {
        id: emptyState
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 600)
        visible: !page.loading && page.model.count === 0 && page.emptyText !== ""
        spacing: 0

        Label {
            Layout.fillWidth: true
            text: page.emptyText
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            font.pixelSize: 15
            lineHeight: 1.3
            color: "#8d8d98"
        }

        RowLayout {
            id: emptyActionRow
            Layout.alignment: Qt.AlignHCenter
            // Room for the actions only when the page has any.
            Layout.topMargin: children.length > 0 ? 20 : 0
        }
    }
}
