import QtQuick
import Qveil

// What people wrote under the video. They are fetched when this is first
// shown for a video, and never before: it costs a lookup of its own.
Item {
    id: tab
    objectName: "comments"

    // The video's ID.
    property string video
    // Whether the tab is the one showing.
    property bool active: false
    property string sort: "top"
    property int n: 40
    property bool replies: false
    // The engine's answer, when it is for this video.
    readonly property var answer: backend.comments.video === video ? backend.comments : ({})
    readonly property var items: answer.items || []
    readonly property int shown: items.length
    readonly property bool loaded: answer.video !== undefined
    readonly property bool loading: backend.busy.indexOf("comments") >= 0
    // How many the video has in all, when YouTube says.
    readonly property var count: answer.count
    signal linkActivated(string link)

    // The video this tab last asked about.
    property string asked
    function load() {
        if (video !== "") {
            asked = video
            backend.loadComments(video, sort, n, replies)
        }
    }
    // The keyboard scrolls them.
    property bool keyed: false
    function step(by) {
        flick.contentY = Math.max(0, Math.min(Math.max(0, flick.contentHeight - flick.height), flick.contentY + by * 96))
    }
    function activate() {}
    function more() {
        n = Math.min(200, n + 40)
        load()
    }
    // Another video starts from the top.
    onVideoChanged: {
        sort = "top"
        n = 40
        replies = false
        if (active && video !== "")
            load()
    }
    onActiveChanged: if (active && !loaded && !loading) load()
    // A lookup for another video was still running when this was shown, so
    // nothing was asked: it is now. (A lookup of this video that failed is
    // not asked again by itself.)
    onLoadingChanged: if (!loading && active && !loaded && asked !== video) load()

    Flickable {
        id: flick
        anchors.fill: parent
        contentHeight: column.height + Style.u * 6
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        Column {
            id: column
            y: Style.u * 2
            width: flick.width
            spacing: 0

            // What fetching them means, in the engine's words.
            Text {
                objectName: "comments-note"
                x: Style.u * 3
                width: parent.width - Style.u * 6
                visible: text !== ""
                bottomPadding: Style.u * 2
                text: backend.privacy.pages.comments || ""
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Style.faint
                font.family: Style.font
                font.pixelSize: Style.caption
            }
            Row {
                x: Style.u
                spacing: 2
                TabWord {
                    objectName: "comments-top"
                    text: qsTr("Top")
                    current: tab.sort === "top"
                    onClicked: {
                        tab.sort = "top"
                        tab.load()
                    }
                }
                TabWord {
                    objectName: "comments-new"
                    text: qsTr("Newest")
                    current: tab.sort === "new"
                    onClicked: {
                        tab.sort = "new"
                        tab.load()
                    }
                }
            }
            BusyLine {
                width: parent.width
                opacity: tab.loading ? 1 : 0
            }
            Text {
                objectName: "comments-empty"
                x: Style.u * 3
                width: parent.width - Style.u * 6
                visible: tab.loaded && !tab.loading && tab.items.length === 0
                topPadding: Style.u * 4
                text: qsTr("No comments here, or they are turned off.")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Style.dim
                font.family: Style.font
                font.pixelSize: Style.small
            }
            Repeater {
                model: tab.items
                delegate: Column {
                    id: thread
                    required property var modelData
                    readonly property string author: modelData.author || ""
                    readonly property bool pinned: !!modelData.pinned
                    objectName: "comment"
                    width: column.width
                    spacing: 0

                    Rule { width: parent.width }
                    Item { width: 1; height: Style.u * 2 + 2 }
                    CommentItem {
                        x: Style.u * 3
                        width: parent.width - Style.u * 6
                        comment: thread.modelData
                        onLinkActivated: link => tab.linkActivated(link)
                    }
                    Repeater {
                        model: thread.modelData.replies || []
                        delegate: Item {
                            required property var modelData
                            objectName: "reply"
                            width: thread.width
                            height: answer.height + Style.u * 2
                            Rectangle {
                                x: Style.u * 3
                                y: Style.u * 2
                                width: 1
                                height: answer.height
                                color: Style.line
                            }
                            CommentItem {
                                id: answer
                                x: Style.gutter
                                y: Style.u * 2
                                width: parent.width - Style.gutter - Style.u * 3
                                comment: modelData
                                reply: true
                                onLinkActivated: link => tab.linkActivated(link)
                            }
                        }
                    }
                    Item { width: 1; height: Style.u * 2 + 2 }
                }
            }
            Rule {
                width: parent.width
                visible: tab.items.length > 0
            }
            Row {
                x: Style.u
                topPadding: Style.u
                spacing: Style.u
                visible: tab.items.length > 0
                Act {
                    objectName: "comments-more"
                    // yt-dlp has no next page: more is asked for from the start.
                    visible: tab.n < 200 && tab.items.length >= tab.n
                    text: qsTr("More")
                    enabled: !tab.loading
                    onClicked: tab.more()
                }
                Act {
                    objectName: "comments-replies"
                    visible: !tab.replies
                    text: qsTr("Show replies")
                    tip: qsTr("Asks again, with up to three replies under each comment")
                    enabled: !tab.loading
                    onClicked: {
                        tab.replies = true
                        tab.load()
                    }
                }
            }
        }
    }
}
