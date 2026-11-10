import QtQuick
import Qveil

// The bar across the top of the window: the places as words, the search
// field in the middle, and at the far end the route (always in view: amber
// while YouTube can see your address, green once it can't) and the settings.
Rectangle {
    id: bar
    objectName: "bar"

    // The place that is showing, by its key.
    property string current
    // What the engine says about the route in use.
    property string routeSummary
    property string route: "direct"
    // What goes in the middle: the search field.
    default property alias middle: slot.data
    // What the route's popup hangs from.
    readonly property alias routeAnchor: routePlace
    // Too narrow for the route to be spelled out beside its dot.
    readonly property bool tight: width < 900
    signal picked(string place)
    signal routeClicked()

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

        Item {
            id: routePlace
            objectName: "nav-route"
            readonly property string label: bar.route === "tor" ? qsTr("Tor") : bar.route === "proxy" ? qsTr("Proxy") : qsTr("Direct")
            // Amber while YouTube sees your address, green once it doesn't.
            readonly property color tone: bar.route === "direct" ? Style.warn : Style.safe
            readonly property string tip: bar.routeSummary || label
            width: routeRow.width + Style.u * 6
            height: parent.height

            Rectangle {
                anchors.fill: parent
                anchors.topMargin: (bar.height - Style.control) / 2
                anchors.bottomMargin: (bar.height - Style.control) / 2
                radius: Style.radius
                color: routeArea.containsMouse ? Style.hover : "transparent"
                Behavior on color { ColorAnimation { duration: Style.quick } }
            }
            Row {
                id: routeRow
                anchors.centerIn: parent
                spacing: Style.u * 2
                Rectangle {
                    anchors.verticalCenter: parent.verticalCenter
                    width: 8
                    height: 8
                    radius: 4
                    color: routePlace.tone
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: !bar.tight
                    text: routePlace.label
                    textFormat: Text.PlainText
                    color: routePlace.tone
                    font.family: Style.font
                    font.pixelSize: Style.body
                }
            }
            MouseArea {
                id: routeArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: bar.routeClicked()
            }
            Tip {
                text: routePlace.tip
                shown: routeArea.containsMouse
            }
        }
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
