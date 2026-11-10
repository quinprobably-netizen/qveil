import QtQuick
import Qveil

// How qveil reaches YouTube: Direct, Tor, or a proxy of your own. A route
// applies as soon as it is chosen; only a proxy waits, for its address.
// Every word about what a route does is the engine's (backend.privacy).
Column {
    id: chooser

    readonly property var config: backend.config
    // Whether Proxy was chosen but its address hasn't been sent yet. The
    // saved route stays what the rail says until it is.
    property bool proxyPending: false
    readonly property string route: proxyPending ? "proxy" : (config.route || "direct")
    // Whether the address in the field has been typed since it was last
    // filled from the config, and so is the user's and must not be overwritten.
    property bool editing: false

    // The saved proxy as the engine reports it: redacted when it has
    // credentials. Sent back unchanged, it means "keep the saved proxy".
    function savedProxy() { return config.route === "proxy" ? (config.proxy || "") : "" }

    function choose(value) {
        if (value === "proxy") {
            // The address comes first: a proxy route without one can't work.
            if (config.route !== "proxy") {
                proxyPending = true
                proxy.edit()
            }
            return
        }
        proxyPending = false
        if (value !== (config.route || "direct"))
            backend.setConfig({ route: value })
    }
    function applyProxy(address) {
        // The field goes as it is: an untouched redacted value keeps the saved proxy.
        backend.setConfig({ route: "proxy", proxy: address })
        editing = false
    }
    function fill() {
        editing = false
        proxy.text = savedProxy()
    }
    // What is shown when the chooser comes into view.
    function refresh() {
        proxyPending = false
        backend.loadConfig()
        fill()
    }
    // The config can arrive late, and after saving it comes back with the
    // proxy redacted, so the field follows it until an address is being typed.
    onConfigChanged: {
        if (config.route === "proxy")
            proxyPending = false
        if (!editing)
            fill()
    }

    spacing: Style.u * 2

    Row {
        spacing: 2
        TabWord {
            objectName: "route-direct"
            text: qsTr("Direct")
            current: chooser.route === "direct"
            onClicked: chooser.choose("direct")
        }
        TabWord {
            objectName: "route-tor"
            text: qsTr("Tor")
            current: chooser.route === "tor"
            onClicked: chooser.choose("tor")
        }
        TabWord {
            objectName: "route-proxy"
            text: qsTr("Proxy")
            current: chooser.route === "proxy"
            onClicked: chooser.choose("proxy")
        }
    }
    Notice {
        objectName: "route-note"
        width: parent.width
        text: backend.privacy.routes[chooser.route] || ""
    }
    Text {
        width: parent.width
        visible: chooser.route === "tor"
        text: chooser.config.torRunning ? qsTr("Tor is running on 127.0.0.1:9050.")
            : qsTr("Tor isn't running. Install and start it: sudo pacman -S tor, then sudo systemctl enable --now tor")
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: chooser.config.torRunning ? Style.safe : Style.warn
        font.family: Style.font
        font.pixelSize: Style.small
    }
    Field {
        id: proxy
        objectName: "route-proxy-address"
        width: Math.min(parent.width, 420)
        visible: chooser.route === "proxy"
        placeholder: qsTr("socks5h://host:port, then Enter")
        onEdited: chooser.editing = true
        onSubmitted: address => chooser.applyProxy(address)
    }
}
