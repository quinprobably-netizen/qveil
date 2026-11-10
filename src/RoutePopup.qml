import QtQuick
import QtQuick.Controls
import Qveil

// The route, a click away from where it is shown: how qveil reaches
// YouTube, and what the engine says that route does. Every word about
// privacy here is the engine's, shown as it comes.
Popup {
    id: popup

    signal settingsRequested()

    width: 440
    padding: Style.u * 5
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    onAboutToShow: chooser.refresh()

    background: Rectangle {
        color: Style.bg
        radius: Style.radius
        border.width: 1
        border.color: Style.line
        // The fill of a raised surface, over the room's own colour.
        Rectangle {
            anchors.fill: parent
            anchors.margins: 1
            radius: parent.radius
            color: Style.rest
        }
    }

    Column {
        width: parent.width
        spacing: Style.u * 3

        Text {
            text: qsTr("Route")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.title
            font.bold: true
        }
        RouteChooser {
            id: chooser
            width: parent.width
        }
        Rule { width: parent.width }
        // What the engine says about what it does and keeps, sentence by sentence.
        Repeater {
            model: backend.privacy.points
            delegate: Notice {
                required property string modelData
                width: popup.availableWidth
                text: modelData
            }
        }
        Act {
            objectName: "all-settings"
            text: qsTr("All settings")
            filled: true
            onClicked: {
                popup.close()
                popup.settingsRequested()
            }
        }
    }
}
