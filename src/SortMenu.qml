import QtQuick
import QtQuick.Controls
import Qveil
import "Arrange.js" as Arrange

// One control for how a list is sorted and filtered. It says what it is set
// to, and opens a small menu of the choices: the order, the length, and
// whether what was watched is shown.
Act {
    id: control

    // { sort, length, watched }: see Arrange.js.
    property var setting: ({})
    readonly property var now: Arrange.of(setting)
    // What the order the list came in is called: "Newest" for a feed, "Best
    // match" for what a search found.
    property string sourceLabel: qsTr("Newest")
    // Whether the list came newest first, so that "Newest" is not a choice of its own.
    readonly property bool sourceIsNewest: sourceLabel === qsTr("Newest")
    signal changed(var setting)
    readonly property bool opened: menu.opened
    function open() { menu.open() }

    function sortName(key) {
        return key === "" ? sourceLabel : key === "newest" ? qsTr("Newest") : key === "views" ? qsTr("Most viewed")
             : key === "longest" ? qsTr("Longest") : qsTr("Shortest")
    }
    function lengthName(key) {
        return key === "short" ? qsTr("under 10 min") : key === "medium" ? qsTr("10 to 30 min") : qsTr("over 30 min")
    }
    function set(part, value) {
        const next = Arrange.of(setting)
        next[part] = value
        changed(next)
    }

    text: [sortName(now.sort), now.length !== "any" ? lengthName(now.length) : "",
           now.watched === "hide" ? qsTr("unwatched") : ""].filter(s => s !== "").join(" · ")
    // A quiet word, with the mark of something that opens.
    quiet: !menu.opened
    trailing: "\u25be"
    tip: menu.opened ? "" : qsTr("Sort and filter")
    onClicked: menu.opened ? menu.close() : menu.open()

    component Head: Text {
        leftPadding: Style.u * 2
        topPadding: Style.u * 2
        bottomPadding: Style.u
        textFormat: Text.PlainText
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.caption
        font.capitalization: Font.AllUppercase
        font.letterSpacing: 1
    }
    component Option: Rectangle {
        id: option
        property string text
        property bool current: false
        // Its place in the menu, from the top, and whether the keyboard is on it.
        property int order: 0
        readonly property bool marked: menu.opened && list.at === order
        signal picked()
        width: list.width
        height: Style.control
        radius: Style.radius
        color: current ? Style.sel : area.containsMouse ? Style.hover : "transparent"
        border.width: marked ? 2 : 0
        border.color: Style.accent
        Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.u * 2
            anchors.verticalCenter: parent.verticalCenter
            text: option.text
            textFormat: Text.PlainText
            color: option.current || area.containsMouse ? Style.fg : Style.dim
            font.family: Style.font
            font.pixelSize: Style.body
            font.weight: option.current ? Style.strong : Font.Normal
        }
        MouseArea {
            id: area
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: option.picked()
        }
    }

    Popup {
        id: menu
        // Under the control, ending where it ends.
        x: control.width - width
        y: control.height + Style.u
        width: 208
        padding: Style.u * 2
        margins: Style.u * 2
        // In a window that is drawn larger, what is over it is drawn larger
        // too, and the bounds a popup keeps to are then larger than what is
        // seen: the far edges are brought in by the difference.
        readonly property real zoom: control.Window.window && control.Window.window.zoom ? control.Window.window.zoom : 1
        bottomMargin: Style.u * 2 + (Overlay.overlay ? Overlay.overlay.height * (1 - 1 / zoom) : 0)
        rightMargin: Style.u * 2 + (Overlay.overlay ? Overlay.overlay.width * (1 - 1 / zoom) : 0)
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
        // It arrives and goes briefly.
        enter: Transition { NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Style.quick } }
        exit: Transition { NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Style.quick } }
        // Escape is the window's key: it is told which menu is open, and closes it.
        onOpened: {
            if (control.Window.window)
                control.Window.window.openMenu = menu
            // The keyboard starts on the order that holds.
            list.at = control.now.sort === "" ? 0 : control.now.sort === "newest" ? 1 : control.now.sort === "views" ? 2
                    : control.now.sort === "longest" ? 3 : 4
            list.forceActiveFocus()
        }
        // (As it starts to go, not when it has gone: the next Escape is the page's.)
        onAboutToHide: if (control.Window.window && control.Window.window.openMenu === menu) control.Window.window.openMenu = null

        background: Rectangle {
            color: Style.bg
            radius: Style.radius
            border.width: 1
            border.color: Style.line
            Rectangle {
                anchors.fill: parent
                anchors.margins: 1
                radius: parent.radius
                color: Style.rest
            }
        }
        contentItem: Column {
            id: list
            // Which option the keyboard is on (see Option.order). Up and
            // down (k, j) move over the options that are showing; Enter picks.
            property int at: 0
            function option(order) {
                for (let i = 0; i < children.length; i++)
                    if (children[i].picked && children[i].order === order)
                        return children[i]
                return null
            }
            function step(by) {
                for (let to = at + by; to >= 0 && to <= 10; to += by) {
                    const there = option(to)
                    if (there && there.visible) {
                        at = to
                        return
                    }
                }
            }
            Keys.onPressed: event => {
                event.accepted = true
                if (event.key === Qt.Key_J || event.key === Qt.Key_Down)
                    step(1)
                else if (event.key === Qt.Key_K || event.key === Qt.Key_Up)
                    step(-1)
                else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
                    const there = option(at)
                    if (there)
                        there.picked()
                } else
                    event.accepted = false
            }
            Head { text: qsTr("Sort") }
            Option { objectName: "sort-default"; order: 0; text: control.sourceLabel; current: control.now.sort === ""; onPicked: control.set("sort", "") }
            Option {
                objectName: "sort-newest"
                order: 1
                visible: !control.sourceIsNewest
                text: qsTr("Newest")
                current: control.now.sort === "newest"
                onPicked: control.set("sort", "newest")
            }
            Option { objectName: "sort-views"; order: 2; text: qsTr("Most viewed"); current: control.now.sort === "views"; onPicked: control.set("sort", "views") }
            Option { objectName: "sort-longest"; order: 3; text: qsTr("Longest"); current: control.now.sort === "longest"; onPicked: control.set("sort", "longest") }
            Option { objectName: "sort-shortest"; order: 4; text: qsTr("Shortest"); current: control.now.sort === "shortest"; onPicked: control.set("sort", "shortest") }
            Head { text: qsTr("Length") }
            Option { objectName: "length-any"; order: 5; text: qsTr("Any"); current: control.now.length === "any"; onPicked: control.set("length", "any") }
            Option { objectName: "length-short"; order: 6; text: qsTr("Under 10 min"); current: control.now.length === "short"; onPicked: control.set("length", "short") }
            Option { objectName: "length-medium"; order: 7; text: qsTr("10 to 30 min"); current: control.now.length === "medium"; onPicked: control.set("length", "medium") }
            Option { objectName: "length-long"; order: 8; text: qsTr("Over 30 min"); current: control.now.length === "long"; onPicked: control.set("length", "long") }
            Head { text: qsTr("Watched") }
            Option { objectName: "watched-show"; order: 9; text: qsTr("Show"); current: control.now.watched === "show"; onPicked: control.set("watched", "show") }
            Option { objectName: "watched-hide"; order: 10; text: qsTr("Hide"); current: control.now.watched === "hide"; onPicked: control.set("watched", "hide") }
        }
    }
}
