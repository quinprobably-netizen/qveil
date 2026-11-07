import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material

// A compact search field for the top bar (a Material TextField is 56px tall
// and floats its placeholder).
FocusScope {
    id: box

    property alias text: input.text
    signal submitted(string query)

    function selectAll() { input.selectAll() }

    implicitHeight: 38

    Rectangle {
        anchors.fill: parent
        radius: Math.min(19, theme.radius)
        color: input.activeFocus ? "#1a1a20" : "#16161b"
        border.color: input.activeFocus ? theme.accent : "#2a2a32"
    }

    Image {
        id: glass
        source: "qrc:/icons/search.svg"
        sourceSize: Qt.size(18, 18)
        opacity: 0.5
        anchors.left: parent.left
        anchors.leftMargin: 12
        anchors.verticalCenter: parent.verticalCenter
    }

    TextInput {
        id: input
        anchors.left: glass.right
        anchors.right: parent.right
        anchors.leftMargin: 10
        anchors.rightMargin: 12
        anchors.verticalCenter: parent.verticalCenter
        focus: true
        clip: true
        color: "#ececf1"
        selectionColor: theme.accent
        selectedTextColor: theme.accentForeground
        font.pixelSize: 14
        selectByMouse: true
        onAccepted: if (text.trim() !== "") box.submitted(text.trim())

        Label {
            anchors.verticalCenter: parent.verticalCenter
            visible: input.text === ""
            text: qsTr("Search YouTube")
            font.pixelSize: 14
            color: "#6f6f7b"
        }
    }

    Label {
        anchors.right: parent.right
        anchors.rightMargin: 10
        anchors.verticalCenter: parent.verticalCenter
        visible: !input.activeFocus && input.text === ""
        text: "/"
        font.pixelSize: 12
        color: "#55555f"
    }
}
