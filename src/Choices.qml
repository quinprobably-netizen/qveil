import QtQuick
import Qveil

// A few choices of which one holds, drawn as one control: the words side by
// side in a frame, the chosen one filled. What goes inside is TabWords.
Rectangle {
    default property alias options: inner.data
    // The choice before or after the one that holds, for the keyboard.
    function turn(by) {
        const found = []
        for (let i = 0; i < inner.children.length; i++)
            if (inner.children[i].visible && inner.children[i].clicked)
                found.push(inner.children[i])
        let at = 0
        for (let i = 0; i < found.length; i++)
            if (found[i].current)
                at = i
        const to = Math.max(0, Math.min(found.length - 1, at + by))
        if (to !== at)
            found[to].clicked()
    }

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
