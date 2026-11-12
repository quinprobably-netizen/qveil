import QtQuick
import Qveil
import "Format.js" as Format

// An announced stream that hasn't started (R1): its thumbnail softened, and
// on it, large, how long until it starts. Nothing is loaded into the player
// meanwhile. The engine asks whether it has started; when it has, the
// player plays it once, from its start (see PlayerSurface.begin).
Item {
    id: room
    objectName: "waiting-room"

    // The play answer (id, title, batch for the thumbnail, …).
    property var info: ({})
    // When it is announced for, in Unix seconds; 0 when that isn't known.
    property real starts: 0
    // "upcoming" while it is waited for; "gave_up" once the engine has
    // stopped waiting, and "failed" when it can't be played at all, with the
    // engine's words for it in `message`. (Not `state`: an Item has one.)
    property string phase: "upcoming"
    property string message: ""
    // While it is waited for: the engine's asks meet a bot check (it goes on
    // asking, on a fresh connection each time).
    property bool botCheck: false
    // Whether another route may get past what the room says: offered here,
    // never taken by itself.
    property bool offersRoute: false
    signal routeRequested()

    // The clock the countdown is told by, a second at a time while it shows.
    property real now: Date.now() / 1000
    Timer {
        interval: 1000
        repeat: true
        running: room.visible && room.phase === "upcoming"
        triggeredOnStart: true
        onTriggered: room.now = Date.now() / 1000
    }
    readonly property bool over: phase !== "upcoming"
    readonly property bool known: starts > 0
    readonly property bool ahead: known && now < starts

    Rectangle {
        anchors.fill: parent
        color: "black"
    }
    // Always softened, whatever the thumbnails' setting: it is a backdrop
    // for the countdown, not a picture to look at. (The same picture as the
    // card's: no new request.)
    Frost {
        anchors.fill: parent
        source: Format.thumb(backend.thumbBase, room.info.id, room.info.batch)
    }
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(Style.bg.r, Style.bg.g, Style.bg.b, 0.6)
    }

    Column {
        anchors.centerIn: parent
        width: Math.min(parent.width - 2 * Style.gutter, 560)
        spacing: Style.u
        Text {
            objectName: "waiting-eyebrow"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            text: room.over ? qsTr("STOPPED WAITING") : room.ahead ? qsTr("STARTS IN")
                : room.known ? qsTr("SCHEDULED FOR") : qsTr("NOT STARTED YET")
            textFormat: Text.PlainText
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.caption
            font.weight: Style.strong
            font.letterSpacing: 1.4
        }
        Text {
            objectName: "waiting-countdown"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            visible: !room.over && room.known
            // Until its time, how long until then; past it, when that was.
            text: room.ahead ? Format.clock(room.starts - room.now) : Format.dayTime(room.starts)
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.lead
            font.weight: Style.strong
            font.letterSpacing: 1
        }
        Text {
            objectName: "waiting-line"
            width: parent.width
            topPadding: room.over ? Style.u : 0
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: room.over ? room.message : Format.waitLine(room.starts, room.now)
            textFormat: Text.PlainText
            color: room.over ? Style.fg : Style.dim
            font.family: Style.font
            font.pixelSize: room.over ? Style.body : Style.small
        }
        // A quiet line under it, while YouTube stands in the way.
        Text {
            objectName: "waiting-note"
            width: parent.width
            topPadding: Style.u
            visible: !room.over && room.botCheck
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: qsTr("YouTube asked to confirm this isn't a bot \u00b7 still trying on a fresh connection")
            textFormat: Text.PlainText
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.small
        }
        // The same offer as the window's error makes, for the same reasons.
        Act {
            objectName: "waiting-route"
            anchors.horizontalCenter: parent.horizontalCenter
            visible: room.offersRoute
            text: qsTr("Change route")
            filled: true
            onClicked: room.routeRequested()
        }
    }
}
