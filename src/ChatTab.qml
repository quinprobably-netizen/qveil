import QtQuick
import Qveil

// A running stream's live chat (T1): read only, and text only. No pictures
// are fetched for it: no avatars, badges or emoji images (custom emoji come
// as their :name:). It is read only while this tab shows, and a new reading
// starts empty: what was missed meanwhile is not fetched.
Item {
    id: tab
    objectName: "chat"

    // The stream's video ID.
    property string video
    // Whether the tab is showing, for a stream that runs.
    property bool active: false
    // Whether the keyboard is in the sheet.
    property bool keyed: false
    // How many messages are kept: the oldest go first.
    readonly property int keep: 300
    readonly property alias count: messages.count
    // "" while it isn't read, "open" while it is, "ended" once the engine
    // said it is over, "failed" when it couldn't be read.
    property string phase: ""
    // Whether the list keeps to the newest message: until it is scrolled up,
    // and again once it is back at the foot.
    property bool follow: true

    // The video it is read for, while it is wanted.
    readonly property string wanted: active ? video : ""
    onWantedChanged: wanted !== "" ? start() : stop()
    Component.onDestruction: stop()

    function start() {
        messages.clear()
        follow = true
        phase = "open"
        backend.chat(wanted)
    }
    function stop() {
        if (phase === "open")
            backend.stopChat()
        phase = ""
    }

    // The keyboard scrolls it; Enter goes back to the newest.
    function step(by) {
        list.contentY = Math.max(list.originY, Math.min(list.originY + Math.max(0, list.contentHeight - list.height),
                                                        list.contentY + by * 96))
        follow = atEnd()
    }
    function activate() {
        follow = true
        list.positionViewAtEnd()
    }
    function atEnd() { return list.contentY + list.height >= list.originY + list.contentHeight - 2 }
    function keepUp() {
        if (follow && !list.moving)
            list.positionViewAtEnd()
    }

    ListModel { id: messages }
    Connections {
        target: backend
        function onChatLines(lines) {
            if (tab.phase !== "open")
                return
            for (const line of lines)
                messages.append({ author: line.author, message: line.text, owner: line.owner, amount: line.amount })
            if (messages.count > tab.keep)
                messages.remove(0, messages.count - tab.keep)
            Qt.callLater(tab.keepUp)
        }
        function onChatEnded() {
            if (tab.phase === "open")
                tab.phase = "ended"
        }
        function onChatFailed(message, code) {
            if (tab.phase === "open")
                tab.phase = "failed"
        }
    }

    BusyLine {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        opacity: tab.phase === "open" && messages.count === 0 ? 1 : 0
    }
    Text {
        objectName: "chat-waiting"
        x: Style.u * 3
        y: Style.u * 4
        width: parent.width - Style.u * 6
        visible: tab.phase === "open" && messages.count === 0
        text: qsTr("Waiting for messages…")
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: Style.dim
        font.family: Style.font
        font.pixelSize: Style.small
    }

    // The messages, the newest at the foot. While there are few, they stand
    // at the foot too, over the footer. Once they fill the tab, the oldest
    // shown is cut a little under the tabs' rule, not right at it, and a
    // whole line at a time: a line is shown in full or not at all.
    ListView {
        id: list
        objectName: "chat-list"
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: note.visible ? note.top : foot.top
        height: Math.min(contentHeight + topMargin + bottomMargin, (note.visible ? note.y : foot.y) - Style.u * 2)
        topMargin: Style.u * 2
        bottomMargin: Style.u * 2
        clip: true
        spacing: Style.u * 3 / 2
        boundsBehavior: Flickable.StopAtBounds
        model: messages
        onMovementEnded: tab.follow = tab.atEnd()
        onContentHeightChanged: tab.keepUp()
        onHeightChanged: tab.keepUp()

        delegate: Item {
            id: line
            required property string author
            required property string message
            required property bool owner
            required property string amount
            readonly property bool paid: amount !== ""
            // Where the words start on the first line: after the amount and the name.
            readonly property real lead: head.width + Style.u * 3 / 2
            // (The name's width is known after the words were first laid out.)
            onLeadChanged: words.forceLayout()
            objectName: "chat-message"
            width: ListView.view.width
            height: words.height
            // How tall one of its lines is.
            readonly property real lineStep: words.height / Math.max(1, words.lineCount)
            // How much of it is past the top and the foot of the list, in
            // whole lines: what is left of a line that is partly out goes too.
            readonly property real topCut: wholeLines(ListView.view.contentY - y)
            readonly property real footCut: wholeLines(y + height - ListView.view.contentY - ListView.view.height)
            function wholeLines(out) {
                return out > 0.5 ? Math.min(height, Math.ceil((out - 0.5) / lineStep) * lineStep) : 0
            }

            Item {
                id: shown
                objectName: "chat-shown"
                y: line.topCut
                width: parent.width
                height: Math.max(0, line.height - line.topCut - line.footCut)
                clip: line.topCut > 0 || line.footCut > 0
                // (Where it would be uncut.)
                Item {
                    y: -line.topCut
                    width: line.width
                    height: line.height

                    // A Super Chat's mark.
                    Rectangle {
                        objectName: "chat-rule"
                        x: Style.u * 3
                        width: 2
                        height: parent.height
                        visible: line.paid
                        color: Style.warn
                    }
                    Row {
                        id: head
                        x: words.x
                        spacing: Style.u * 3 / 2
                        Text {
                            objectName: "chat-amount"
                            visible: line.paid
                            text: line.amount
                            textFormat: Text.PlainText
                            color: Style.warn
                            font.family: Style.font
                            font.pixelSize: Style.small
                            font.weight: Style.strong
                        }
                        Text {
                            objectName: "chat-author"
                            // A long name leaves room for the words.
                            width: Math.min(implicitWidth, words.width * 0.6)
                            text: line.author
                            textFormat: Text.PlainText
                            elide: Text.ElideRight
                            // The channel's owner stands out; everyone else is muted.
                            color: line.owner ? Style.accent : Style.faint
                            font.family: Style.font
                            font.pixelSize: Style.small
                        }
                    }
                    // The words, as they were written: plain text, never markup.
                    // Their first line starts after the name.
                    Text {
                        id: words
                        objectName: "chat-text"
                        x: Style.u * 3 + (line.paid ? 2 + Style.u * 2 : 0)
                        width: parent.width - x - Style.u * 3
                        // Where its first line starts, as it was last laid out.
                        property real indent: 0
                        text: line.message
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.small
                        onLineLaidOut: l => {
                            if (l.number === 0) {
                                l.x = line.lead
                                l.width = Math.max(1, width - line.lead)
                                indent = l.x
                            }
                        }
                    }
                }
            }
        }
    }

    // Why it isn't being read any more.
    Item {
        id: note
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: foot.top
        visible: tab.phase === "ended" || tab.phase === "failed"
        height: visible ? Style.control + Style.u * 2 : 0
        Row {
            x: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.u
            Text {
                objectName: "chat-note"
                anchors.verticalCenter: parent.verticalCenter
                text: tab.phase === "failed" ? qsTr("The chat couldn't be read.") : qsTr("The chat has ended.")
                textFormat: Text.PlainText
                color: Style.dim
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Act {
                objectName: "chat-retry"
                anchors.verticalCenter: parent.verticalCenter
                visible: tab.phase === "failed"
                text: qsTr("Try again")
                activeFocusOnTab: false
                onClicked: tab.start()
            }
        }
    }

    // What reading it costs: nothing but text.
    Item {
        id: foot
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: footer.implicitHeight + Style.u * 3
        Rule {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
        }
        Text {
            id: footer
            objectName: "chat-footer"
            x: Style.u * 3
            width: parent.width - Style.u * 6
            anchors.verticalCenter: parent.verticalCenter
            text: qsTr("Read only · text, no pictures fetched")
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.caption
        }
    }
}
