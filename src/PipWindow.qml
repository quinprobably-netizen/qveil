import QtQuick
import Qveil

// Picture in picture: the video in a small window of its own, so the main
// window can go on browsing. Omarchy floats and pins a window with this
// title; elsewhere it is an ordinary small window that asks to stay on top.
Window {
    id: pip
    objectName: "pip"

    readonly property alias surface: surface
    property string thumbs: "frosted"
    property real savedVolume: 80
    signal backRequested()

    // Takes over a video: `info` is the engine's play answer, `at` the
    // second to carry on from.
    function take(info, at) {
        surface.openAt(info, at)
        visible = true
    }
    // Takes over the video the page's surface `from` is playing, with its
    // player (see PlayerSurface.adopt). False if it had none to give.
    // (The player first: mpv starts on it while the window is being mapped.)
    function adopt(from) {
        if (!surface.adopt(from))
            return false
        visible = true
        return true
    }
    // Stops the video and goes away.
    function end() {
        surface.stop()
        visible = false
    }

    title: "Picture-in-Picture"
    width: 600
    height: 338
    minimumWidth: 320
    minimumHeight: 180
    flags: Qt.Window | Qt.WindowStaysOnTopHint
    color: "black"
    visible: false
    // Closed by the window manager: the video stops with it.
    onClosing: surface.stop()

    PlayerSurface {
        id: surface
        objectName: "pip-surface"
        anchors.fill: parent
        compact: true
        // Hidden, this window lets go of what it drew with (see MpvItem).
        mpv.releaseWhenHidden: true
        thumbs: pip.thumbs
        savedVolume: pip.savedVolume
        onSavedVolumeChanged: pip.savedVolume = savedVolume
        onPipRequested: pip.backRequested()
        // Stopped from anywhere (its own Close, the engine going away): no
        // video, no window. A player handed back is drawn here until the
        // page has it (see MpvItem::takeOver), so the window stays till then.
        onClosed: if (!surface.mpv.handingOver) pip.visible = false
    }

    Connections {
        target: surface.mpv
        function onHandingOverChanged() {
            if (!surface.mpv.handingOver && !surface.active)
                pip.visible = false
        }
    }

    Row {
        anchors.right: parent.right
        anchors.top: parent.top
        opacity: surface.chrome && surface.active ? 1 : 0
        visible: opacity > 0
        Behavior on opacity { NumberAnimation { duration: 220 } }
        Rectangle {
            width: actions.width
            height: actions.height
            color: Style.scrim
            Row {
                id: actions
                Act {
                    objectName: "pip-back"
                    text: qsTr("Back to qveil")
                    onClicked: pip.backRequested()
                }
                Act {
                    objectName: "pip-close"
                    glyph: "qrc:/icons/close.svg"
                    tip: qsTr("Close")
                    onClicked: pip.end()
                }
            }
        }
    }
}
