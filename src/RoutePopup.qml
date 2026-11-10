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
    // How much of the window's height lies above where it opens (the bar).
    property real above: 0
    // The window's size, in the units it is drawn in.
    property real windowWidth: Overlay.overlay ? Overlay.overlay.width : 600
    property real windowHeight: Overlay.overlay ? Overlay.overlay.height : 600
    readonly property real room: windowHeight - above - 2 * margins - 2 * padding
    width: Math.min(440, windowWidth - 2 * margins)
    padding: Style.u * 5
    // Never over the window's edge, wherever it is opened from.
    margins: Style.u * 2
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    // It arrives and goes briefly.
    enter: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.quick } }
    exit: Transition { NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Style.quick } }
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
            font.weight: Style.strong
        }
        RouteChooser {
            id: chooser
            width: parent.width
            height: implicitHeight
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
