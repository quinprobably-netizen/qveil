import QtQuick
import Qveil

// The way around the window: the places down the left edge, as glyphs that
// say what they are when pointed at, and at the foot the route (always in
// view: amber while YouTube can see your address, green once it can't) and
// the settings.
Rectangle {
    id: rail

    // The place that is showing, by its key.
    property string current
    // What the engine says about the route in use.
    property string routeSummary
    property string route: "direct"
    signal picked(string place)
    signal routeClicked()

    readonly property var places: [
        { key: "home", label: qsTr("Home"), glyph: "qrc:/icons/home.svg" },
        { key: "lenses", label: qsTr("Lenses"), glyph: "qrc:/icons/lens.svg" },
        { key: "channels", label: qsTr("Channels"), glyph: "qrc:/icons/channels.svg" },
        { key: "history", label: qsTr("History"), glyph: "qrc:/icons/history.svg" },
    ]

    width: Style.rail
    color: Style.bg

    Rule {
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        width: 1
    }

    component Place: Rectangle {
        id: place
        property string label
        property url glyph
        property bool current: false
        property string tip
        // The colour of the glyph and the word; the route's is its own.
        property color tone: current || area.containsMouse ? Style.fg : Style.faint
        default property alias mark: slot.data
        signal clicked()

        width: Style.rail - 14
        height: Style.rail - 14
        radius: Style.radius
        color: current ? Style.sel : area.containsMouse ? Style.hover : "transparent"
        Behavior on color { ColorAnimation { duration: Style.quick } }

        Column {
            anchors.centerIn: parent
            spacing: 5
            Item {
                id: slot
                anchors.horizontalCenter: parent.horizontalCenter
                width: 18
                height: 18
                Glyph {
                    visible: place.glyph.toString() !== ""
                    source: place.glyph
                    color: place.tone
                }
            }
        }
        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: place.clicked()
        }
        Tip {
            // Beside the rail, not under the entry.
            x: parent.width + Style.u * 2
            y: (parent.height - height) / 2
            text: place.tip
            shown: area.containsMouse
        }
    }

    Column {
        anchors.top: parent.top
        anchors.topMargin: Style.u * 4
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: Style.u

        Text {
            objectName: "wordmark"
            anchors.horizontalCenter: parent.horizontalCenter
            bottomPadding: Style.u * 3
            text: "qveil"
            textFormat: Text.PlainText
            color: Style.accent
            font.family: Style.font
            font.pixelSize: 15
            font.weight: Style.strong
        }
        Repeater {
            model: rail.places
            delegate: Place {
                required property var modelData
                objectName: "rail-" + modelData.key
                label: modelData.label
                tip: modelData.label
                glyph: modelData.glyph
                current: rail.current === modelData.key
                onClicked: rail.picked(modelData.key)
            }
        }
    }

    Column {
        anchors.bottom: parent.bottom
        anchors.bottomMargin: Style.u * 3
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: Style.u

        Place {
            id: routePlace
            objectName: "rail-route"
            label: rail.route === "tor" ? qsTr("Tor") : rail.route === "proxy" ? qsTr("Proxy") : qsTr("Direct")
            tone: rail.route === "direct" ? Style.warn : Style.safe
            tip: rail.routeSummary || label
            onClicked: rail.routeClicked()
            Rectangle {
                anchors.centerIn: parent
                width: 8
                height: 8
                radius: 4
                color: routePlace.tone
            }
        }
        Place {
            objectName: "rail-settings"
            label: qsTr("Settings")
            tip: label
            glyph: "qrc:/icons/settings.svg"
            current: rail.current === "settings"
            onClicked: rail.picked("settings")
        }
    }
}
