import QtQuick
import QtQuick.Controls
import Qveil

// Every key the window answers to.
Popup {
    id: help
    objectName: "help"

    // Two groups, because the same key does different things in each: what
    // it does while browsing, and what it does while a video is playing.
    readonly property var groups: [
        {
            title: qsTr("Anywhere"),
            keys: [
                [["/", "Ctrl+L"], qsTr("Search")],
                [["1", "2", "3", "4", "5"], qsTr("Home, lenses, channels, history, settings")],
                [["H", "J", "K", "L"], qsTr("Move, like the arrows (while browsing)")],
                [["S"], qsTr("Sort and filter what the keyboard is on")],
                [["O"], qsTr("Open the shelf in full")],
                [["U"], qsTr("Unsubscribe (on the channels page; asked twice)")],
                [["R"], qsTr("Refresh what is showing")],
                [["P"], qsTr("Route")],
                [["Esc"], qsTr("Back")],
                [["?"], qsTr("This help")],
                [["Q"], qsTr("Quit")],
            ]
        },
        {
            title: qsTr("Playing"),
            keys: [
                [["Space"], qsTr("Play or pause (browsing: play the selected video)")],
                [["Left", "Right"], qsTr("Back or forward 5 seconds")],
                [["J", "L"], qsTr("Back or forward 10 seconds")],
                [["[", "]"], qsTr("Previous or next chapter")],
                [["<", ">"], qsTr("Slower or faster")],
                [["Up", "Down"], qsTr("Volume")],
                [["M"], qsTr("Mute")],
                [["I"], qsTr("Picture in picture, and back")],
                [["T"], qsTr("Theatre")],
                [["F"], qsTr("Fullscreen")],
                [["C"], qsTr("Comments, with the keyboard in the sheet (Esc gives it back)")],
                [["N"], qsTr("Next video")],
            ]
        },
    ]
    // Every key that is listed, for checking against what is bound.
    readonly property var listed: {
        const all = []
        for (const group of groups)
            for (const entry of group.keys)
                for (const key of entry[0])
                    all.push(key)
        return all
    }
    // How a key is written: arrows as arrows.
    function shown(key) {
        return ({ Left: "\u2190", Right: "\u2192", Up: "\u2191", Down: "\u2193" })[key] || key
    }

    // The window's width, in the units it is drawn in.
    property real windowWidth: Overlay.overlay ? Overlay.overlay.width : 600
    // And its height.
    property real windowHeight: Overlay.overlay ? Overlay.overlay.height : 600
    // Its two groups stand side by side where there is room for that.
    readonly property bool wide: windowWidth >= 760
    anchors.centerIn: parent
    width: Math.min(wide ? 880 : 560, windowWidth - Style.gutter * 2)
    padding: Style.u * 5
    modal: true
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    // It arrives and goes briefly.
    enter: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.quick } }
    exit: Transition { NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Style.quick } }
    Overlay.modal: Rectangle { color: Style.scrim }

    background: Rectangle {
        color: Style.bg
        radius: Style.radius
        border.width: 1
        border.color: Style.line
    }

    // Never taller than the window: what doesn't fit scrolls.
    contentItem: Flickable {
        implicitHeight: Math.min(all.height, help.windowHeight - Style.gutter * 2 - 2 * help.padding)
        contentHeight: all.height
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        Column {
            id: all
            width: parent.width
            spacing: Style.u * 3

            Text {
                text: qsTr("Keyboard")
                textFormat: Text.PlainText
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.title
                font.weight: Style.strong
            }
            Grid {
                columns: help.wide ? 2 : 1
                columnSpacing: Style.u * 8
                rowSpacing: Style.u * 3
                Repeater {
                    model: help.groups
                    delegate: Column {
                        id: group
                        required property var modelData
                        required property int index
                        objectName: "help-group-" + index
                        width: help.wide ? (all.width - Style.u * 8) / 2 : all.width
                        spacing: Style.u
                        SectionHead {
                            width: parent.width
                            text: group.modelData.title
                        }
                        Repeater {
                            model: group.modelData.keys
                            delegate: Item {
                                required property var modelData
                                width: group.width
                                height: Style.control - 6
                                Text {
                                    width: 96
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData[0].map(help.shown).join("  ")
                                    textFormat: Text.PlainText
                                    color: Style.accent
                                    font.family: Style.font
                                    font.pixelSize: Style.small
                                }
                                Text {
                                    x: 104
                                    width: parent.width - 104
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData[1]
                                    textFormat: Text.PlainText
                                    elide: Text.ElideRight
                                    color: Style.fg
                                    font.family: Style.font
                                    font.pixelSize: Style.small
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
