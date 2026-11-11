import QtQuick
import Qveil
import "Format.js" as Format
import "Arrange.js" as Arrange

// What you watched, when the history is on; and when it is off, what that
// means, in the engine's words.
FocusScope {
    id: page

    readonly property bool on: !!backend.config.history
    // Where focus belongs when the page is shown: what was watched, or,
    // with the history off, the unfinished videos that are kept.
    readonly property Item content: on ? list : kept
    // Whether Clear has been pressed once, and waits for the second.
    property bool arming: false
    property string thumbs: "clear"
    // `list` is what the video was clicked in, and `index` where: what comes
    // next is the rest of it.
    signal playRequested(var video, var list, int index)

    // Plays the video at `index` of what is listed (the history, or what is
    // kept unfinished).
    function play(rows, index) {
        page.playRequested(rows[index], Arrange.listed(rows), index)
    }
    // The unfinished videos as videos: where they were stopped is no part of one.
    readonly property var keptRows: backend.resume.map(k => ({ id: k.id, title: k.title || "", channel: k.channel || "", duration: k.dur }))

    onVisibleChanged: arming = false
    // Turned on or off here, the keyboard goes to the list that is now showing.
    onOnChanged: Qt.callLater(() => { if (page.visible) page.content.forceActiveFocus() })

    Column {
        id: top
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        width: Math.min(parent.width - 2 * Style.gutter, 720)
        spacing: Style.u * 3

        Item {
            width: parent.width
            height: Math.max(title.height, acts.height)
            Text {
                id: title
                anchors.verticalCenter: parent.verticalCenter
                text: qsTr("History")
                textFormat: Text.PlainText
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.head
                font.weight: Style.strong
            }
            Row {
                id: acts
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.u
                visible: page.on
                Act {
                    objectName: "clear-history"
                    visible: backend.history.length > 0
                    text: page.arming ? qsTr("Clear history?") : qsTr("Clear history")
                    onClicked: {
                        if (page.arming)
                            backend.clearHistory()
                        page.arming = !page.arming
                    }
                }
                Act {
                    text: qsTr("Turn off")
                    quiet: true
                    onClicked: backend.setConfig({ history: false })
                }
            }
        }
        // The history's sentence, in the engine's words; off, its switch on the same line.
        Item {
            width: parent.width
            height: Math.max(historyNote.height, turnOn.visible ? turnOn.height : 0)
            Notice {
                id: historyNote
                objectName: "history-note"
                anchors.verticalCenter: parent.verticalCenter
                width: Math.min(implicitWidth, parent.width - (turnOn.visible ? turnOn.width + Style.u * 4 : 0))
                text: (page.on ? backend.privacy.pages.history_on : backend.privacy.pages.history_off) || ""
            }
            Act {
                id: turnOn
                objectName: "history-turn-on"
                anchors.left: historyNote.right
                anchors.leftMargin: Style.u * 4
                anchors.verticalCenter: parent.verticalCenter
                visible: !page.on
                text: qsTr("Turn on")
                filled: true
                onClicked: {
                    backend.setConfig({ history: true })
                    backend.loadHistory()
                }
            }
        }
        Rule {
            width: parent.width
            visible: page.on
        }
        // Off, there is still something kept: where unfinished videos were
        // stopped. It has a heading of its own, and its sentence (the engine's).
        Text {
            objectName: "history-kept-head"
            visible: !page.on && backend.resume.length > 0
            topPadding: Style.u * 2
            text: qsTr("Unfinished")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.title
            font.weight: Style.strong
        }
        Notice {
            objectName: "history-kept-note"
            width: parent.width
            visible: !page.on && backend.resume.length > 0
            text: backend.privacy.pages.resume || ""
        }
    }

    // The unfinished videos that are kept, with their pictures (the same
    // ones home shows for them). j and k move, Enter plays, Delete forgets.
    ListView {
        id: kept
        objectName: "history-kept-list"
        anchors.left: top.left
        anchors.right: parent.right
        anchors.rightMargin: Style.gutter
        anchors.top: top.bottom
        anchors.topMargin: Style.u * 2
        anchors.bottom: parent.bottom
        visible: !page.on
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: page.on ? [] : backend.resume
        Keys.onPressed: event => {
            event.accepted = true
            if (event.key === Qt.Key_J)
                incrementCurrentIndex()
            else if (event.key === Qt.Key_K)
                decrementCurrentIndex()
            else if ((event.key === Qt.Key_Delete || event.key === Qt.Key_Backspace) && currentItem)
                backend.forgetResume(currentItem.modelData.id)
            else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && currentItem)
                currentItem.play()
            else
                event.accepted = false
        }

        delegate: Rectangle {
            id: keptRow
            required property var modelData
            required property int index
            readonly property bool current: ListView.isCurrentItem && kept.activeFocus
            function play() { page.play(page.keptRows, index) }
            objectName: "history-kept"
            width: kept.width
            height: 92
            color: current ? Style.sel : keptArea.containsMouse ? Style.hover : "transparent"
            MouseArea {
                id: keptArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: keptRow.play()
            }
            Item {
                id: keptPicture
                objectName: "history-kept-picture"
                x: Style.u * 2
                anchors.verticalCenter: parent.verticalCenter
                width: 136
                height: 76
                clip: true
                Rectangle {
                    anchors.fill: parent
                    color: Style.rest
                }
                Frost {
                    anchors.fill: parent
                    source: Format.thumb(backend.thumbBase, keptRow.modelData.id, 0)
                    clear: keptRow.current || keptArea.containsMouse
                    mode: page.thumbs
                }
                // How far it was watched.
                Rectangle {
                    anchors.left: parent.left
                    anchors.bottom: parent.bottom
                    width: parent.width * Math.min(1, (keptRow.modelData.pos || 0) / Math.max(1, keptRow.modelData.dur || 1))
                    height: 3
                    color: Style.accent
                }
                // The keyboard's cursor: a frame on the picture, as on a card.
                Rectangle {
                    objectName: "cursor"
                    anchors.fill: parent
                    visible: keptRow.current
                    color: "transparent"
                    border.width: 2
                    border.color: Style.accent
                }
            }
            Column {
                anchors.left: keptPicture.right
                anchors.leftMargin: Style.u * 3
                anchors.right: gone.left
                anchors.rightMargin: Style.u * 3
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    width: parent.width
                    text: keptRow.modelData.title || ""
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.body
                    font.weight: Style.strong
                }
                Row {
                    spacing: Style.u * 2
                    Text {
                        visible: text !== ""
                        text: keptRow.modelData.channel || ""
                        textFormat: Text.PlainText
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                    Text {
                        text: qsTr("%1 left").arg(Format.clock(Math.max(0, (keptRow.modelData.dur || 0) - (keptRow.modelData.pos || 0))))
                        textFormat: Text.PlainText
                        color: Style.accent
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
            }
            Act {
                id: gone
                objectName: "history-forget"
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: qsTr("Forget")
                quiet: true
                onClicked: backend.forgetResume(keptRow.modelData.id)
            }
            Rule {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
            }
        }
    }

    ListView {
        id: list
        objectName: "history-list"
        // j and k move like the arrows.
        Keys.onPressed: event => {
            event.accepted = true
            if (event.key === Qt.Key_J)
                incrementCurrentIndex()
            else if (event.key === Qt.Key_K)
                decrementCurrentIndex()
            else
                event.accepted = false
        }
        anchors.left: top.left
        anchors.right: top.right
        anchors.top: top.bottom
        anchors.bottom: parent.bottom
        visible: page.on
        focus: true
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: page.on ? backend.history : []
        Keys.onReturnPressed: if (currentIndex >= 0) page.play(backend.history, currentIndex)

        delegate: Rectangle {
            id: row
            required property var modelData
            required property int index
            objectName: "history-row"
            width: list.width
            height: 52
            color: area.containsMouse || (ListView.isCurrentItem && list.activeFocus) ? Style.hover : "transparent"

            Column {
                anchors.left: parent.left
                anchors.leftMargin: Style.u * 3
                anchors.right: when.left
                anchors.rightMargin: Style.u * 3
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    width: parent.width
                    text: row.modelData.title
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.body
                }
                Text {
                    width: parent.width
                    visible: text !== ""
                    text: row.modelData.channel || ""
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
            Text {
                id: when
                anchors.right: parent.right
                anchors.rightMargin: Style.u * 3
                anchors.verticalCenter: parent.verticalCenter
                text: Format.age(row.modelData.at)
                textFormat: Text.PlainText
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Rule {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
            }
            MouseArea {
                id: area
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: page.play(backend.history, row.index)
            }
        }
    }
    EmptyState {
        anchors.fill: list
        visible: page.on && backend.history.length === 0
        text: qsTr("Nothing watched yet.")
    }
}
