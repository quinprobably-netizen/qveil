import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// How qveil reaches YouTube, and what it keeps.
Popup {
    id: popup

    required property var prefs
    readonly property var config: backend.config
    property string route: config.route || "direct"
    // The saved proxy as the engine reports it: redacted when it has
    // credentials. Sent back unchanged, it means "keep the saved proxy". (A
    // function, not a property: a change handler would otherwise read it
    // before its binding had caught up with the new config.)
    function savedProxy() { return config.route === "proxy" ? (config.proxy || "") : "" }
    // Whether the user has chosen a route or typed an address since the form
    // was last filled from the config.
    property bool touched: false
    readonly property bool changed: route !== (config.route || "direct")
                                    || (route === "proxy" && proxyField.text !== savedProxy())

    function fillFromConfig() {
        route = config.route || "direct"
        proxyField.text = savedProxy()
    }

    width: 440
    padding: 22
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    // The config can arrive after the popup opens, and after saving it comes
    // back with the proxy redacted, so the form follows it until the user
    // touches it.
    onConfigChanged: if (!touched) fillFromConfig()
    onAboutToShow: {
        touched = false
        backend.loadConfig()
        fillFromConfig()
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
                onClicked: {
                    popup.route = option.value
                    popup.touched = true
                }
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
            color: popup.config.torRunning ? theme.protectedColor : "#ff8a80"
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
            onTextEdited: popup.touched = true
        }

        Button {
            Layout.alignment: Qt.AlignRight
            text: qsTr("Use this route")
            highlighted: true
            // Only when something changed, and a proxy route needs an address.
            enabled: popup.changed && (popup.route !== "proxy" || proxyField.text.trim() !== "")
            onClicked: {
                // The field goes as it is: an untouched redacted value keeps
                // the saved proxy.
                backend.setConfig({ route: popup.route, proxy: proxyField.text })
                popup.touched = false
            }
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
