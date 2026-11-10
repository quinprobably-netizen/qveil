import QtQuick
import Qveil

// How a search or a lens was read, as tags: "about systems talks",
// "longer than 30:00". They are the engine's reading, shown as it comes, so
// a rule that was misread is seen before anything is searched.
Flow {
    id: reading

    // [{ label, value }]
    property var tags: []
    // What stands in front of them.
    property string lead: qsTr("Read as")
    readonly property int count: reading.tags ? reading.tags.length : 0

    spacing: Style.u + 2

    Text {
        height: 24
        verticalAlignment: Text.AlignVCenter
        rightPadding: Style.u
        visible: reading.lead !== ""
        text: reading.lead
        textFormat: Text.PlainText
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
    Repeater {
        model: reading.tags
        delegate: Rectangle {
            required property var modelData
            objectName: "tag"
            width: row.implicitWidth + Style.u * 4
            height: 24
            radius: Style.radius
            color: "transparent"
            border.width: 1
            border.color: Style.line
            Row {
                id: row
                anchors.centerIn: parent
                spacing: Style.u + 3
                Text {
                    text: modelData.label
                    textFormat: Text.PlainText
                    color: Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
                Text {
                    text: modelData.value
                    textFormat: Text.PlainText
                    color: Style.fg
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
        }
    }
}
