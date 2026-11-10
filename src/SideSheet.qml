import QtQuick
import Qveil

// Beside the video: what it says about itself, what people say about it,
// and what comes next. One of them at a time.
Rectangle {
    id: sheet
    objectName: "sheet"

    // "next", "comments" or "chapters".
    property string current: "next"
    // How many comments the video has, when that is known.
    property var commentCount: undefined
    // Whether the video has chapters: without them there is no tab for them.
    property bool hasChapters: false
    property alias next: nextSlot.data
    property alias comments: commentsSlot.data
    property alias chapters: chaptersSlot.data
    readonly property var tabsNow: hasChapters ? ["next", "comments", "chapters"] : ["next", "comments"]
    // Whether the keyboard is in the sheet.
    readonly property bool keyed: activeFocus
    // What is in the tab that is showing (it takes step(by) and activate()).
    readonly property Item shown: {
        const slot = current === "comments" ? commentsSlot : current === "chapters" ? chaptersSlot : nextSlot
        return slot.children.length > 0 ? slot.children[0] : null
    }
    onHasChaptersChanged: if (!hasChapters && current === "chapters") current = "next"

    function turn(by) {
        const at = tabsNow.indexOf(current)
        current = tabsNow[Math.max(0, Math.min(tabsNow.length - 1, at + by))]
    }

    color: Style.rest
    activeFocusOnTab: false
    // With the keyboard in it: left and right (h, l) change the tab, up and
    // down (k, j) move in it, Enter acts on what the keyboard is on.
    Keys.onPressed: event => {
        event.accepted = true
        if (event.key === Qt.Key_H || event.key === Qt.Key_Left)
            turn(-1)
        else if (event.key === Qt.Key_L || event.key === Qt.Key_Right)
            turn(1)
        else if ((event.key === Qt.Key_J || event.key === Qt.Key_Down) && shown)
            shown.step(1)
        else if ((event.key === Qt.Key_K || event.key === Qt.Key_Up) && shown)
            shown.step(-1)
        else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && shown)
            shown.activate()
        else
            event.accepted = false
    }
    // (Its edge, while the keyboard is in it.)
    border.width: keyed ? 1 : 0
    border.color: Style.faint

    Row {
        id: tabs
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        TabWord {
            objectName: "tab-next"
            underlined: true
            activeFocusOnTab: false
            text: qsTr("Up next")
            current: sheet.current === "next"
            onClicked: sheet.current = "next"
        }
        TabWord {
            objectName: "tab-comments"
            underlined: true
            activeFocusOnTab: false
            text: sheet.commentCount > 0 ? qsTr("Comments %1").arg(Number(sheet.commentCount).toLocaleString(Qt.locale(), "f", 0))
                                         : qsTr("Comments")
            current: sheet.current === "comments"
            onClicked: sheet.current = "comments"
        }
        TabWord {
            objectName: "tab-chapters"
            underlined: true
            activeFocusOnTab: false
            visible: sheet.hasChapters
            text: qsTr("Chapters")
            current: sheet.current === "chapters"
            onClicked: sheet.current = "chapters"
        }
    }
    Rule {
        id: under
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: tabs.bottom
    }
    Item {
        id: nextSlot
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: under.bottom
        anchors.bottom: parent.bottom
        visible: sheet.current === "next"
    }
    Item {
        id: commentsSlot
        anchors.fill: nextSlot
        visible: sheet.current === "comments"
    }
    Item {
        id: chaptersSlot
        anchors.fill: nextSlot
        visible: sheet.current === "chapters"
    }
}
