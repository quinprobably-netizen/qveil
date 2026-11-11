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
    // Back to the shelves.
    signal back()
    signal newLens()

    implicitHeight: Style.control

    // Latest, then the lenses (which scroll sideways when there are more
    // than fit), then the way to a new one, which is never pushed out of reach.
    Row {
        id: row
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        spacing: 2

        Act {
            id: home
            objectName: "feed-back"
            glyph: "qrc:/icons/back.svg"
            tip: qsTr("Back to home")
            quiet: true
            onClicked: tabs.back()
        }
        TabWord {
            id: latest
            objectName: "feed-tab-latest"
            underlined: true
            text: qsTr("Latest")
            current: tabs.current === "latest"
            onClicked: tabs.picked("latest")
        }
        Flickable {
            id: strip
            width: Math.max(0, Math.min(lensRow.width,
                                        tabs.width - home.width - latest.width - fresh.width - tail.width - note.width - Style.u * 8))
            height: Style.control
            contentWidth: lensRow.width
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            flickableDirection: Flickable.HorizontalFlick

            Row {
                id: lensRow
                spacing: 2
                Repeater {
                    model: tabs.lenses
                    delegate: TabWord {
                        required property var modelData
                        objectName: "feed-tab-" + modelData.id
                        // A lens can be a long sentence; its tab is the start of it.
                        text: modelData.text.length > 26 ? modelData.text.slice(0, 25) + "\u2026" : modelData.text
                        glyph: "qrc:/icons/lens.svg"
                        underlined: true
                        current: tabs.current === modelData.id
                        onClicked: tabs.picked(modelData.id)
                    }
                }
            }
        }
        Act {
            id: fresh
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
        // The last word stands at the edge, not its button's padding inside it.
        anchors.rightMargin: -Style.actPad
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u
    }
}
