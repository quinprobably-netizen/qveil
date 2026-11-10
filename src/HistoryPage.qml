import QtQuick
import Qveil
import "Format.js" as Format

// What you watched, when the history is on; and when it is off, what that
// means, in the engine's words.
FocusScope {
    id: page

    readonly property bool on: !!backend.config.history
    // Where focus belongs when the page is shown.
    readonly property Item content: list
    // Whether Clear has been pressed once, and waits for the second.
    property bool arming: false
    signal playRequested(var video)

    onVisibleChanged: arming = false

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
        Notice {
            objectName: "history-note"
            width: parent.width
            text: (page.on ? backend.privacy.pages.history_on : backend.privacy.pages.history_off) || ""
        }
        Act {
            objectName: "history-turn-on"
            visible: !page.on
            text: qsTr("Turn on")
            filled: true
            onClicked: {
                backend.setConfig({ history: true })
                backend.loadHistory()
            }
        }
        Rule {
            width: parent.width
            visible: page.on
        }
        // Off, there is still something kept: where unfinished videos were
        // stopped. It is said here, in the engine's words, and can be forgotten.
        Notice {
            objectName: "history-kept-note"
            width: parent.width
            visible: !page.on && backend.resume.length > 0
            text: backend.privacy.pages.resume || ""
        }
        Column {
            width: parent.width
            visible: !page.on
            Repeater {
                model: page.on ? [] : backend.resume
                delegate: Rectangle {
                    id: kept
                    required property var modelData
                    objectName: "history-kept"
                    width: parent.width
                    height: 52
                    color: keptArea.containsMouse ? Style.hover : "transparent"
                    MouseArea {
                        id: keptArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: page.playRequested({ id: kept.modelData.id, title: kept.modelData.title || "", channel: kept.modelData.channel || "" })
                    }
                    Column {
                        anchors.left: parent.left
                        anchors.leftMargin: Style.u * 3
                        anchors.right: gone.left
                        anchors.rightMargin: Style.u * 3
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 2
                        Text {
                            width: parent.width
                            text: kept.modelData.title || ""
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            color: Style.fg
                            font.family: Style.font
                            font.pixelSize: Style.body
                        }
                        Text {
                            width: parent.width
                            text: [kept.modelData.channel || "", qsTr("%1 left").arg(Format.clock(Math.max(0, kept.modelData.dur - kept.modelData.pos)))]
                                      .filter(s => s !== "").join("  ·  ")
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            color: Style.dim
                            font.family: Style.font
                            font.pixelSize: Style.small
                        }
                    }
                    Act {
                        id: gone
                        objectName: "history-forget"
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: qsTr("Forget")
                        quiet: true
                        onClicked: backend.forgetResume(kept.modelData.id)
                    }
                    Rule {
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.bottom: parent.bottom
                    }
                }
            }
        }
    }

    // The history is asked for again when it has just been turned on.
    Connections {
        target: backend
        function onConfigChanged() {
            if (page.visible && page.on)
                backend.loadHistory()
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
        Keys.onReturnPressed: if (currentItem) page.playRequested(currentItem.modelData)

        delegate: Rectangle {
            id: row
            required property var modelData
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
                onClicked: page.playRequested(row.modelData)
            }
        }
    }
    EmptyState {
        anchors.fill: list
        visible: page.on && backend.history.length === 0
        text: qsTr("Nothing watched yet.")
    }
}
