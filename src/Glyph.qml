import QtQuick
import QtQuick.Controls.impl
import Qveil

// One of the icons, in a colour. (IconImage tints on the CPU, so it also
// draws where there is no GPU.)
IconImage {
    property int size: 18
    width: size
    height: size
    sourceSize: Qt.size(size, size)
    color: Style.fg
}
