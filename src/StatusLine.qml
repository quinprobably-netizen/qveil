import QtQuick
import Qveil

// The line along the foot of the window, as an editor's has one: on the left
// what the window does with what you watch (the route, whether a history is
// kept, how long a place is kept, how many channels), and on the right the
// keys that work where you are.
Rectangle {
    id: line
    objectName: "status-line"

    // Where the window is: a page's key (home, lenses, channels, channel,
    // history, search, settings), or "watch" while a video has the window.
    property string context: "home"
    // The route's word was pressed.
    signal routeClicked()
    // What the route's popup hangs from.
    readonly property alias routeAnchor: routeWord

    readonly property string route: backend.config.route || "direct"
    // The keys, by where they work, as [key, what it does]. The last is
    // always the help's.
    readonly property var hints: ({
        home: [["h j k l", qsTr("move")], ["/", qsTr("search")], ["?", qsTr("keys")]],
        lenses: [["h j k l", qsTr("move")], ["r", qsTr("refresh")], ["?", qsTr("keys")]],
        channels: [["j k", qsTr("move")], ["u", qsTr("unsubscribe")], ["?", qsTr("keys")]],
        channel: [["h j k l", qsTr("move")], ["s", qsTr("sort")], ["?", qsTr("keys")]],
        history: [["j k", qsTr("move")], ["Enter", qsTr("play")], ["?", qsTr("keys")]],
        search: [["h j k l", qsTr("move")], ["s", qsTr("sort")], ["?", qsTr("keys")]],
        settings: [["j k", qsTr("move")], ["Enter", qsTr("open")], ["?", qsTr("keys")]],
        watch: [["space", qsTr("pause")], ["n", qsTr("next")], ["c", qsTr("comments")], ["f", qsTr("fullscreen")], ["?", qsTr("keys")]]
    })
    // Fewer keys where the window is narrow: the first two, and the help.
    readonly property var shownHints: {
        const all = hints[context] || hints.home
        return width < 1000 && all.length > 3 ? all.slice(0, 2).concat([all[all.length - 1]]) : all
    }
    // Whether the keys have room beside the facts.
    readonly property bool roomy: left.width + right.width + Style.gutter * 3 <= width
    readonly property var facts: {
        const config = backend.config
        const days = config.resumeDays || 0
        const out = [config.history ? qsTr("history on") : qsTr("history off"),
                     days > 0 ? qsTr("resume %1d").arg(days) : qsTr("resume off")]
        // (Until the channels are listed, the feed knows how many there are.)
        if (backend.subs.length > 0 || backend.feedInfo.channels === 0)
            out.push(qsTr("%n channels", "", backend.subs.length))
        return out
    }

    implicitHeight: 22
    color: Style.bg
    // A raised strip, with a hairline over it.
    Rectangle {
        anchors.fill: parent
        color: Style.rest
    }
    Rule {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
    }

    Row {
        id: left
        anchors.left: parent.left
        anchors.leftMargin: Style.gutter
        anchors.verticalCenter: parent.verticalCenter
        anchors.verticalCenterOffset: 1
        spacing: Style.u * 2

        // The route: a plain word while it is direct, in its own colour once
        // it hides you. It opens the route's choices.
        Item {
            id: routeWord
            objectName: "status-route"
            readonly property string label: line.route === "tor" ? qsTr("Tor") : line.route === "proxy" ? qsTr("Proxy") : qsTr("Direct")
            readonly property color tone: line.route === "direct" ? Style.fg : Style.safe
            readonly property string tip: backend.privacy.summary || label
            anchors.verticalCenter: parent.verticalCenter
            width: word.implicitWidth
            height: 20

            Rectangle {
                anchors.fill: parent
                anchors.leftMargin: -Style.u * 2
                anchors.rightMargin: -Style.u * 2
                radius: Style.radius
                color: routeArea.containsMouse ? Style.hover : "transparent"
                Behavior on color { ColorAnimation { duration: Style.quick } }
            }
            Text {
                id: word
                anchors.centerIn: parent
                text: routeWord.label
                textFormat: Text.PlainText
                color: routeWord.tone
                font.family: Style.font
                font.pixelSize: Style.caption
                font.weight: Style.strong
            }
            MouseArea {
                id: routeArea
                anchors.fill: parent
                anchors.leftMargin: -Style.u * 2
                anchors.rightMargin: -Style.u * 2
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: line.routeClicked()
            }
            Tip {
                // (Above the word: there is no room under it.)
                y: -height - Style.u
                text: routeWord.tip
                shown: routeArea.containsMouse
            }
        }
        Repeater {
            model: line.facts
            delegate: Row {
                required property string modelData
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.u * 2
                Text {
                    text: "·"
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
                Text {
                    objectName: "status-fact"
                    text: modelData
                    textFormat: Text.PlainText
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
            }
        }
    }

    Row {
        id: right
        anchors.right: parent.right
        anchors.rightMargin: Style.gutter
        anchors.verticalCenter: parent.verticalCenter
        anchors.verticalCenterOffset: 1
        spacing: Style.u * 4
        // Where there is no room beside what is on the left, the keys go. (They
        // fade, not hide: a hidden row has no width to be measured by.)
        opacity: line.roomy ? 1 : 0

        Repeater {
            model: line.shownHints
            delegate: Row {
                required property var modelData
                objectName: "status-hint"
                readonly property string key: modelData[0]
                readonly property string what: modelData[1]
                spacing: Style.u + 2
                Text {
                    text: key
                    textFormat: Text.PlainText
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.caption
                    font.weight: Style.strong
                }
                Text {
                    text: what
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.caption
                }
            }
        }
    }
}
