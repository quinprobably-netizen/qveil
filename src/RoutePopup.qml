import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// How veil reaches YouTube, and what it keeps.
Popup {
    id: popup

    required property var prefs
    readonly property var config: backend.config
    property string route: config.route || "direct"

    width: 440
    padding: 22
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    onAboutToShow: {
        backend.loadConfig()
        route = config.route || "direct"
        proxyField.text = config.route === "proxy" ? (config.proxy || "") : ""
    }

    background: Rectangle {
        color: "#16161b"
        radius: theme.radius
        border.color: "#2a2a32"
    }

    ColumnLayout {
        width: parent.width
        spacing: 6

        Label {
            text: qsTr("Connection")
            font.pixelSize: 17
            font.weight: Font.DemiBold
        }

        ButtonGroup { id: routes }

        component RouteOption: ColumnLayout {
            id: option
            property string value
            property string label
            property string detail
            Layout.fillWidth: true
            spacing: 0
            RadioButton {
                ButtonGroup.group: routes
                text: option.label
                checked: popup.route === option.value
                onClicked: popup.route = option.value
                Layout.leftMargin: -8
            }
            Label {
                Layout.fillWidth: true
                Layout.leftMargin: 32
                Layout.topMargin: -10
                text: option.detail
                wrapMode: Text.Wrap
                font.pixelSize: 12
                color: "#8d8d98"
            }
        }

        RouteOption {
            value: "direct"
            label: qsTr("Direct")
            detail: qsTr("YouTube sees your IP address.")
        }
        RouteOption {
            value: "tor"
            label: qsTr("Tor")
            detail: qsTr("Every request, thumbnail and video takes its own circuit, so YouTube can't link them by IP.")
        }
        Label {
            Layout.fillWidth: true
            Layout.leftMargin: 32
            visible: popup.route === "tor"
            text: popup.config.torRunning ? qsTr("Tor is running on 127.0.0.1:9050.")
                : qsTr("Tor isn't running. Install and start it: omarchy-pkg-add tor, then sudo systemctl enable --now tor")
            wrapMode: Text.Wrap
            font.pixelSize: 12
            color: popup.config.torRunning ? theme.accent : "#ff8a80"
        }
        RouteOption {
            value: "proxy"
            label: qsTr("Proxy")
            detail: qsTr("An HTTP or SOCKS proxy you trust, such as a VPN's.")
        }
        TextField {
            id: proxyField
            Layout.fillWidth: true
            Layout.leftMargin: 32
            visible: popup.route === "proxy"
            placeholderText: qsTr("socks5h://host:port or http://host:port")
        }

        Button {
            Layout.alignment: Qt.AlignRight
            text: qsTr("Use this route")
            highlighted: true
            enabled: popup.route !== popup.config.route
                     || (popup.route === "proxy" && proxyField.text !== (popup.config.proxy || ""))
            onClicked: backend.setConfig({ route: popup.route, proxy: proxyField.text })
        }

        MenuSeparator { Layout.fillWidth: true }

        Switch {
            Layout.leftMargin: -8
            text: qsTr("Veil thumbnails until hovered")
            checked: popup.prefs.veilThumbnails
            onToggled: popup.prefs.veilThumbnails = checked
        }
        Switch {
            Layout.leftMargin: -8
            text: qsTr("Load thumbnails")
            checked: popup.config.thumbnails !== false
            onToggled: backend.setConfig({ thumbnails: checked })
        }
        Switch {
            Layout.leftMargin: -8
            text: qsTr("Keep a watch history on this computer")
            checked: !!popup.config.history
            onToggled: backend.setConfig({ history: checked })
        }

        RowLayout {
            Layout.fillWidth: true
            Label {
                Layout.fillWidth: true
                text: qsTr("Highest quality")
            }
            ComboBox {
                id: quality
                readonly property var heights: [480, 720, 1080, 1440, 2160]
                model: heights.map(h => h + "p")
                currentIndex: Math.max(0, heights.indexOf(popup.config.maxHeight || 1080))
                onActivated: index => backend.setConfig({ maxHeight: heights[index] })
            }
        }

        Label {
            Layout.fillWidth: true
            Layout.topMargin: 8
            text: qsTr("Never sent: cookies, a Google account, tracking parameters, or your list of subscriptions. Subscriptions and history stay on this computer.")
            wrapMode: Text.Wrap
            font.pixelSize: 12
            lineHeight: 1.25
            color: "#8d8d98"
        }
    }
}
