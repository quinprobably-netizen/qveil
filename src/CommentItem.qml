import QtQuick
import Qveil
import "Format.js" as Format
import "Linkify.js" as Linkify

// One comment: who and when, what they wrote, and how it was liked. Replies
// hang under it, a step in. No picture: commenters' pictures are never fetched.
Column {
    id: item

    // { author, text, likes, timeText, pinned, byUploader, replies }
    property var comment: ({})
    property bool reply: false
    readonly property string author: comment.author || ""
    readonly property bool pinned: !!comment.pinned
    signal linkActivated(string link)

    spacing: 3

    Text {
        width: parent.width
        text: {
            const parts = []
            if (item.pinned)
                parts.push(qsTr("pinned"))
            if (item.comment.timeText)
                parts.push(item.comment.timeText)
            return parts.join("  \u00b7  ")
        }
        textFormat: Text.PlainText
        leftPadding: who.implicitWidth + (text !== "" ? Style.u * 2 : 0)
        elide: Text.ElideRight
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small

        Text {
            id: who
            text: item.author
            textFormat: Text.PlainText
            // The video's own channel stands out.
            color: item.comment.byUploader ? Style.accent : Style.dim
            font.family: Style.font
            font.pixelSize: Style.small
            font.weight: Style.strong
        }
    }
    Text {
        objectName: "comment-text"
        width: parent.width
        text: Linkify.html(item.comment.text || "", Style.accent)
        textFormat: Text.StyledText
        wrapMode: Text.Wrap
        lineHeight: 1.2
        color: Style.fg
        linkColor: Style.accent
        font.family: Style.font
        font.pixelSize: Style.small
        onLinkActivated: link => item.linkActivated(link)
    }
    Text {
        visible: item.comment.likes > 0
        text: "\u2665 " + Format.count(item.comment.likes)
        textFormat: Text.PlainText
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.caption
    }
}
