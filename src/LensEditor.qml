import QtQuick
import Qveil

// A lens's words, how they read, and what a refresh will ask YouTube for.
Column {
    id: editor

    // The lens being changed: api/lenses' entry, or {} for one being written.
    property var lens: ({})
    readonly property bool fresh: !lens.id
    readonly property string typed: area.text.trim()
    readonly property bool changed: typed !== (lens.text || "")
    // The engine's reading of what is typed, once it has answered for it.
    readonly property var reading: backend.reading.text === typed ? backend.reading : ({})
    readonly property var tags: changed ? (reading.tags || []) : (lens.tags || [])
    readonly property var searches: changed ? (reading.searches || []) : (lens.searches || [])
    readonly property string error: changed ? (reading.error || "") : ""
    readonly property bool refreshing: backend.busy.indexOf("lens") >= 0
    // Whether Delete has been pressed once, and waits to be pressed again.
    property bool arming: false
    // What takes the keyboard.
    readonly property Item field: area.input
    // Which lens the words in the field are for.
    readonly property string lensId: lens.id || ""
    // Whether the whole of what a refresh means is showing.
    property bool telling: false
    // Other actions, after the lens's own (the list's sort control).
    default property alias more: extra.data
    signal saved(string text)
    signal refreshRequested()
    signal deleted()

    function edit() { area.edit() }
    function save() {
        if (changed && typed !== "" && error === "" && reading.text === typed)
            saved(typed)
    }

    // Another lens: its words, and nothing half-decided left over. (The
    // same lens comes again whenever the list is reloaded, which is no
    // reason to throw away what is being typed or asked.)
    onLensIdChanged: {
        area.text = lens.text || ""
        arming = false
    }
    Component.onCompleted: area.text = lens.text || ""

    spacing: Style.u * 2

    Area {
        id: area
        objectName: "lens-text"
        width: parent.width
        // It heads the page: the lens's own words, as large as a heading.
        least: 44
        placeholder: qsTr("Describe a feed: what it's about, and any rules. For example: slow woodworking, no music, longer than 15 minutes")
        onEdited: text => {
            editor.arming = false
            ask.restart()
        }
        onSubmitted: editor.save()
        Timer {
            id: ask
            interval: 150
            onTriggered: backend.read(area.text, true)
        }
        Text {
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.margins: Style.u
            visible: area.text.length >= 400
            text: area.text.length + " / " + area.maximumLength
            textFormat: Text.PlainText
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.caption
        }
    }
    // How the words read, and beside that what can be done with the lens.
    Item {
        width: parent.width
        height: Math.max(readAs.height, acts.height)
        ReadingTags {
            // (Not `reading`: that is the editor's own property.)
            id: readAs
            objectName: "lens-tags"
            anchors.left: parent.left
            anchors.right: acts.left
            anchors.rightMargin: Style.u * 3
            anchors.verticalCenter: parent.verticalCenter
            visible: count > 0
            tags: editor.tags
        }
        Row {
            id: acts
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.u
            Row {
                id: extra
                spacing: Style.u
            }
            Act {
                objectName: "lens-save"
                visible: editor.changed || editor.fresh
                text: qsTr("Save")
                filled: true
                enabled: editor.changed && editor.typed !== "" && editor.error === "" && editor.reading.text === editor.typed
                onClicked: editor.save()
            }
            Act {
                objectName: "lens-refresh"
                visible: !editor.fresh && !editor.changed
                text: qsTr("Refresh")
                glyph: "qrc:/icons/refresh.svg"
                filled: true
                enabled: !editor.refreshing
                onClicked: editor.refreshRequested()
            }
            Act {
                objectName: "lens-delete"
                visible: !editor.fresh
                text: editor.arming ? qsTr("Delete?") : qsTr("Delete")
                onClicked: {
                    if (editor.arming)
                        editor.deleted()
                    editor.arming = !editor.arming
                }
            }
            Act {
                objectName: "lens-cancel"
                visible: editor.arming
                text: qsTr("Cancel")
                quiet: true
                onClicked: editor.arming = false
            }
        }
    }
    Text {
        objectName: "lens-error"
        width: parent.width
        visible: text !== ""
        text: editor.error
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: Style.warn
        font.family: Style.font
        font.pixelSize: Style.small
    }
    // What a refresh asks for, and what that means: the engine's short
    // sentence for it, with its whole text a press away, in place.
    Flow {
        width: parent.width
        spacing: Style.u * 2
        Text {
            objectName: "lens-searches"
            visible: editor.searches.length > 0
            text: editor.refreshing && backend.lensTotal > 0
                  ? qsTr("Refreshing %1 of %2").arg(backend.lensDone).arg(backend.lensTotal)
                  : qsTr("A refresh searches for: %1").arg(editor.searches.map(s => "\u201c" + s + "\u201d").join(", "))
            textFormat: Text.PlainText
            color: Style.dim
            font.family: Style.font
            font.pixelSize: Style.small
        }
        Text {
            objectName: "lens-note-first"
            visible: text !== ""
            text: backend.privacy.pages.lenses_short || ""
            textFormat: Text.PlainText
            color: Style.faint
            font.family: Style.font
            font.pixelSize: Style.small
        }
        Text {
            objectName: "lens-note-more"
            visible: !!backend.privacy.pages.lenses
            // (Only a way to the rest of the engine's text: the window says nothing of its own here.)
            text: editor.telling ? qsTr("Less") : qsTr("More")
            textFormat: Text.PlainText
            color: moreArea.containsMouse ? Style.fg : Style.accent
            font.family: Style.font
            font.pixelSize: Style.small
            MouseArea {
                id: moreArea
                anchors.fill: parent
                anchors.margins: -4
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: editor.telling = !editor.telling
            }
        }
    }
    // The whole of it, in the engine's words.
    Text {
        objectName: "lens-note"
        width: parent.width
        visible: editor.telling && text !== ""
        text: backend.privacy.pages.lenses || ""
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        lineHeight: 1.2
        color: Style.faint
        font.family: Style.font
        font.pixelSize: Style.small
    }
}
