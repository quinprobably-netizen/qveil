import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Effects
import "Format.js" as Format

// One video in a grid. Thumbnails stay veiled (blurred and muted) until the
// card is hovered or selected, unless veiling is switched off.
Item {
    id: card

    required property var modelData
    required property int index

    readonly property GridView view: GridView.view
    readonly property bool selected: GridView.isCurrentItem && view.activeFocus && view.navigating
    readonly property bool lit: hover.hovered || selected
    readonly property bool veiled: view.veil && !lit
    readonly property string thumb: backend.thumbBase ? backend.thumbBase + modelData.id : ""

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
            card.view.activated(card.modelData)
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
                text: (card.modelData.channel || card.modelData.title || "?").charAt(0).toUpperCase()
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

            MultiEffect {
                anchors.fill: parent
                source: img
                visible: img.status === Image.Ready
                // Keep the blur inside the frame instead of bleeding past its edges.
                autoPaddingEnabled: false
                blurEnabled: true
                blurMax: 48
                blur: card.veiled ? 0.9 : 0
                saturation: card.veiled ? -0.55 : 0
                brightness: card.veiled ? -0.18 : 0
                maskEnabled: theme.radius > 0
                maskSource: mask
                Behavior on blur { NumberAnimation { duration: 260; easing.type: Easing.OutCubic } }
                Behavior on saturation { NumberAnimation { duration: 260 } }
                Behavior on brightness { NumberAnimation { duration: 260 } }
            }

            Rectangle {
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.margins: 6
                visible: badge.text !== ""
                width: badge.implicitWidth + 10
                height: badge.implicitHeight + 4
                radius: Math.min(4, theme.radius)
                color: card.modelData.short ? theme.accent : "#d9000000"

                Label {
                    id: badge
                    anchors.centerIn: parent
                    text: card.modelData.short ? qsTr("Short")
                        : card.modelData.duration ? Format.clock(card.modelData.duration) : ""
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                    color: card.modelData.short ? theme.accentForeground : "white"
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
            text: card.modelData.title
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
            text: card.modelData.channel || ""
            elide: Text.ElideRight
            font.pixelSize: 12
            color: channelHover.hovered ? theme.accent : "#a2a2ad"

            HoverHandler {
                id: channelHover
                enabled: !!card.modelData.channel_id
                cursorShape: Qt.PointingHandCursor
            }
            MouseArea {
                anchors.fill: parent
                enabled: !!card.modelData.channel_id
                onClicked: card.view.channelRequested(card.modelData.channel_id)
            }
        }

        Label {
            anchors.top: channel.bottom
            anchors.topMargin: 2
            width: Math.min(implicitWidth, parent.width)
            text: Format.meta(card.modelData, card.modelData.date || card.modelData.published,
                              card.modelData.estimated)
            elide: Text.ElideRight
            font.pixelSize: 12
            color: "#6f6f7b"

            HoverHandler { id: dateHover }
            ToolTip.visible: dateHover.hovered && !!card.modelData.estimated && card.view.estimateNote !== ""
            ToolTip.delay: 500
            ToolTip.text: card.view.estimateNote
        }
    }
}
