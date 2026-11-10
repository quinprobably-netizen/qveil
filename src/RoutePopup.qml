import QtQuick
import QtQuick.Controls
import Qveil

// The route, a click away from where it is shown: how qveil reaches
// YouTube, and what the engine says that route does. Every word about
// privacy here is the engine's, shown as it comes.
Popup {
    id: popup

    signal settingsRequested()

    // How tall what is in it may be, for the popup to fit the window.
    readonly property real room: (Overlay.overlay ? Overlay.overlay.height : 600) - 2 * margins - 2 * padding
    width: Math.min(440, (Overlay.overlay ? Overlay.overlay.width : 600) - 2 * margins)
    padding: Style.u * 5
    // Never over the window's edge, wherever it is opened from.
    margins: Style.u * 2
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

    contentItem: Column {
        id: column
        spacing: Style.u * 3

        Text {
            id: title
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
        // What the engine says about what it does and keeps, sentence by
        // sentence. There is a lot of it: it scrolls, so that the route and
        // the way to the settings stay in reach.
        Flickable {
            width: parent.width
            height: Math.max(48, Math.min(points.height,
                popup.room - title.height - chooser.height - 1 - settings.height - 4 * column.spacing))
            contentHeight: points.height
            clip: true
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: points
                width: parent.width
                spacing: Style.u * 3
                Repeater {
                    model: backend.privacy.points
                    delegate: Notice {
                        required property string modelData
                        width: points.width
                        text: modelData
                    }
                }
            }
        }
        Act {
            id: settings
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
