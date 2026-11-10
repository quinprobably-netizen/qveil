import QtQuick
import Qveil
import "Format.js" as Format

// One video in a grid: its thumbnail under the veil until the card is
// pointed at or selected, its title, channel and date, and, where a lens or
// a search put it there, why.
Item {
    id: card
    objectName: "card"

    // The row's roles, named like the engine's JSON keys (a role called `id`
    // can only be reached through `model`).
    required property var model
    required property int index

    readonly property GridView view: GridView.view
    readonly property bool selected: GridView.isCurrentItem && view.activeFocus && view.navigating
    readonly property bool lit: hover.hovered || selected
    // The batch is the page the card came with: its thumbnails share one circuit on Tor.
    readonly property string thumb: Format.thumb(backend.thumbBase, model.id, model.batch)
    // Feed and channel entries say whether their date is approximate. A search
    // result has only YouTube's "3 days ago", so there it always is.
    readonly property bool approximate: model.estimated !== false
    // Unix seconds, or 0 when the video has no date at all.
    readonly property real stamp: model.date || model.published || 0
    // How far it was watched, 0 to 1, when that is remembered.
    readonly property real progress: (view.progress && view.progress[model.id]) || 0
    readonly property var why: model.why || []

    width: view.cellWidth
    height: view.cellHeight

    HoverHandler {
        id: hover
        cursorShape: Qt.PointingHandCursor
        onHoveredChanged: if (hovered) card.view.navigating = false
    }
    TapHandler {
        onTapped: {
            card.view.currentIndex = card.index
            card.view.activated(card.view.model.get(card.index), card.index)
        }
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
                text: (card.model.channel || card.model.title || "?").charAt(0).toUpperCase()
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
            }
            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                visible: tag.text !== ""
                width: tag.implicitWidth + Style.u * 3
                height: tag.implicitHeight + Style.u
                color: card.model.short ? Style.accent : Style.scrim
                Text {
                    id: tag
                    anchors.centerIn: parent
                    text: card.model.short ? qsTr("Short")
                        : card.model.duration ? Format.clock(card.model.duration) : ""
                    textFormat: Text.PlainText
                    color: card.model.short ? Style.accentText : Style.fg
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
            visible: card.lit
            color: "transparent"
            border.width: 1
            border.color: Style.fg
        }

        Text {
            id: title
            anchors.top: frame.bottom
            anchors.topMargin: Style.u * 2 + 1
            width: parent.width
            text: card.model.title
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            maximumLineCount: 2
            elide: Text.ElideRight
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.body
            font.bold: true
        }
        Text {
            id: channel
            anchors.top: title.bottom
            anchors.topMargin: 3
            width: Math.min(implicitWidth, parent.width)
            text: card.model.channel || ""
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: channelHover.hovered ? Style.accent : Style.dim
            font.family: Style.font
            font.pixelSize: Style.small

            HoverHandler {
                id: channelHover
                enabled: !!card.model.channel_id
                cursorShape: Qt.PointingHandCursor
            }
            TapHandler {
                enabled: !!card.model.channel_id
                onTapped: card.view.channelRequested(card.model.channel_id)
            }
        }
        Text {
            id: meta
            anchors.top: channel.bottom
            anchors.topMargin: 2
            width: Math.min(implicitWidth, parent.width)
            text: Format.meta(card.model, card.stamp)
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
