import QtQuick
import Qveil

// A few choices of which one holds, drawn as one control: the words side by
// side in a frame, the chosen one filled. What goes inside is TabWords.
Rectangle {
    default property alias options: inner.data

    implicitWidth: inner.width + 2
    implicitHeight: inner.height + 2
    color: "transparent"
    radius: Style.radius
    border.width: 1
    border.color: Style.line

    Row {
        id: inner
        x: 1
        y: 1
    }
}
