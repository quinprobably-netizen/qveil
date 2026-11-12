import QtQuick
import Qveil
import "Format.js" as Format
import "Linkify.js" as Linkify

// What comes after this video in the list it was opened from (the feed, a
// lens, a search). It is already here, so showing it asks for nothing.
// In an announced stream's waiting room it is the channel's lineup instead:
// what else of it runs or is announced (see Backend.lineup).
ListView {
    id: tab

    // A VideoModel, or null when the video wasn't opened from a list.
    property var list: null
    // Where in the list the video that is playing is.
    property int index: -1
    // Where the list came from, in words ("Unfinished", "lens: slow woodworking"), or "".
    property string from: ""
    // How many follow the video that is playing: all of them, though it lists the next forty.
    readonly property int remaining: list && index >= 0 ? Math.max(0, list.count - index - 1) : 0
    // "frosted" or "clear": how thumbnails are shown (see Frost).
    property string thumbs: "clear"
    // In a waiting room, the channel's lineup (Backend.lineup) and the
    // channel's name: shown in place of the list. Null otherwise.
    property var lineup: null
    property string channel: ""
    readonly property bool room: lineup !== null
    readonly property var entries: {
        if (room)
            return (lineup.items || []).map((video, i) => ({ row: i, video: video }))
        const out = []
        if (list && index >= 0)
            for (let i = index + 1; i < list.count && out.length < 40; i++)
                out.push({ row: i, video: list.get(i) })
        return out
    }
    signal picked(var video, int row)
    // Whether the keyboard is in the sheet: the row it is on is marked.
    property bool keyed: false
    function step(by) {
        currentIndex = Math.max(0, Math.min(count - 1, currentIndex + by))
        // (The first has the line over it in view too.)
        if (currentIndex === 0)
            positionViewAtBeginning()
        else
            positionViewAtIndex(currentIndex, ListView.Contain)
    }
    function activate() {
        if (currentIndex >= 0 && currentIndex < entries.length)
            picked(entries[currentIndex].video, entries[currentIndex].row)
    }
    // A list begins at its start, with the line over it; the line may have
    // come after the list, which then stood where it was.
    function toStart() {
        currentIndex = 0
        positionViewAtBeginning()
    }
    onEntriesChanged: toStart()
    onFromChanged: Qt.callLater(positionViewAtBeginning)
    keyNavigationEnabled: false

    model: entries
    clip: true
    boundsBehavior: Flickable.StopAtBounds

    // Where the list came from, once, over all of it.
    header: (tab.room ? tab.channel !== "" : tab.from !== "") && tab.count > 0 ? provenance : null
    Component {
        id: provenance
        Item {
            objectName: "next-from"
            width: tab.width
            height: 34
            Row {
                anchors.left: parent.left
                anchors.leftMargin: Style.u * 2
                anchors.right: parent.right
                anchors.rightMargin: Style.u * 3
                anchors.bottom: parent.bottom
                anchors.bottomMargin: Style.u
                Text {
                    objectName: "next-from-text"
                    // The count keeps its place: a long name gives way, not it.
                    width: Math.min(implicitWidth, parent.width - (tab.room ? 0 : count.width))
                    text: tab.room ? qsTr("From %1 \u00b7 Streams").arg(tab.channel) : qsTr("From %1").arg(tab.from)
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
                Text {
                    id: count
                    objectName: "next-from-count"
                    visible: !tab.room
                    text: " \u00b7 " + tab.remaining
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
        }
    }

    delegate: Rectangle {
        id: entry
        required property var modelData
        readonly property string title: modelData.video.title || ""
        required property int index
        // The keyboard's cursor: a frame on the picture, as on a card.
        readonly property bool marked: tab.keyed && ListView.isCurrentItem
        objectName: "next-entry"
        width: tab.width
        height: 92
        color: area.containsMouse ? Style.hover : "transparent"

        Item {
            id: picture
            x: Style.u * 2
            anchors.verticalCenter: parent.verticalCenter
            width: 136
            height: 76
            clip: true
            Rectangle {
                anchors.fill: parent
                color: Style.rest
            }
            // The engine has these from the list the video was opened from.
            Frost {
                objectName: "frost"
                anchors.fill: parent
                source: Format.thumb(backend.thumbBase, entry.modelData.video.id, entry.modelData.video.batch || 0)
                clear: area.containsMouse
                mode: tab.thumbs
            }
            // The corner tag, as on a card (L2): a running stream's "● LIVE"
            // in the accent's fill, an announced one's day and time outlined,
            // or the length.
            Rectangle {
                objectName: "next-tag"
                readonly property bool running: entry.modelData.video.live === "now"
                readonly property bool announced: entry.modelData.video.live === "upcoming"
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                visible: clock.text !== ""
                width: clock.implicitWidth + Style.u * 2
                height: clock.implicitHeight + 2
                color: running ? Style.accent : Style.scrim
                border.width: announced ? 1 : 0
                border.color: Style.dim
                Text {
                    id: clock
                    anchors.centerIn: parent
                    text: parent.running ? qsTr("\u25cf LIVE")
                        : parent.announced ? (Format.dayTime(entry.modelData.video.starts) || qsTr("Upcoming"))
                        : entry.modelData.video.duration ? Format.clock(entry.modelData.video.duration) : ""
                    textFormat: Text.PlainText
                    color: parent.running ? Style.accentText : Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
            }
        }
        // The keyboard's cursor: a ring around the picture, as on a card.
        Rectangle {
            objectName: "cursor"
            anchors.fill: picture
            anchors.margins: -(Style.ringGap + Style.ring)
            visible: entry.marked
            color: "transparent"
            border.width: Style.ring
            border.color: Style.accent
        }
        Column {
            anchors.left: picture.right
            anchors.right: parent.right
            anchors.leftMargin: Style.u * 3
            anchors.rightMargin: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            spacing: 3
            Text {
                width: parent.width
                text: entry.title
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                color: Style.fg
                font.family: Style.font
                font.pixelSize: Style.body
                font.weight: Style.strong
            }
            Text {
                width: parent.width
                visible: text !== ""
                text: entry.modelData.video.channel || ""
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: Style.dim
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Text {
                id: metaLine
                objectName: "next-meta"
                readonly property var when: entry.modelData.video.date || entry.modelData.video.published || 0
                readonly property string whole: Format.meta(entry.modelData.video, when)
                readonly property string said: fits.width <= width ? whole : Format.meta(entry.modelData.video, when, true)
                // "live now" in the accent, as on a card.
                readonly property string word: qsTr("live now")
                readonly property bool marked: entry.modelData.video.live === "now" && said.indexOf(word) === 0
                width: parent.width
                visible: said !== ""
                // Without the word "views" where all of it won't fit.
                text: marked ? "<font color=\"" + Style.accent + "\">" + Linkify.escape(word) + "</font>" + Linkify.escape(said.slice(word.length))
                             : said
                TextMetrics {
                    id: fits
                    font.family: Style.font
                    font.pixelSize: Style.small
                    text: metaLine.whole
                }
                textFormat: marked ? Text.StyledText : Text.PlainText
                elide: Text.ElideRight
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.small
            }
        }
        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: tab.picked(entry.modelData.video, entry.modelData.row)
        }
    }

    Text {
        objectName: "next-empty"
        anchors.centerIn: parent
        width: parent.width - Style.u * 8
        // (A room's lineup says nothing until it has been read.)
        visible: tab.count === 0 && text !== ""
        text: !tab.room ? qsTr("Nothing follows: this video wasn't opened from a list, or it was the last in it.")
            : !tab.lineup.loaded ? ""
            : tab.lineup.failed ? qsTr("The channel's streams couldn't be read.")
            : qsTr("Nothing else announced")
        textFormat: Text.PlainText
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
}
