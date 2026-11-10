import QtQuick
import Qveil

// Lenses: feeds described in words. The list, the one that is open (its
// words, how they read, what a refresh asks for), and what it found.
FocusScope {
    id: page
    objectName: "lenses"

    property string thumbs: "frosted"
    property var progress: ({})
    // A new lens is being written, so none of the list is open.
    property bool writing: false
    // The lens that is open, by its id.
    readonly property string current: writing ? "" : (backend.lensInfo.id || "")
    readonly property var lens: {
        for (const l of backend.lenses)
            if (l.id === current)
                return l
        return ({})
    }
    readonly property alias grid: grid
    readonly property var model: grid.model
    // Where focus belongs when the page is shown.
    readonly property Item content: writing ? editor.field : grid

    signal activated(var video, int index)
    signal channelRequested(string id)

    function startNew() {
        writing = true
        Qt.callLater(() => editor.edit())
    }
    function show(id) {
        writing = false
        backend.openLens(id, 1)
    }
    function refresh() {
        if (current !== "")
            backend.openLens(current, 2)
    }
    // Shown with nothing open: open the first lens, or start one if there
    // is none. `listed` is true when the list has just come from the engine:
    // only then does a lens that isn't in it count as gone (one that was
    // just added is open before the list has it).
    function settle(listed) {
        if (!visible || writing)
            return
        if (backend.lenses.length === 0) {
            if (listed || current === "")
                startNew()
        } else if (current === "" || (listed && !backend.lenses.some(l => l.id === current))) {
            show(backend.lenses[0].id)
        }
    }
    onVisibleChanged: settle(false)
    Connections {
        target: backend
        function onLensesChanged() { page.settle(true) }
        function onLensAdded(id) { page.show(id) }
    }

    Column {
        id: head
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        spacing: 3
        Text {
            text: qsTr("Lenses")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.head
            font.bold: true
        }
        Text {
            width: parent.width
            text: qsTr("A lens is a feed you describe in words: what it is about, and any rules, such as a length or what to leave out.")
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.small
        }
    }

    // The list. In a narrow window it gives way to the lens itself.
    Column {
        id: list
        anchors.left: parent.left
        anchors.top: head.bottom
        anchors.leftMargin: Style.gutter
        anchors.topMargin: Style.u * 4
        width: page.width >= 900 ? 300 : 220
        spacing: Style.u

        Repeater {
            model: backend.lenses
            delegate: Rectangle {
                required property var modelData
                readonly property bool current: page.current === modelData.id
                objectName: "lens-row"
                width: list.width
                height: words.height + Style.u * 5
                color: current ? Style.sel : rowArea.containsMouse ? Style.hover : Style.rest
                Behavior on color { ColorAnimation { duration: Style.quick } }
                Column {
                    id: words
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.margins: Style.u * 3
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 3
                    Text {
                        width: parent.width
                        text: modelData.text
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: Style.body
                        font.bold: true
                    }
                    Text {
                        width: parent.width
                        text: modelData.fetchedAt
                              ? qsTr("%n searches", "", (modelData.searches || []).length) + "  ·  " + qsTr("%n videos", "", modelData.count || 0)
                              : qsTr("not fetched yet")
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
                MouseArea {
                    id: rowArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: page.show(modelData.id)
                }
            }
        }
        Act {
            objectName: "lens-new"
            text: qsTr("New lens")
            glyph: "qrc:/icons/plus.svg"
            enabled: backend.lenses.length < 12
            tip: backend.lenses.length < 12 ? "" : qsTr("12 lenses is the most; remove one first")
            onClicked: page.startNew()
        }
    }

    LensEditor {
        id: editor
        anchors.left: list.right
        anchors.right: parent.right
        anchors.top: head.bottom
        anchors.leftMargin: Style.u * 5
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 4
        lens: page.lens
        onSaved: text => {
            if (fresh)
                backend.addLens(text)
            else
                backend.editLens(lens.id, text)
        }
        onRefreshRequested: page.refresh()
        onDeleted: backend.removeLens(lens.id)
    }
    Rule {
        id: under
        anchors.left: editor.left
        anchors.right: editor.right
        anchors.top: editor.bottom
        anchors.topMargin: Style.u * 3
    }
    VideoGrid {
        id: grid
        objectName: "lens-grid"
        anchors.left: editor.left
        anchors.right: parent.right
        anchors.top: under.bottom
        anchors.bottom: parent.bottom
        anchors.topMargin: Style.u * 2
        // The cards have a margin of their own.
        anchors.leftMargin: -Style.u * 2 - gutter
        anchors.rightMargin: Style.gutter - Style.u * 2 - gutter
        model: backend.lensVideos
        visible: !page.writing
        thumbs: page.thumbs
        progress: page.progress
        estimateNote: qsTr("Approximate: YouTube's search only says how long ago")
        onActivated: (video, index) => page.activated(video, index)
        onChannelRequested: id => page.channelRequested(id)
    }
    EmptyState {
        anchors.fill: grid
        visible: !page.writing && grid.count === 0 && backend.busy.indexOf("lens") < 0
        text: page.lens.fetchedAt ? qsTr("Nothing fits this lens right now.")
                                  : qsTr("Nothing fetched for this lens yet. Refresh runs its searches.")
    }
}
