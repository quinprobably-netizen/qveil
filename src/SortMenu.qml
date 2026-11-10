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
           now.watched === "hide" ? qsTr("unwatched") : ""].filter(s => s !== "").join("  ·  ")
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
        signal picked()
        width: list.width
        height: Style.control
        radius: Style.radius
        color: current ? Style.sel : area.containsMouse ? Style.hover : "transparent"
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
        onOpened: if (control.Window.window) control.Window.window.openMenu = menu
        onClosed: if (control.Window.window && control.Window.window.openMenu === menu) control.Window.window.openMenu = null

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
            Head { text: qsTr("Sort") }
            Option { objectName: "sort-default"; text: control.sourceLabel; current: control.now.sort === ""; onPicked: control.set("sort", "") }
            Option {
                objectName: "sort-newest"
                visible: !control.sourceIsNewest
                text: qsTr("Newest")
                current: control.now.sort === "newest"
                onPicked: control.set("sort", "newest")
            }
            Option { objectName: "sort-views"; text: qsTr("Most viewed"); current: control.now.sort === "views"; onPicked: control.set("sort", "views") }
            Option { objectName: "sort-longest"; text: qsTr("Longest"); current: control.now.sort === "longest"; onPicked: control.set("sort", "longest") }
            Option { objectName: "sort-shortest"; text: qsTr("Shortest"); current: control.now.sort === "shortest"; onPicked: control.set("sort", "shortest") }
            Head { text: qsTr("Length") }
            Option { objectName: "length-any"; text: qsTr("Any"); current: control.now.length === "any"; onPicked: control.set("length", "any") }
            Option { objectName: "length-short"; text: qsTr("Under 10 min"); current: control.now.length === "short"; onPicked: control.set("length", "short") }
            Option { objectName: "length-medium"; text: qsTr("10 to 30 min"); current: control.now.length === "medium"; onPicked: control.set("length", "medium") }
            Option { objectName: "length-long"; text: qsTr("Over 30 min"); current: control.now.length === "long"; onPicked: control.set("length", "long") }
            Head { text: qsTr("Watched") }
            Option { objectName: "watched-show"; text: qsTr("Show"); current: control.now.watched === "show"; onPicked: control.set("watched", "show") }
            Option { objectName: "watched-hide"; text: qsTr("Hide"); current: control.now.watched === "hide"; onPicked: control.set("watched", "hide") }
        }
    }
}
