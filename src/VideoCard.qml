import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Effects
import "Format.js" as Format

// One video in a grid. Thumbnails stay veiled (blurred and steeped in the
// theme's dyes) until the card is hovered or selected, unless veiling is off.
Item {
    id: card

    // The row's roles, named like the engine's JSON keys (a role called `id`
    // can only be reached through `model`).
    required property var model
    required property int index

    readonly property GridView view: GridView.view
    readonly property bool selected: GridView.isCurrentItem && view.activeFocus && view.navigating
    readonly property bool lit: hover.hovered || selected
    readonly property bool veiled: view.veil && !lit
    readonly property string thumb: backend.thumbBase ? backend.thumbBase + model.id : ""
    // How steeped in dye the thumbnail is: 1 veiled, 0 clear.
    property real steep: veiled ? 1 : 0
    Behavior on steep { NumberAnimation { duration: 420; easing.type: Easing.InOutCubic } }
    // A stable, different swirl for every video.
    readonly property real seed: {
        let h = 0
        for (let i = 0; i < model.id.length; i++)
            h = (h * 31 + model.id.charCodeAt(i)) % 9973
        return h / 97
    }
    // Feed and channel entries say whether their date is approximate. A search
    // result has only YouTube's "3 days ago", so there it always is.
    readonly property bool approximate: model.estimated !== false
    // Unix seconds, or 0 when the video has no date at all.
    readonly property real stamp: model.date || model.published || 0

    width: view.cellWidth
    height: view.cellHeight

    HoverHandler {
        id: hover
        cursorShape: Qt.PointingHandCursor
        onHoveredChanged: if (hovered) card.view.navigating = false
    }

    MouseArea {
        anchors.fill: parent
        onClicked: {
            card.view.currentIndex = card.index
            card.view.activated(card.view.model.get(card.index))
        }
    }

    Item {
        anchors.fill: parent
        anchors.margins: 8

        Rectangle {
            id: frame
            width: parent.width
            height: Math.round(width * 9 / 16)
            radius: theme.radius
            color: "#16161b"

            // Without a thumbnail, the channel's initial stands in.
            Label {
                anchors.centerIn: parent
                visible: img.status !== Image.Ready
                text: (card.model.channel || card.model.title || "?").charAt(0).toUpperCase()
                font.pixelSize: frame.height * 0.36
                font.weight: Font.Light
                color: "#2c2c34"
            }

            Image {
                id: img
                anchors.fill: parent
                source: card.thumb
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                visible: false
            }

            Rectangle {
                id: mask
                anchors.fill: parent
                radius: theme.radius
                visible: false
                layer.enabled: true
            }

            // First pass: blur (and round the corners). The dye goes on top.
            MultiEffect {
                id: blurred
                anchors.fill: parent
                source: img
                visible: false
                layer.enabled: true
                // Keep the blur inside the frame instead of bleeding past its edges.
                autoPaddingEnabled: false
                blurEnabled: true
                blurMax: 80
                blur: card.steep
                maskEnabled: theme.radius > 0
                maskSource: mask
            }

            // The veil: the thumbnail steeps in the theme's dyes, and hovering or
            // selecting the card drains the dye away (the swirl only moves then).
            DyeWash {
                anchors.fill: parent
                source: blurred
                visible: img.status === Image.Ready
                // Not quite all the way: a little of the thumbnail's own colour
                // shows through, so cards keep different silhouettes instead of
                // making a wall of one swirl.
                strength: 0.85 * card.steep
                seed: card.seed
                drift: (1 - card.steep) * 1.4
            }

            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.margins: 6
                visible: badge.text !== ""
                width: badge.implicitWidth + 10
                height: badge.implicitHeight + 4
                radius: Math.min(4, theme.radius)
                color: card.model.short ? theme.accent : "#d9000000"

                Label {
                    id: badge
                    anchors.centerIn: parent
                    text: card.model.short ? qsTr("Short")
                        : card.model.duration ? Format.clock(card.model.duration) : ""
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                    color: card.model.short ? theme.accentForeground : "white"
                }
            }

            Rectangle {
                anchors.fill: parent
                radius: theme.radius
                color: "transparent"
                border.width: card.selected ? 2 : 0
                border.color: theme.accent
            }
        }

        Label {
            id: title
            anchors.top: frame.bottom
            anchors.topMargin: 10
            width: parent.width
            text: card.model.title
            wrapMode: Text.Wrap
            maximumLineCount: 2
            elide: Text.ElideRight
            font.pixelSize: 14
            font.weight: Font.Medium
            lineHeight: 1.1
            color: card.lit ? "#ffffff" : "#e2e2e8"
        }

        Label {
            id: channel
            anchors.top: title.bottom
            anchors.topMargin: 5
            width: Math.min(implicitWidth, parent.width)
            text: card.model.channel || ""
            elide: Text.ElideRight
            font.pixelSize: 12
            color: channelHover.hovered ? theme.accent : "#a2a2ad"

            HoverHandler {
                id: channelHover
                enabled: !!card.model.channel_id
                cursorShape: Qt.PointingHandCursor
            }
            MouseArea {
                anchors.fill: parent
                enabled: !!card.model.channel_id
                onClicked: card.view.channelRequested(card.model.channel_id)
            }
        }

        Label {
            anchors.top: channel.bottom
            anchors.topMargin: 2
            width: Math.min(implicitWidth, parent.width)
            text: Format.meta(card.model, card.stamp)
            elide: Text.ElideRight
            font.pixelSize: 12
            color: "#6f6f7b"

            HoverHandler { id: dateHover }
            ToolTip.visible: dateHover.hovered && card.stamp > 0 && card.approximate && card.view.estimateNote !== ""
            ToolTip.delay: 500
            ToolTip.text: card.view.estimateNote
        }
    }
}
