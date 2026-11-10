import QtQuick
import Qveil

// What the home page shows: the latest from the channels you follow, or one
// of your lenses. At the far end, the things that act on it.
Item {
    id: tabs

    // api/lenses as it is.
    property var lenses: []
    // "latest", or a lens's id.
    property string current: "latest"
    // A line about what is loading or loaded, before the actions.
    property string status
    default property alias actions: tail.data
    signal picked(string which)
    signal newLens()

    implicitHeight: Style.control

    Row {
        id: row
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        spacing: 2
        // The tabs give way to the actions, never the other way round.
        width: Math.max(0, parent.width - tail.width - note.width - Style.u * 6)
        clip: true

        TabWord {
            objectName: "feed-tab-latest"
            text: qsTr("Latest")
            current: tabs.current === "latest"
            onClicked: tabs.picked("latest")
        }
        Repeater {
            model: tabs.lenses
            delegate: TabWord {
                required property var modelData
                objectName: "feed-tab-" + modelData.id
                // A lens can be a long sentence; its tab is the start of it.
                text: modelData.text.length > 30 ? modelData.text.slice(0, 29) + "…" : modelData.text
                glyph: "qrc:/icons/lens.svg"
                current: tabs.current === modelData.id
                onClicked: tabs.picked(modelData.id)
            }
        }
        Act {
            objectName: "feed-tab-new"
            text: qsTr("New lens")
            glyph: "qrc:/icons/plus.svg"
            quiet: true
            onClicked: tabs.newLens()
        }
    }
    Text {
        id: note
        anchors.right: tail.left
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        text: tabs.status
        textFormat: Text.PlainText
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
    Row {
        id: tail
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u
    }
}
