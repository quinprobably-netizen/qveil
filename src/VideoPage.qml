import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// A heading, optional actions on the right, and a grid of videos.
FocusScope {
    id: page

    property string heading
    property string subheading
    property var model: []
    property string emptyText
    property bool loading: false
    property alias actions: actionRow.data
    property alias grid: grid
    // Where focus belongs when the page is shown.
    readonly property Item content: grid
    property alias veil: grid.veil
    // Explains a "~" in a card's date, where the page knows why dates are estimated.
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

    Label {
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 520)
        visible: !page.loading && page.model.length === 0 && page.emptyText !== ""
        text: page.emptyText
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        font.pixelSize: 15
        lineHeight: 1.3
        color: "#8d8d98"
    }
}
