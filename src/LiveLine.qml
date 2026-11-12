import QtQuick
import QtQuick.Controls
import Qveil
import "Format.js" as Format

// One line under home's lead that names what is live: "● LIVE NOW", then
// each running stream by its channel and title, then what is announced to
// start within a day by its day and time. What doesn't fit on the line is
// under a "+N" at its end, which opens the rest as a list. No pictures.
Rectangle {
    id: line
    objectName: "live-line"

    // Feed entries: the running ones, then the upcoming ones (see Backend.liveNow).
    property var entries: []
    // Which stop the keyboard is on: an entry, or the "+N" after the ones
    // that are shown; −1 when it is not on the line.
    property int current: -1
    signal picked(var video)
    // The pointer moved over the line, at this point of the window.
    signal pointed(point at)

    // How many entries fit, and how many places the keyboard can stop at.
    readonly property int shown: fit()
    readonly property int stops: shown + (shown < entries.length ? 1 : 0)
    readonly property bool opened: rest.opened

    // What an entry says: "Channel · title" while it runs, "Fri 18:00 ·
    // Channel" when it is to start.
    function labelOf(video) {
        if (!video)
            return ""
        const parts = video.live === "now" ? [video.channel, video.title] : [Format.dayTime(video.starts), video.channel]
        return parts.filter(s => !!s).join(" · ")
    }
    // The first part of it, which stands out, and the rest.
    function headOf(video) {
        return video.live === "now" ? (video.channel || video.title || "") : Format.dayTime(video.starts)
    }
    // What Enter does on a stop.
    function activate(at) {
        if (at >= 0 && at < shown)
            picked(entries[at])
        else if (at === shown && shown < entries.length)
            rest.open()
    }

    readonly property real pad: Style.u * 3
    readonly property real gap: Style.u * 3
    readonly property real sepWidth: metrics.advanceWidth("/") + gap
    // What the "+N" takes, at its widest.
    readonly property real moreWidth: metrics.advanceWidth("+" + entries.length) + Style.actPad * 2 + 2
    // The room the entries have, after the mark.
    readonly property real room: width - pad * 2 - mark.implicitWidth
    function fit() {
        const n = entries.length
        let used = 0
        for (let i = 0; i < n; i++) {
            used += gap + (i > 0 ? sepWidth : 0) + Math.ceil(metrics.advanceWidth(labelOf(entries[i]))) + 1
            const after = i + 1 < n ? gap + moreWidth : 0
            if (used + after > room)
                // (The first is always there, cut short if it must be.)
                return Math.max(1, i)
        }
        return n
    }

    visible: entries.length > 0
    implicitHeight: Style.control + Style.u
    color: Style.rest
    radius: Style.radius
    clip: true

    FontMetrics {
        id: metrics
        font.family: Style.font
        font.pixelSize: Style.small
    }

    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.NoButton
        onPositionChanged: mouse => line.pointed(mapToItem(null, mouse.x, mouse.y))
    }

    Row {
        id: row
        x: line.pad
        height: parent.height
        spacing: line.gap

        Text {
            id: mark
            objectName: "live-mark"
            anchors.verticalCenter: parent.verticalCenter
            text: qsTr("● LIVE NOW")
            textFormat: Text.PlainText
            color: Style.accent
            font.family: Style.font
            font.pixelSize: Style.caption
            font.letterSpacing: 1
            font.weight: Font.Bold
        }

        Repeater {
            model: line.entries
            delegate: Item {
                id: entry
                required property var modelData
                required property int index
                objectName: "live-item"
                readonly property string label: line.labelOf(modelData)
                readonly property bool running: modelData.live === "now"
                readonly property bool marked: line.current === index
                visible: index < line.shown
                height: row.height
                // The first is cut short when even it doesn't fit.
                width: Math.min(sep.width + words.implicitWidth,
                                index === 0 ? Math.max(0, line.room - line.gap - (line.shown < line.entries.length ? line.gap + line.moreWidth : 0)) : Infinity)

                Text {
                    id: sep
                    anchors.verticalCenter: parent.verticalCenter
                    width: entry.index > 0 ? implicitWidth + line.gap : 0
                    visible: entry.index > 0
                    text: "/"
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
                Item {
                    id: words
                    anchors.left: sep.right
                    anchors.right: parent.right
                    height: parent.height
                    implicitWidth: head.implicitWidth + tail.implicitWidth + 1
                    Text {
                        id: head
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.min(implicitWidth, parent.width)
                        text: line.headOf(entry.modelData)
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: entry.running || area.containsMouse ? Style.fg : Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    Text {
                        id: tail
                        anchors.left: head.right
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: entry.label.slice(head.text.length)
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: area.containsMouse ? Style.fg : Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    // The keyboard's mark.
                    Rectangle {
                        anchors.fill: parent
                        anchors.topMargin: Style.u
                        anchors.bottomMargin: Style.u
                        anchors.leftMargin: -Style.u * 2
                        anchors.rightMargin: -Style.u * 2
                        visible: entry.marked
                        color: "transparent"
                        radius: Style.radius
                        border.width: 2
                        border.color: Style.accent
                    }
                    MouseArea {
                        id: area
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: line.picked(entry.modelData)
                        onPositionChanged: mouse => line.pointed(mapToItem(null, mouse.x, mouse.y))
                    }
                }
            }
        }

        Act {
            id: more
            objectName: "live-more"
            anchors.verticalCenter: parent.verticalCenter
            visible: line.shown < line.entries.length
            text: "+" + (line.entries.length - line.shown)
            quiet: !rest.opened
            marked: line.current === line.shown && visible
            tip: rest.opened ? "" : qsTr("What else is live or starting today")
            onClicked: rest.opened ? rest.close() : rest.open()
        }
    }

    // The rest, in the same words.
    Popup {
        id: rest
        // Under the "+N", ending where the line ends when there is no room past it.
        x: Math.max(0, Math.min(row.x + more.x, line.width - width))
        y: line.height + Style.u
        width: Math.min(Math.max(240, list.widest + padding * 2), line.width)
        padding: Style.u * 2
        margins: Style.u * 2
        readonly property real zoom: line.Window.window && line.Window.window.zoom ? line.Window.window.zoom : 1
        bottomMargin: Style.u * 2 + (Overlay.overlay ? Overlay.overlay.height * (1 - 1 / zoom) : 0)
        rightMargin: Style.u * 2 + (Overlay.overlay ? Overlay.overlay.width * (1 - 1 / zoom) : 0)
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
        enter: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.quick } }
        exit: Transition { NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Style.quick } }
        // Escape is the window's key: it is told which menu is open (see SortMenu).
        onOpened: {
            if (line.Window.window)
                line.Window.window.openMenu = rest
            list.at = 0
            list.forceActiveFocus()
        }
        onAboutToHide: if (line.Window.window && line.Window.window.openMenu === rest) line.Window.window.openMenu = null
        // The keyboard goes back to what is showing once it has gone (see SortMenu).
        onClosed: if (line.Window.window && typeof line.Window.window.afterPopup === "function") line.Window.window.afterPopup()

        background: Rectangle {
            color: Style.bg
            radius: Style.radius
            border.width: 1
            border.color: Style.line
            Rectangle {
                anchors.fill: parent
                anchors.margins: 1
                radius: parent.radius
                color: Style.rest
            }
        }
        contentItem: Column {
            id: list
            // Which of them the keyboard is on: j and k (up, down) move, Enter opens.
            property int at: 0
            readonly property var others: line.entries.slice(line.shown)
            // As wide as the longest of them.
            readonly property real widest: {
                let most = 0
                for (const video of others)
                    most = Math.max(most, metrics.advanceWidth(line.labelOf(video)))
                return Math.ceil(most) + Style.u * 4 + 2
            }
            function pick(video) {
                rest.close()
                line.picked(video)
            }
            Keys.onPressed: event => {
                event.accepted = true
                if (event.key === Qt.Key_J || event.key === Qt.Key_Down)
                    at = Math.min(others.length - 1, at + 1)
                else if (event.key === Qt.Key_K || event.key === Qt.Key_Up)
                    at = Math.max(0, at - 1)
                else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
                    if (at < others.length)
                        pick(others[at])
                } else
                    event.accepted = false
            }
            Repeater {
                model: rest.opened || rest.visible ? list.others : []
                delegate: Rectangle {
                    id: option
                    required property var modelData
                    required property int index
                    objectName: "live-rest"
                    readonly property string label: line.labelOf(modelData)
                    width: list.width
                    height: Style.control
                    radius: Style.radius
                    color: optionArea.containsMouse ? Style.hover : "transparent"
                    border.width: list.at === index ? 2 : 0
                    border.color: Style.accent
                    Text {
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.leftMargin: Style.u * 2
                        anchors.rightMargin: Style.u * 2
                        anchors.verticalCenter: parent.verticalCenter
                        text: option.label
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: option.modelData.live === "now" || optionArea.containsMouse ? Style.fg : Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    MouseArea {
                        id: optionArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: list.pick(option.modelData)
                    }
                }
            }
        }
    }
}
