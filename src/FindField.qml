import QtQuick
import Qveil

// The search field. While something is typed it asks the engine how that
// reads (nothing leaves the machine for it), so that a search with a rule in
// it shows what the rule is before Enter.
Field {
    id: search

    placeholder: qsTr("Search, or describe what you're after")
    hint: "/"
    glyph: "qrc:/icons/search.svg"
    maximumLength: 500

    onEdited: text => {
        if (text.trim() === "")
            backend.read("")
        else
            ask.restart()
    }
    // Not once per key: once the typing pauses.
    Timer {
        id: ask
        interval: 150
        onTriggered: backend.read(search.text)
    }
}
