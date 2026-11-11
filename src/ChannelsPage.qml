import QtQuick
import Qveil
import "Format.js" as Format
import "Arrange.js" as Arrange

// The channels you follow, in two panes: the channels down one side, and
// beside them the latest from the one the keyboard is on. What the second
// pane shows is what qveil already has from the feed: it asks for nothing.
FocusScope {
    id: page

    property string thumbs: "clear"
    property var progress: ({})
    // Where focus belongs when the page is shown.
    readonly property Item content: list
    signal channelRequested(string id)
    // A video in the pane: `list` is what comes after it.
    signal activated(var video, var list, int index)

    // The feed's videos, by channel (named for its count, so that it follows the feed).
    readonly property var byChannel: {
        const found = {}
        for (let i = 0; i < backend.feed.count; i++) {
            const video = backend.feed.get(i)
            if (!found[video.channel_id])
                found[video.channel_id] = []
            found[video.channel_id].push(video)
        }
        return found
    }
    readonly property var chosen: list.currentIndex >= 0 && list.currentIndex < backend.subs.length
        ? backend.subs[list.currentIndex] : ({})
    // Whether a channel is being added.
    property bool adding: false

    function focusAdd() {
        adding = true
        add.edit()
    }
    function submit(text) {
        backend.subscribe(text)
        add.text = ""
        adding = false
        list.forceActiveFocus()
    }

    Item {
        id: rail
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        anchors.leftMargin: Style.gutter
        anchors.topMargin: Style.u * 2
        anchors.bottomMargin: Style.gutter
        width: page.width < 700 ? 200 : 260

        Column {
            id: railHead
            width: parent.width
            spacing: 2
            Item {
                width: parent.width
                height: Style.control + Style.u
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: qsTr("Channels")
                    textFormat: Text.PlainText
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.title
                    font.weight: Style.strong
                }
                Text {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: backend.subs.length
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
            // What following a channel means, in the engine's words.
            Notice {
                objectName: "channels-note"
                width: parent.width
                text: backend.privacy.pages.subscriptions || ""
                color: Style.faint
            }
        }

        ListView {
            id: list
            objectName: "channel-list"
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: railHead.bottom
            anchors.topMargin: Style.u * 2
            height: Math.min(contentHeight, parent.height - railHead.height - Style.u * 2 - more.height - Style.u * 2)
            focus: true
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            keyNavigationEnabled: true
            model: backend.subs
            // Which row Unsubscribe has been asked of once, by the keyboard.
            property int arming: -1
            onCurrentIndexChanged: arming = -1
            // j and k move like the arrows; right goes into the pane; u lets
            // the channel go (asked twice); Enter opens its page.
            Keys.onPressed: event => {
                event.accepted = true
                if (event.key === Qt.Key_J)
                    incrementCurrentIndex()
                else if (event.key === Qt.Key_K)
                    decrementCurrentIndex()
                else if (event.key === Qt.Key_L || event.key === Qt.Key_Right)
                    pane.forceActiveFocus()
                else if (event.key === Qt.Key_U && currentIndex >= 0) {
                    if (arming === currentIndex) {
                        arming = -1
                        backend.unsubscribe(page.chosen.id)
                    } else {
                        arming = currentIndex
                    }
                } else
                    event.accepted = false
            }
            Keys.onReturnPressed: if (page.chosen.id) page.channelRequested(page.chosen.id)

            delegate: Rectangle {
                id: row
                required property var modelData
                required property int index
                readonly property bool current: ListView.isCurrentItem
                // Whether Unsubscribe has been asked once, and waits for the second.
                property bool armed: false
                readonly property bool arming: armed || list.arming === index
                readonly property var latest: (page.byChannel[modelData.id] || [])[0]
                objectName: "channel-row"
                width: list.width
                height: 52
                color: current ? Style.sel : area.containsMouse ? Style.hover : "transparent"

                MouseArea {
                    id: area
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        list.currentIndex = row.index
                        list.forceActiveFocus()
                    }
                    onDoubleClicked: page.channelRequested(row.modelData.id)
                    onExited: row.armed = false
                }
                // Its latest video's picture, which is already here.
                Item {
                    id: picture
                    x: Style.u * 2
                    anchors.verticalCenter: parent.verticalCenter
                    width: 40
                    height: 40
                    clip: true
                    Rectangle {
                        anchors.fill: parent
                        color: Style.rest
                    }
                    Frost {
                        objectName: "channel-picture"
                        anchors.fill: parent
                        source: row.latest ? Format.thumb(backend.thumbBase, row.latest.id, row.latest.batch || 0) : ""
                        clear: row.current || area.containsMouse
                        mode: page.thumbs
                    }
                }
                Column {
                    anchors.left: picture.right
                    anchors.leftMargin: Style.u * 3
                    anchors.right: leave.visible ? leave.left : parent.right
                    anchors.rightMargin: Style.u * 2
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Text {
                        width: parent.width
                        text: row.modelData.name
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.body
                        font.weight: row.current ? Style.strong : Font.Normal
                    }
                    Text {
                        width: parent.width
                        visible: text !== ""
                        text: row.latest ? Format.age(row.latest.date || row.latest.published || 0) : ""
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.faint
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
                Act {
                    id: leave
                    objectName: "unsubscribe"
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    visible: area.containsMouse || hot || row.arming
                    text: row.arming ? qsTr("Unsubscribe?") : qsTr("Unsubscribe")
                    quiet: !row.arming
                    onClicked: {
                        if (row.arming) {
                            list.arming = -1
                            backend.unsubscribe(row.modelData.id)
                        }
                        row.armed = !row.arming
                    }
                }
            }
        }
        Column {
            id: more
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: list.bottom
            anchors.topMargin: Style.u * 2
            spacing: Style.u * 2

            Act {
                objectName: "channel-add-open"
                visible: !page.adding
                text: qsTr("Add a channel")
                glyph: "qrc:/icons/plus.svg"
                quiet: true
                onClicked: page.focusAdd()
            }
            Field {
                id: add
                objectName: "channel-add"
                visible: page.adding
                width: parent.width
                placeholder: qsTr("@handle, link or ID")
                onSubmitted: text => page.submit(text)
                onEscaped: {
                    page.adding = false
                    list.forceActiveFocus()
                }
            }
            Notice {
                width: parent.width
                visible: backend.subs.length === 0
                color: Style.faint
                text: qsTr("Moving from YouTube? Drop your Google Takeout subscriptions.csv, or a NewPipe or FreeTube export, anywhere on this window.")
            }
        }
    }

    // The latest of the channel the keyboard is on.
    Item {
        anchors.left: rail.right
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        anchors.leftMargin: Style.u * 4
        anchors.topMargin: Style.u * 2

        Item {
            id: paneHead
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: Style.u * 2
            anchors.rightMargin: Style.gutter
            height: Style.control + Style.u
            Text {
                objectName: "channel-pane-name"
                anchors.left: parent.left
                anchors.right: keys.left
                anchors.rightMargin: Style.u * 3
                anchors.verticalCenter: parent.verticalCenter
                text: page.chosen.name || ""
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.title
                font.weight: Style.strong
            }
            // What the keys do with this channel.
            Text {
                id: keys
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                visible: !!page.chosen.id
                text: qsTr("Enter  open    u  unsubscribe")
                textFormat: Text.PlainText
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
        }
        VideoModel { id: theirs }
        readonly property var videos: page.byChannel[page.chosen.id] || []
        onVideosChanged: theirs.assign(videos)

        VideoGrid {
            id: pane
            objectName: "channel-pane"
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: paneHead.bottom
            anchors.bottom: parent.bottom
            anchors.topMargin: Style.u
            model: theirs
            thumbs: page.thumbs
            progress: page.progress
            estimateNote: qsTr("Approximate: YouTube's feed for this channel was unavailable")
            onActivated: (video, index) => page.activated(video, theirs, index)
            // Left (or h) at its edge goes back to the channels.
            leftExits: true
            onLeftEdge: list.forceActiveFocus()
        }
        EmptyState {
            anchors.fill: pane
            visible: pane.count === 0 && !!page.chosen.id
            text: qsTr("Nothing from this channel in the feed yet. Enter opens its page.")
        }
        EmptyState {
            anchors.fill: pane
            visible: backend.subs.length === 0
            text: qsTr("No channels yet.")
        }
    }
}
