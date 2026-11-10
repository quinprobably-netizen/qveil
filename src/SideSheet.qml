import QtQuick
import Qveil

// Beside the video: what it says about itself, what people say about it,
// and what comes next. One of them at a time.
Rectangle {
    id: sheet
    objectName: "sheet"

    // "about", "comments" or "next".
    property string current: "about"
    // How many comments the video has, when that is known.
    property var commentCount: undefined
    property alias about: aboutSlot.data
    property alias comments: commentsSlot.data
    property alias next: nextSlot.data

    color: Style.rest

    Row {
        id: tabs
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        TabWord {
            objectName: "tab-about"
            text: qsTr("About")
            current: sheet.current === "about"
            onClicked: sheet.current = "about"
        }
        TabWord {
            objectName: "tab-comments"
            text: sheet.commentCount > 0 ? qsTr("Comments %1").arg(Number(sheet.commentCount).toLocaleString(Qt.locale(), "f", 0))
                                         : qsTr("Comments")
            current: sheet.current === "comments"
            onClicked: sheet.current = "comments"
        }
        TabWord {
            objectName: "tab-next"
            text: qsTr("Up next")
            current: sheet.current === "next"
            onClicked: sheet.current = "next"
        }
    }
    Rule {
        id: under
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: tabs.bottom
    }
    Item {
        id: aboutSlot
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: under.bottom
        anchors.bottom: parent.bottom
        visible: sheet.current === "about"
    }
    Item {
        id: commentsSlot
        anchors.fill: aboutSlot
        visible: sheet.current === "comments"
    }
    Item {
        id: nextSlot
        anchors.fill: aboutSlot
        visible: sheet.current === "next"
    }
}
