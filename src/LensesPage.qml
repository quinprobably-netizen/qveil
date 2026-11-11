import QtQuick
import Qveil

// Lenses: feeds described in words. They are a strip along the top; under it
// the one that is open: its words, how they read, what a refresh asks for,
// and what it found, across the whole width.
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
    readonly property var model: grid.list
    // How each list is sorted and filtered, by its key, and what is known to
    // have been watched.
    property var arrangements: ({})
    property var watched: ({})
    signal arranged(string key, var setting)
    // Where focus belongs when the page is shown.
    readonly property Item content: writing ? editor.field : grid

    signal activated(var video, int index)
    signal channelRequested(string id)

    function openSort() { if (!writing && current !== "") lensSort.open() }
    function startNew() {
        writing = true
        Qt.callLater(() => editor.edit())
    }
    // The lens before or after the one that is open, along the strip.
    function step(by) {
        const ids = backend.lenses.map(l => l.id)
        if (ids.length === 0)
            return
        const at = ids.indexOf(current)
        const to = at < 0 ? 0 : Math.max(0, Math.min(ids.length - 1, at + by))
        if (ids[to] !== current)
            show(ids[to])
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

    // The lenses, along the top: the open one marked, and the way to a new
    // one at the end, where it is never scrolled out of reach.
    Item {
        id: head
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 2
        height: Style.control + Style.u

        Flickable {
            id: strip
            objectName: "lens-list"
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: Math.min(lensRow.width, parent.width - fresh.width - Style.u * 3)
            height: Style.control
            contentWidth: lensRow.width
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            flickableDirection: Flickable.HorizontalFlick

            Row {
                id: lensRow
                Repeater {
                    model: backend.lenses
                    delegate: TabWord {
                        required property var modelData
                        objectName: "lens-row"
                        underlined: true
                        activeFocusOnTab: false
                        // A lens can be a long sentence; its tab is the start of it.
                        text: modelData.text.length > 34 ? modelData.text.slice(0, 33) + "\u2026" : modelData.text
                        current: page.current === modelData.id
                        onClicked: page.show(modelData.id)
                        // The open one is kept in view.
                        onCurrentChanged: if (current) {
                            if (x < strip.contentX)
                                strip.contentX = x
                            else if (x + width > strip.contentX + strip.width)
                                strip.contentX = x + width - strip.width
                        }
                    }
                }
            }
        }
        Act {
            id: fresh
            objectName: "lens-new"
            anchors.left: strip.right
            anchors.leftMargin: Style.u
            anchors.verticalCenter: parent.verticalCenter
            text: qsTr("New lens")
            glyph: "qrc:/icons/plus.svg"
            quiet: !page.writing
            enabled: backend.lenses.length < 12
            tip: backend.lenses.length < 12 ? "" : qsTr("12 lenses is the most; remove one first")
            onClicked: page.startNew()
        }
        Rule {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
        }
    }

    // The lens's words and what goes with them. In a short window they
    // scroll, so that what the lens found keeps room under them.
    Flickable {
        id: editorView
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: head.bottom
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        anchors.topMargin: Style.u * 3
        height: Math.min(editor.height, Math.max(96, page.height - head.y - head.height - Style.u * 3 - (page.writing ? Style.gutter : 240)))
        contentHeight: editor.height
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        LensEditor {
            id: editor
            width: parent.width
            lens: page.lens
            onSaved: text => {
                // (`editor.` by name: the page has an Act whose id is `fresh`.)
                if (editor.fresh)
                    backend.addLens(text)
                else
                    backend.editLens(editor.lens.id, text)
            }
            onRefreshRequested: page.refresh()
            onDeleted: backend.removeLens(lens.id)

            SortMenu {
                id: lensSort
                objectName: "lens-sort"
                visible: !page.writing && page.current !== ""
                setting: page.arrangements[page.current] || ({})
                sourceLabel: qsTr("Best match")
                onChanged: setting => page.arranged(page.current, setting)
            }
        }
    }
    VideoGrid {
        id: grid
        objectName: "lens-grid"
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: editorView.bottom
        anchors.bottom: parent.bottom
        anchors.topMargin: Style.u * 2
        source: backend.lensVideos
        arrange: page.arrangements[page.current] || ({})
        watched: page.watched
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
