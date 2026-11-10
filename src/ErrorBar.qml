import QtQuick
import Qveil

// What went wrong, in the engine's words, until it is dismissed. It is part
// of the column, so the page gets shorter instead of the bar lying on it.
Rectangle {
    id: bar

    signal routeRequested()

    visible: backend.error !== ""
    implicitHeight: Math.max(Style.control + Style.u * 4, message.implicitHeight + Style.u * 6)
    color: Qt.rgba(0.94, 0.42, 0.54, 0.12)

    Rule {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        color: Qt.rgba(0.94, 0.42, 0.54, 0.45)
    }
    Text {
        id: message
        anchors.left: parent.left
        anchors.leftMargin: Style.gutter
        anchors.right: actions.left
        anchors.rightMargin: Style.u * 3
        anchors.verticalCenter: parent.verticalCenter
        text: backend.error
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: Style.fg
        font.family: Style.font
        font.pixelSize: Style.body
    }
    Row {
        id: actions
        anchors.right: parent.right
        anchors.rightMargin: Style.gutter - Style.u * 2
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.u

        // The only thing an error offers to do: for the errors another
        // route may get past (a bot check, a route that is down).
        Act {
            objectName: "changeRoute"
            visible: backend.errorOffersRoute
            text: qsTr("Change route")
            filled: true
            onClicked: {
                backend.clearError()
                bar.routeRequested()
            }
        }
        Act {
            objectName: "dismissError"
            glyph: "qrc:/icons/close.svg"
            tip: qsTr("Dismiss")
            onClicked: backend.clearError()
        }
    }
}
