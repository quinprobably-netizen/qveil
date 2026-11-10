import QtQuick
import Qveil
import "Format.js" as Format

// One video, in a grid or on a shelf: its thumbnail (frosted until it is
// pointed at or selected, when thumbnails are set to be), its title, channel
// and date, and, where a lens or a search put it there, why.
Item {
    id: card
    objectName: "card"

    // The video, with the engine's JSON keys: a row of a list, or an object.
    property var video: ({})
    required property int index
    // What the card is in: a grid or a shelf. It gives the card its size
    // (cellWidth, cellHeight), how thumbnails are shown (thumbs), how far
    // videos were watched (progress), the note for approximate dates
    // (estimateNote), and takes pick(index), pointed() and channelRequested(id).
    property Item view
    // Whether the keyboard is on this card.
    property bool selected: false
    readonly property bool lit: hover.hovered || selected
    // The batch is the page the card came with: its thumbnails share one circuit on Tor.
    readonly property string thumb: Format.thumb(backend.thumbBase, video.id, video.batch)
    // Feed and channel entries say whether their date is approximate. A search
    // result has only YouTube's "3 days ago", so there it always is.
    readonly property bool approximate: video.estimated !== false
    // Unix seconds, or 0 when the video has no date at all.
    readonly property real stamp: video.date || video.published || 0
    // How far it was watched, 0 to 1, when that is remembered.
    readonly property real progress: (view.progress && view.progress[video.id]) || 0
    readonly property var why: video.why || []

    width: view.cellWidth
    height: view.cellHeight

    HoverHandler {
        id: hover
        cursorShape: Qt.PointingHandCursor
        onHoveredChanged: if (hovered) card.view.pointed()
    }
    TapHandler {
        onTapped: card.view.pick(card.index)
    }

    Item {
        anchors.fill: parent
        anchors.margins: Style.u * 2

        Rectangle {
            id: frame
            width: parent.width
            height: Math.round(width * 9 / 16)
            color: Style.rest
            clip: true

            // Without a thumbnail, the channel's initial stands in.
            Text {
                anchors.centerIn: parent
                visible: !frost.ready
                text: (card.video.channel || card.video.title || "?").charAt(0).toUpperCase()
                textFormat: Text.PlainText
                color: Style.line
                font.family: Style.font
                font.pixelSize: frame.height * 0.36
            }
            Frost {
                id: frost
                objectName: "frost"
                anchors.fill: parent
                source: card.thumb
                clear: card.lit
                mode: card.view.thumbs
                // A little closer when it is pointed at (the frame clips it).
                scale: hover.hovered ? 1.045 : 1
                Behavior on scale { NumberAnimation { duration: Style.move * 1.5; easing.type: Easing.OutCubic } }
            }
            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                visible: tag.text !== ""
                width: tag.implicitWidth + Style.u * 3
                height: tag.implicitHeight + Style.u
                color: card.video.short ? Style.accent : Style.scrim
                Text {
                    id: tag
                    anchors.centerIn: parent
                    text: card.video.short ? qsTr("Short")
                        : card.video.duration ? Format.clock(card.video.duration) : ""
                    textFormat: Text.PlainText
                    color: card.video.short ? Style.accentText : Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
            }
            // How far it was watched.
            Rectangle {
                anchors.left: parent.left
                anchors.bottom: parent.bottom
                visible: card.progress > 0
                width: parent.width * Math.min(1, card.progress)
                height: 2
                color: Style.accent
            }
        }
        // What is looked at is outlined, a little way out.
        Rectangle {
            anchors.fill: frame
            anchors.margins: -3
            opacity: card.lit ? 1 : 0
            visible: opacity > 0
            Behavior on opacity { NumberAnimation { duration: Style.quick } }
            color: "transparent"
            border.width: 1
            border.color: Style.fg
        }

        Text {
            id: title
            anchors.top: frame.bottom
            anchors.topMargin: Style.u * 2 + 1
            width: parent.width
            text: card.video.title
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            maximumLineCount: 2
            elide: Text.ElideRight
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.card
            font.weight: Style.strong
        }
        Text {
            id: channel
            anchors.top: title.bottom
            anchors.topMargin: 3
            width: Math.min(implicitWidth, parent.width)
            text: card.video.channel || ""
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: channelHover.hovered ? Style.accent : Style.dim
            font.family: Style.font
            font.pixelSize: Style.small

            HoverHandler {
                id: channelHover
                enabled: !!card.video.channel_id
                cursorShape: Qt.PointingHandCursor
            }
            TapHandler {
                enabled: !!card.video.channel_id
                onTapped: card.view.channelRequested(card.video.channel_id)
            }
        }
        Text {
            id: meta
            anchors.top: channel.bottom
            anchors.topMargin: 2
            width: Math.min(implicitWidth, parent.width)
            text: Format.meta(card.video, card.stamp)
            // A video with neither date nor views has no such line.
            height: text === "" ? 0 : implicitHeight
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.small

            HoverHandler { id: dateHover }
            Tip {
                text: card.view.estimateNote
                shown: dateHover.hovered && card.stamp > 0 && card.approximate
            }
        }
        Text {
            objectName: "card-why"
            anchors.top: meta.bottom
            anchors.topMargin: 2
            width: parent.width
            visible: card.why.length > 0
            text: card.why.join("  ·  ")
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Style.accent
            font.family: Style.font
            font.pixelSize: Style.small
        }
    }
}
