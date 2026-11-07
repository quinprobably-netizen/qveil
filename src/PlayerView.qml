import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import Veil
import "Format.js" as Format

// The video, with controls that fade out while it plays.
FocusScope {
    id: view

    property bool active: false
    property var info: ({})
    property bool chrome: true
    property alias mpv: mpv
    property real savedVolume: 80

    signal closed()
    signal channelRequested(string id)
    signal fullscreenToggled()

    visible: active

    function open(i) {
        info = i
        active = true
        mpv.volume = savedVolume
        mpv.load(i.video, i.audio || "", i.start || 0)
        reveal()
        forceActiveFocus()
    }

    function close() {
        mpv.stop()
        active = false
        info = ({})
        closed()
    }

    function reveal() {
        chrome = true
        hideTimer.restart()
    }

    Rectangle {
        anchors.fill: parent
        color: "black"
    }

    MpvItem {
        id: mpv
        anchors.fill: parent
        onFailed: message => backend.reportError(message)
        onVolumeChanged: view.savedVolume = volume
        onPausedChanged: view.reveal()
    }

    BusyIndicator {
        anchors.centerIn: parent
        running: view.active && mpv.buffering
    }

    Timer {
        id: hideTimer
        interval: 2600
        onTriggered: {
            if (!mpv.paused && !mpv.ended && !seekBar.pressed && !controlsHover.hovered)
                view.chrome = false
        }
    }

    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: view.chrome ? Qt.ArrowCursor : Qt.BlankCursor
        onPositionChanged: view.reveal()
        onClicked: mpv.togglePause()
        onDoubleClicked: view.fullscreenToggled()
    }

    // Replay when it's over.
    RoundButton {
        anchors.centerIn: parent
        visible: mpv.ended
        width: 72
        height: 72
        icon.source: "qrc:/icons/replay.svg"
        icon.width: 32
        icon.height: 32
        highlighted: true
        onClicked: mpv.togglePause()
    }

    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: 110
        opacity: view.chrome ? 1 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 220 } }
        gradient: Gradient {
            GradientStop { position: 0; color: "#d0000000" }
            GradientStop { position: 1; color: "transparent" }
        }

        RowLayout {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: 14
            spacing: 10

            ToolButton {
                icon.source: "qrc:/icons/back.svg"
                onClicked: view.close()
                ToolTip.text: qsTr("Back (Esc)")
                ToolTip.visible: hovered
                ToolTip.delay: 500
            }

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 1

                Label {
                    Layout.fillWidth: true
                    text: view.info.title || ""
                    elide: Text.ElideRight
                    font.pixelSize: 18
                    font.weight: Font.DemiBold
                }
                Label {
                    text: view.info.channel || ""
                    font.pixelSize: 13
                    color: channelHover.hovered ? theme.accent : "#c0c0ca"
                    HoverHandler {
                        id: channelHover
                        enabled: !!view.info.channelId
                        cursorShape: Qt.PointingHandCursor
                    }
                    MouseArea {
                        anchors.fill: parent
                        enabled: !!view.info.channelId
                        onClicked: view.channelRequested(view.info.channelId)
                    }
                }
            }
        }
    }

    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: 124
        opacity: view.chrome ? 1 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 220 } }
        gradient: Gradient {
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 1; color: "#e0000000" }
        }

        HoverHandler { id: controlsHover }

        ColumnLayout {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            anchors.bottomMargin: 8
            spacing: 0

            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: seekBar.implicitHeight

                // What's buffered ahead, under the seek bar.
                Rectangle {
                    x: seekBar.leftPadding
                    anchors.verticalCenter: parent.verticalCenter
                    width: (seekBar.availableWidth) * Math.min(1, mpv.cached / Math.max(1, mpv.duration))
                    height: 4
                    radius: 2
                    color: "#40ffffff"
                }

                Slider {
                    id: seekBar
                    anchors.fill: parent
                    from: 0
                    to: Math.max(1, mpv.duration)
                    onMoved: mpv.seek(value, false)
                }
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: 4

                ToolButton {
                    icon.source: mpv.paused || mpv.ended ? "qrc:/icons/play.svg" : "qrc:/icons/pause.svg"
                    onClicked: mpv.togglePause()
                }

                Label {
                    text: Format.clock(mpv.position) + "  /  " + Format.clock(mpv.duration)
                    font.pixelSize: 13
                    font.features: { "tnum": 1 }
                    color: "#d8d8de"
                }

                Item { Layout.fillWidth: true }

                ToolButton {
                    icon.source: mpv.muted || mpv.volume === 0 ? "qrc:/icons/mute.svg" : "qrc:/icons/volume.svg"
                    onClicked: mpv.muted = !mpv.muted
                }

                Slider {
                    id: volumeBar
                    Layout.preferredWidth: 120
                    from: 0
                    to: 100
                    onMoved: {
                        mpv.volume = value
                        mpv.muted = false
                    }
                }

                ToolButton {
                    icon.source: Window.window && Window.window.visibility === Window.FullScreen
                                 ? "qrc:/icons/fullscreen-exit.svg" : "qrc:/icons/fullscreen.svg"
                    onClicked: view.fullscreenToggled()
                }
            }
        }
    }

    // Sliders are driven from mpv without bindings, which dragging would break.
    Connections {
        target: mpv
        function onPositionChanged() {
            if (!seekBar.pressed)
                seekBar.value = mpv.position
        }
        function onVolumeChanged() {
            if (!volumeBar.pressed)
                volumeBar.value = mpv.volume
        }
    }
}
