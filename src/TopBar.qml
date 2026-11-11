import QtQuick
import Qveil

// The bar across the top of the window: the places as words, the search
// field in the middle, and at the far end the settings. (The route is on
// the line along the foot of the window: see StatusLine.)
Rectangle {
    id: bar
    objectName: "bar"

    // The place that is showing, by its key.
    property string current
    // What goes in the middle: the search field.
    default property alias middle: slot.data
    // Narrow enough for the places to stand closer.
    readonly property bool tight: width < 900
    // Too narrow for the search field to stand open: it folds into its key,
    // and opens over the places while it is used.
    readonly property bool folded: width - start.width - end.width - Style.gutter * 2 < 240
    // How wide the field is when it opens over the places.
    readonly property real openWidth: slot.x + slot.width - Style.gutter
    signal picked(string place)

    readonly property var places: [
        { key: "home", label: qsTr("Home") },
        { key: "lenses", label: qsTr("Lenses") },
        { key: "channels", label: qsTr("Channels") },
        { key: "history", label: qsTr("History") },
    ]

    height: Style.bar
    color: Style.bg

    Rule {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
    }

    Row {
        id: start
        anchors.left: parent.left
        anchors.leftMargin: Style.gutter
        height: parent.height
        spacing: bar.tight ? Style.u * 2 : Style.u * 4

        Text {
            objectName: "wordmark"
            anchors.verticalCenter: parent.verticalCenter
            text: "qveil"
            textFormat: Text.PlainText
            color: Style.accent
            font.family: Style.font
            font.pixelSize: Style.title - 1
            font.weight: Style.strong
        }
        Row {
            height: parent.height
            Repeater {
                model: bar.places
                delegate: TabWord {
                    required property var modelData
                    objectName: "nav-" + modelData.key
                    height: bar.height
                    underlined: true
                    text: modelData.label
                    current: bar.current === modelData.key
                    onClicked: bar.picked(modelData.key)
                }
            }
        }
    }

    Item {
        id: slot
        anchors.left: start.right
        anchors.right: end.left
        anchors.leftMargin: Style.u * 4
        anchors.rightMargin: Style.u * 4
        height: parent.height
    }

    Row {
        id: end
        anchors.right: parent.right
        anchors.rightMargin: Style.gutter - Style.u * 2
        height: parent.height
        spacing: Style.u

        TabWord {
            objectName: "nav-settings"
            height: bar.height
            underlined: true
            glyph: "qrc:/icons/settings.svg"
            current: bar.current === "settings"
            onClicked: bar.picked("settings")
        }
    }
}
