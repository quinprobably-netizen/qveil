import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts

// How qveil reaches YouTube, and what it keeps. A route applies as soon as it
// is chosen; only a proxy waits, for its address. Every word about privacy
// here is the engine's (backend.privacy), shown as it comes: the window has
// no copy of its own to fall out of step with what the engine does.
Popup {
    id: popup

    required property var prefs
    readonly property var config: backend.config
    // Whether Proxy was chosen but its address hasn't been sent yet. The saved
    // route stays what the chip says until it is.
    property bool proxyPending: false
    readonly property string route: proxyPending ? "proxy" : (config.route || "direct")
    // The saved proxy as the engine reports it: redacted when it has
    // credentials. Sent back unchanged, it means "keep the saved proxy". (A
    // function, not a property: a change handler would otherwise read it
    // before its binding had caught up with the new config.)
    function savedProxy() { return config.route === "proxy" ? (config.proxy || "") : "" }

    // Whether the address in the field has been typed since it was last filled
    // from the config, and so is the user's and must not be overwritten.
    property bool editing: false

    function choose(value) {
        if (value === "proxy") {
            // The address comes first: a proxy route without one can't work.
            if (config.route !== "proxy") {
                proxyPending = true
                proxyField.forceActiveFocus()
            }
            return
        }
        proxyPending = false
        if (value !== (config.route || "direct"))
            backend.setConfig({ route: value })
    }

    function applyProxy() {
        const address = proxyField.text.trim()
        if (address === "")
            return
        // The field goes as it is: an untouched redacted value keeps the saved proxy.
        backend.setConfig({ route: "proxy", proxy: address })
        editing = false
    }

    function fillProxy() {
        editing = false
        proxyField.text = savedProxy()
    }

    width: 440
    padding: 22
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    // The config can arrive after the popup opens, and after saving it comes
    // back with the proxy redacted, so the field follows it until the user
    // has begun to type an address.
    onConfigChanged: {
        if (config.route === "proxy")
            proxyPending = false
        if (!editing)
            fillProxy()
    }
    onAboutToShow: {
        proxyPending = false
        backend.loadConfig()
        fillProxy()
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
            text: qsTr("Route & privacy")
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
                onClicked: popup.choose(option.value)
                Layout.leftMargin: -8
            }
            Label {
                Layout.fillWidth: true
                Layout.leftMargin: 32
                Layout.topMargin: -10
                visible: text !== ""
                text: option.detail
                wrapMode: Text.Wrap
                font.pixelSize: 12
                color: "#8d8d98"
            }
        }

        RouteOption {
            value: "direct"
            label: qsTr("Direct")
            detail: backend.privacy.routes.direct
        }
        RouteOption {
            value: "tor"
            label: qsTr("Tor")
            detail: backend.privacy.routes.tor
        }
        Label {
            Layout.fillWidth: true
            Layout.leftMargin: 32
            visible: popup.route === "tor"
            text: popup.config.torRunning ? qsTr("Tor is running on 127.0.0.1:9050.")
                : qsTr("Tor isn't running. Install and start it: sudo pacman -S tor, then sudo systemctl enable --now tor")
            wrapMode: Text.Wrap
            font.pixelSize: 12
            color: popup.config.torRunning ? theme.protectedColor : "#ff8a80"
        }
        RouteOption {
            value: "proxy"
            label: qsTr("Proxy")
            detail: backend.privacy.routes.proxy
        }
        TextField {
            id: proxyField
            Layout.fillWidth: true
            Layout.leftMargin: 32
            visible: popup.route === "proxy"
            placeholderText: qsTr("socks5h://host:port, then Enter")
            onTextEdited: popup.editing = true
            onAccepted: popup.applyProxy()
        }

        MenuSeparator { Layout.fillWidth: true }

        Switch {
            Layout.leftMargin: -8
            text: qsTr("Veil thumbnails until hovered")
            checked: popup.prefs.thumbnails === "frosted"
            onToggled: popup.prefs.thumbnails = checked ? "frosted" : "clear"
        }
        Switch {
            Layout.leftMargin: -8
            text: qsTr("Load thumbnails")
            checked: popup.config.thumbnails !== false
            onToggled: backend.setConfig({ thumbnails: checked })
        }
        Switch {
            Layout.leftMargin: -8
            text: qsTr("Keep a watch history")
            checked: !!popup.config.history
            onToggled: backend.setConfig({ history: checked })
        }

        // What the engine says about what it does and keeps, sentence by sentence.
        Repeater {
            model: backend.privacy.points
            delegate: Label {
                required property string modelData
                required property int index
                Layout.fillWidth: true
                Layout.topMargin: index === 0 ? 8 : 0
                text: modelData
                wrapMode: Text.Wrap
                font.pixelSize: 12
                lineHeight: 1.25
                color: "#8d8d98"
            }
        }
    }
}
