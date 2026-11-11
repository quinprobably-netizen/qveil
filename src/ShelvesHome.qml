import QtQuick
import QtQuick.Controls
import Qveil
import "Format.js" as Format
import "Arrange.js" as Arrange

// Home, as it opens: the video you were in the middle of leads the page, and
// under it every feed is a shelf: the other unfinished videos, the latest
// from the channels you follow, and each of your lenses.
FocusScope {
    id: page
    objectName: "shelves-home"

    property string thumbs: "clear"
    // How far each unfinished video was watched: { id: fraction }.
    property var progress: ({})
    // A line about the channels' feed: how many, or how the refresh is going.
    property string status
    property bool loading: false
    property bool noChannels: false
    // How long positions are kept, in days.
    readonly property int days: backend.config.resumeDays || 0
    // The video that leads: the one most recently left unfinished.
    readonly property var first: backend.resume.length > 0 ? backend.resume[0] : null
    // The shelves, by key: "latest", "resume" (what else was left
    // unfinished, when there is more than the lead), then each lens's id. Kept as
    // it is while the same shelves are wanted, so that a list that is read
    // again doesn't rebuild the page.
    property var keys: ["latest"]
    // Where the keyboard is: which shelf, and (remembered per shelf) which video on it.
    property int row: 0
    property var columns: ({})
    readonly property int column: columns[keys[row]] || 0
    // The keyboard's mark shows once the keyboard is used, and hides under the pointer.
    // (It is there from the start: the keyboard needs no first key to show where it is.)
    property bool navigating: true
    // Whether the keyboard is on the lead, above the first shelf, and on
    // which of its actions (0 resume, 1 start over, 2 forget). It starts
    // there when there is a lead: Enter picks up where you were.
    property bool onLead: false
    property int leadAction: 0
    property bool moved: false
    readonly property bool leadSelected: navigating && activeFocus && onLead && first !== null
    onFirstChanged: {
        // A new video leads with Resume marked (Forget on the last must not
        // carry over to the next).
        leadAction = 0
        if (first === null)
            onLead = false
        else if (!moved)
            onLead = true
    }
    // How many videos a shelf shows; the rest is a step away, in full.
    readonly property int most: 12
    // In a short tile the lead is a strip, so that a whole shelf fits under it.
    readonly property bool compact: height < 560
    readonly property int leadHeight: compact ? 106 : width < 840 ? 200 : 264
    // How many cards fit across, and how tall one is (as in Shelf).
    readonly property int across: Math.max(2, Math.floor((view.width + Style.u * 4) / 280))
    readonly property int cardHeight: Math.round((Math.floor((view.width + Style.u * 4) / across) - Style.u * 4) * 9 / 16) + 104
    // The latest take the rows the window has free under the lead and beside
    // the other shelves, so that a window with little else in it is not left
    // empty: one row that scrolls sideways when there is no room for more.
    readonly property int latestRows: {
        const shelf = Style.control + Style.u * 2 + cardHeight + 18 + view.spacing
        const lead = first !== null ? leadHeight + view.spacing : 0
        const free = view.height - view.topMargin - lead - (keys.length - 1) * shelf
                     - Style.control - Style.u * 2 - (Style.control + 2 * view.spacing)
        return Math.max(1, Math.min(6, Math.ceil(free / cardHeight)))
    }

    // How each shelf is sorted and filtered, by its key (see Arrange.js).
    property var arrangements: ({})
    // What is known to have been watched: { id: true }.
    property var watched: ({})
    signal arranged(string key, var setting)
    // The topic feeds, while they are on: api/topics' groups.
    // A group that was never searched for is only a shelf while it is being
    // searched for: otherwise there is nothing to show for it, and nothing coming.
    readonly property bool searching: backend.busy.indexOf("topics") >= 0
    readonly property var topics: backend.topics.enabled
        ? (backend.topics.groups || []).filter(g => g.fetchedAt || searching) : []
    onTopicsChanged: rekey()
    function topicOf(key) {
        for (const group of topics)
            if ("topic-" + group.id === key)
                return group
        return ({})
    }
    signal activated(var video, var list, int index)
    signal resumed(var video)
    signal allRequested(string feed)
    signal channelRequested(string id)
    signal refreshRequested()
    signal addChannelRequested()
    signal newLensRequested()

    // When each lens's shelf was last read, by what the lens had fetched
    // then: a shelf is read once, and again when its lens is fetched anew.
    // (Not a property: nothing is drawn from it.)
    readonly property var read: ({})
    function wantShelf(lens) {
        if (!lens.id || !lens.fetchedAt || read[lens.id] === lens.fetchedAt)
            return
        read[lens.id] = lens.fetchedAt
        backend.loadShelf(lens.id)
    }
    function rekey() {
        const next = ["latest"]
        if (backend.resume.length > 1)
            next.push("resume")
        for (const lens of backend.lenses)
            next.push(lens.id)
        for (const group of topics)
            next.push("topic-" + group.id)
        if (next.join("|") !== keys.join("|")) {
            keys = next
            row = Math.max(0, Math.min(row, keys.length - 1))
        }
    }
    function lensOf(key) {
        for (const lens of backend.lenses)
            if (lens.id === key)
                return lens
        return ({})
    }
    // All of a shelf's videos, as it is sorted and filtered.
    function sourceOf(key) {
        let all
        if (key === "resume") {
            all = []
            for (const kept of backend.resume.slice(1))
                all.push({ id: kept.id, title: kept.title || "", channel: kept.channel || "", duration: kept.dur,
                           why: [qsTr("%1 left").arg(Format.clock(Math.max(0, kept.dur - kept.pos)))] })
        } else if (key === "latest") {
            // As it came, only as much of it as is shown is read.
            // (The video that leads is not shown again under it.)
            const lead = first !== null ? first.id : ""
            if (Arrange.plain(arrangements[key])) {
                all = []
                const n = latestRows > 1 ? latestRows * across : most
                for (let i = 0; i < backend.feed.count && all.length < n; i++) {
                    const v = backend.feed.get(i)
                    if (v.id !== lead)
                        all.push(v)
                }
                return all
            }
            all = Arrange.rows(backend.feed).filter(v => v.id !== lead)
        } else if (key.indexOf("topic-") === 0) {
            all = topicOf(key).videos || []
        } else {
            all = backend.shelves[key] || []
        }
        // A live stream can't be played here, so it isn't offered.
        all = all.filter(v => !v.live)
        // (What is left unfinished is not "watched" on its own shelf.)
        return Arrange.apply(all, arrangements[key], key === "resume" ? ({}) : watched)
    }
    // What a shelf shows of them.
    function entriesOf(key) {
        const n = key === "latest" && latestRows > 1 ? latestRows * across : most
        return sourceOf(key).slice(0, n)
    }
    function pick(key, video, index) {
        if (key === "resume") {
            page.resumed(video)
        } else if (key === "latest" && Arrange.plain(arrangements[key])) {
            // The shelf is the start of the feed: what comes next is the rest of
            // it, from where the video is in the feed (the lead's is left out here).
            let at = index
            for (let i = 0; i < backend.feed.count; i++)
                if (backend.feed.get(i).id === video.id) {
                    at = i
                    break
                }
            page.activated(video, backend.feed, at)
        } else {
            page.activated(video, Arrange.listed(sourceOf(key)), index)
        }
    }
    // The cursor steps aside when the pointer moves over the page (not when
    // it only rests there as the page opens).
    property point pointerAt: Qt.point(-1, -1)
    function pointed(at) {
        if (pointerAt.x >= 0 && (at.x !== pointerAt.x || at.y !== pointerAt.y))
            navigating = false
        pointerAt = at
    }
    function move(by) {
        navigating = true
        if (onLead) {
            // Along the lead's actions.
            leadAction = Math.max(0, Math.min(2, leadAction + by))
            return
        }
        const key = keys[row]
        const last = entriesOf(key).length - 1
        const next = Object.assign({}, columns)
        next[key] = Math.max(0, Math.min(last, column + by))
        columns = next
    }
    function step(by) {
        navigating = true
        moved = true
        if (onLead) {
            if (by > 0) {
                onLead = false
                Qt.callLater(ensure)
            }
            return
        }
        // Up from the top of the first shelf is the lead.
        if (by < 0 && row === 0 && first !== null && !(keys[0] === "latest" && latestRows > 1 && column >= across)) {
            onLead = true
            leadAction = 0
            scrollTo(view.originY - view.topMargin)
            return
        }
        // Within the rows of the latest first; then to the shelf before or after.
        if (keys[row] === "latest" && latestRows > 1) {
            const to = column + by * across
            if (to >= 0 && to < entriesOf("latest").length) {
                const next = Object.assign({}, columns)
                next.latest = to
                columns = next
                Qt.callLater(ensure)
                return
            }
        }
        row = Math.max(0, Math.min(keys.length - 1, row + by))
        Qt.callLater(ensure)
    }
    // Brings what the keyboard is on into view.
    function ensure() {
        const shelf = view.itemAtIndex(row)
        if (!shelf) {
            view.positionViewAtIndex(row, ListView.Contain)
            return
        }
        const top = shelf.y + shelf.rowTop(column)
        const bottom = top + shelf.cellHeight
        if (row === 0 && top - shelf.y < shelf.cellHeight)
            scrollTo(view.originY - view.topMargin)
        else if (bottom > view.contentY + view.height)
            scrollTo(bottom - view.height + Style.u * 2)
        else if (top < view.contentY)
            scrollTo(shelf.y)
    }
    // The page glides there.
    function scrollTo(y) {
        scroll.stop()
        if (Style.move === 0) {
            view.contentY = y
            return
        }
        scroll.to = y
        scroll.start()
    }
    NumberAnimation {
        id: scroll
        target: view
        property: "contentY"
        duration: Style.move * 1.4
        easing.type: Easing.OutCubic
    }
    // The sort menu of the shelf the keyboard is on, and that shelf in full.
    function openSort() {
        const shelf = view.itemAtIndex(row)
        if (shelf)
            shelf.openSort()
    }
    function openCurrent() {
        const key = keys[row]
        if (key !== "resume" && key.indexOf("topic-") !== 0)
            page.allRequested(key)
    }
    function pickCurrent() {
        if (onLead && first !== null) {
            const kept = first
            if (leadAction === 1)
                startOver(kept)
            else if (leadAction === 2)
                backend.forgetResume(kept.id)
            else
                page.resumed({ id: kept.id, title: kept.title || "", channel: kept.channel || "" })
            return
        }
        const key = keys[row]
        const entries = entriesOf(key)
        if (column < entries.length)
            pick(key, entries[column], column)
    }
    function startOver(kept) {
        // The place is forgotten first, so that the video starts at its start.
        backend.forgetResume(kept.id)
        page.resumed({ id: kept.id, title: kept.title || "", channel: kept.channel || "" })
    }

    Component.onCompleted: rekey()
    Connections {
        target: backend
        function onResumeChanged() { page.rekey() }
        function onLensesChanged() { page.rekey() }
        // A shelf whose read failed is asked for again when it is next wanted.
        function onTaskFailed(task) {
            if (task.indexOf("shelf-") === 0)
                delete page.read[task.slice(6)]
        }
    }

    // h j k l move like the arrows.
    Keys.onPressed: event => {
        navigating = true
        event.accepted = true
        if (event.key === Qt.Key_H)
            move(-1)
        else if (event.key === Qt.Key_L)
            move(1)
        else if (event.key === Qt.Key_K)
            step(-1)
        else if (event.key === Qt.Key_J)
            step(1)
        else
            event.accepted = false
    }
    Keys.onLeftPressed: move(-1)
    Keys.onRightPressed: move(1)
    Keys.onUpPressed: step(-1)
    Keys.onDownPressed: step(1)
    Keys.onReturnPressed: pickCurrent()
    Keys.onEnterPressed: pickCurrent()

    ListView {
        id: view
        objectName: "shelves"
        anchors.fill: parent
        anchors.leftMargin: Style.gutter
        anchors.rightMargin: Style.gutter
        topMargin: Style.u * 2
        bottomMargin: Style.gutter
        spacing: Style.u * 6
        clip: true
        focus: true
        // The keys are the page's: they move along a shelf and between
        // shelves. The list has no current item of its own, which it would
        // scroll to whenever it is given the keyboard.
        keyNavigationEnabled: false
        currentIndex: -1
        highlightFollowsCurrentItem: false
        boundsBehavior: Flickable.StopAtBounds
        model: page.keys

        ScrollBar.vertical: ScrollBar {
            id: bar
            // Only while the page moves or the bar is reached for: it lies over the pictures' edge.
            contentItem: Rectangle {
                implicitWidth: 4
                color: bar.pressed || bar.hovered ? Style.faint : Style.line
                opacity: bar.active ? 1 : 0
                Behavior on opacity { NumberAnimation { duration: 200 } }
            }
            background: Item {}
        }

        // The lead: what you were in the middle of.
        header: Item {
            width: view.width
            // (By whether there is a lead, not by whether it is showing: a
            // page that is hidden for a while must come back as it was.)
            height: page.first !== null ? lead.height + view.spacing : 0

            Rectangle {
                id: lead
                objectName: "lead"
                readonly property var kept: page.first || ({})
                // The picture comes out of this colour.
                readonly property color panel: Qt.tint(Style.bg, Style.tint(0.05))
                readonly property bool mirrored: Qt.application.layoutDirection === Qt.RightToLeft
                width: parent.width
                height: page.leadHeight
                visible: page.first !== null
                // It comes in, when there turns out to be one.
                opacity: page.first !== null ? 1 : 0
                Behavior on opacity { NumberAnimation { duration: Style.move } }
                color: panel
                clip: true

                MouseArea {
                    id: leadArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: page.resumed({ id: lead.kept.id, title: lead.kept.title || "", channel: lead.kept.channel || "" })
                    onPositionChanged: mouse => page.pointed(mapToItem(null, mouse.x, mouse.y))
                }
                Item {
                    id: picture
                    objectName: "lead-picture"
                    // A narrow column has no room for it beside the words.
                    visible: view.width >= 700
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: Math.round(height * 16 / 9)
                    clip: true
                    Frost {
                        anchors.fill: parent
                        source: page.first !== null ? Format.thumb(backend.thumbBase, lead.kept.id, 0) : ""
                        clear: leadArea.containsMouse
                        mode: page.thumbs
                    }
                    Rectangle {
                        anchors.left: parent.left
                        anchors.top: parent.top
                        anchors.bottom: parent.bottom
                        width: 96
                        gradient: Gradient {
                            orientation: Gradient.Horizontal
                            GradientStop { position: lead.mirrored ? 1 : 0; color: lead.panel }
                            GradientStop { position: lead.mirrored ? 0 : 1; color: Qt.rgba(lead.panel.r, lead.panel.g, lead.panel.b, 0) }
                        }
                    }
                    // How far it was watched.
                    Rectangle {
                        anchors.left: parent.left
                        anchors.bottom: parent.bottom
                        width: parent.width * Math.min(1, (lead.kept.pos || 0) / Math.max(1, lead.kept.dur || 1))
                        height: 3
                        color: Style.accent
                    }
                }
                Column {
                    id: words
                    anchors.left: parent.left
                    anchors.leftMargin: page.compact ? Style.u * 5 : Style.u * 8
                    // (In a strip, the actions stand between the words and the picture.)
                    anchors.right: picture.visible ? picture.left : parent.right
                    anchors.rightMargin: page.compact ? acts.width + Style.u * 12 + (picture.visible ? 0 : -Style.u * 5) : picture.visible ? -Style.u * 8 : Style.u * 5
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: page.compact ? Style.u : Style.u * 3

                    Text {
                        // (A strip has no room for it; the title and what is left say it.)
                        visible: !page.compact
                        text: qsTr("Continue watching")
                        textFormat: Text.PlainText
                        color: Style.accent
                        font.family: Style.font
                        font.pixelSize: Style.caption
                        font.capitalization: Font.AllUppercase
                        font.letterSpacing: 1.6
                        font.weight: Style.strong
                    }
                    Text {
                        objectName: "lead-title"
                        width: parent.width
                        text: lead.kept.title || ""
                        textFormat: Text.PlainText
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        lineHeight: 1.05
                        color: Style.fg
                        font.family: Style.font
                        font.pixelSize: page.compact ? Style.title : lead.height < 240 ? Style.head + 2 : Style.lead
                        font.weight: Style.strong
                    }
                    Text {
                        objectName: "lead-meta"
                        width: parent.width
                        text: [page.compact ? qsTr("Continue watching") : "", lead.kept.channel || "",
                               qsTr("%1 left").arg(Format.clock(Math.max(0, (lead.kept.dur || 0) - (lead.kept.pos || 0))))]
                                  .filter(s => s !== "").join("  ·  ")
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        color: Style.dim
                        font.family: Style.font
                        font.pixelSize: Style.body
                    }
                    // Where the actions are, under the words (a strip has them beside).
                    Item {
                        visible: !page.compact
                        width: 1
                        height: acts.height
                    }
                }
                Row {
                    id: acts
                    // Under the words; in a strip, in the room between them and the picture.
                    // (Placed by hand, so mirrored by hand for right-to-left.)
                    x: lead.mirrored
                       ? (page.compact ? (picture.visible ? picture.x + picture.width : 0) + Style.u * 6 : words.x + words.width - width)
                       : (page.compact ? (picture.visible ? picture.x : lead.width) - width - Style.u * 6 : words.x)
                    y: page.compact ? Math.round((lead.height - height) / 2) : words.y + words.height - height
                    spacing: page.compact ? Style.u : Style.u * 2
                    Act {
                        objectName: "lead-resume"
                        implicitHeight: page.compact ? Style.control + Style.u : 36
                        text: qsTr("Resume")
                        primary: !page.compact
                        quiet: page.compact
                        marked: page.leadSelected && page.leadAction === 0
                        onClicked: page.resumed({ id: lead.kept.id, title: lead.kept.title || "", channel: lead.kept.channel || "" })
                    }
                    Act {
                        objectName: "lead-over"
                        implicitHeight: page.compact ? Style.control + Style.u : 36
                        text: qsTr("Start over")
                        filled: !page.compact
                        quiet: page.compact
                        marked: page.leadSelected && page.leadAction === 1
                        onClicked: page.startOver(lead.kept)
                    }
                    Act {
                        objectName: "lead-forget"
                        implicitHeight: page.compact ? Style.control + Style.u : 36
                        text: qsTr("Forget")
                        quiet: true
                        marked: page.leadSelected && page.leadAction === 2
                        tip: qsTr("Forget where you stopped in this video")
                        onClicked: backend.forgetResume(lead.kept.id)
                    }
                    Text {
                        objectName: "lead-kept"
                        visible: !page.compact
                        anchors.verticalCenter: parent.verticalCenter
                        leftPadding: Style.u * 2
                        text: qsTr("kept for %n days", "", page.days)
                        textFormat: Text.PlainText
                        color: Style.faint
                        font.family: Style.font
                        font.pixelSize: Style.small
                    }
                }
            }
        }

        // After the shelves, the way to one more of them.
        footer: Item {
            width: view.width
            height: more.height + view.spacing
            Row {
                id: more
                y: view.spacing
                spacing: Style.u * 3
                Act {
                    objectName: "home-new-lens"
                    text: qsTr("New lens")
                    glyph: "qrc:/icons/plus.svg"
                    filled: true
                    enabled: backend.lenses.length < 12
                    onClicked: page.newLensRequested()
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: qsTr("A lens is a feed you describe in words. Each one is a shelf here.")
                    textFormat: Text.PlainText
                    color: Style.faint
                    font.family: Style.font
                    font.pixelSize: Style.small
                }
            }
        }

        delegate: Shelf {
            id: shelf
            required property string modelData
            required property int index
            readonly property bool isTopic: modelData.indexOf("topic-") === 0
            readonly property var topic: isTopic ? page.topicOf(modelData) : ({})
            readonly property bool isLens: !isTopic && modelData !== "resume" && modelData !== "latest"
            readonly property var lens: isLens ? page.lensOf(modelData) : ({})
            // A lens's shelf is read from what the engine keeps for it, when
            // the shelf comes into being and again when the lens was fetched anew.
            readonly property var fetchedAt: lens.fetchedAt
            onFetchedAtChanged: page.wantShelf(lens)
            Component.onCompleted: page.wantShelf(lens)

            width: view.width
            height: implicitHeight
            key: modelData
            title: modelData === "resume" ? qsTr("Also unfinished")
                 : modelData === "latest" ? qsTr("Latest from your channels")
                 : isTopic ? (topic.label || "").charAt(0).toUpperCase() + (topic.label || "").slice(1)
                 : (lens.text || "")
            // A topic says which of your channels it comes from.
            readonly property var from: topic.channels || []
            note: modelData === "latest" ? page.status : isLens ? qsTr("Lens")
                : isTopic ? (from.length > 1 ? qsTr("Topic  ·  from %1 and %n more", "", from.length - 1).arg(from[0])
                                             : qsTr("Topic  ·  from %1").arg(from[0] || "")) : ""
            all: modelData !== "resume" && !isTopic
            arrange: page.arrangements[modelData] || ({})
            sourceLabel: modelData === "latest" ? qsTr("Newest") : modelData === "resume" ? qsTr("Last watched") : qsTr("Best match")
            onArranged: setting => page.arranged(modelData, setting)
            rows: modelData === "latest" ? page.latestRows : 1
            // What is still coming has its places shown meanwhile.
            loading: modelData === "latest" ? page.loading
                   : isTopic ? !topic.fetchedAt && page.searching
                   : isLens ? !!lens.fetchedAt && backend.shelves[modelData] === undefined : false
            entries: page.entriesOf(modelData)
            empty: modelData === "latest"
                   ? (page.loading ? "" : page.noChannels
                      ? qsTr("Add a channel, or drop a Google Takeout subscriptions.csv anywhere on this window.")
                      : qsTr("Nothing here yet. Press R to refresh."))
                   : isTopic ? qsTr("Nothing found for this topic.")
                   : isLens && !lens.fetchedAt ? qsTr("Nothing fetched for this lens yet. Open it to run its searches.")
                   : isLens ? qsTr("Nothing fits this lens right now.") : ""
            thumbs: page.thumbs
            progress: page.progress
            estimateNote: isLens ? qsTr("Approximate: YouTube's search only says how long ago")
                                 : qsTr("Approximate: YouTube's feed for this channel was unavailable")
            current: page.navigating && page.activeFocus && !page.onLead && page.row === index
            column: page.columns[modelData] || 0
            onActivated: (video, at) => page.pick(modelData, video, at)
            onChannelRequested: id => page.channelRequested(id)
            onAllRequested: page.allRequested(modelData)
            onPointed: at => page.pointed(at)

            Act {
                objectName: "topic-keep"
                visible: shelf.isTopic
                text: qsTr("Keep as lens")
                quiet: true
                tip: qsTr("Make a lens of this topic's searches")
                onClicked: backend.addLens((shelf.topic.searches || []).join(", "))
            }
            Act {
                objectName: "topic-hide"
                visible: shelf.isTopic
                text: qsTr("Hide")
                quiet: true
                tip: qsTr("Hide this topic")
                onClicked: backend.hideTopic(shelf.topic.id)
            }
            Act {
                visible: shelf.modelData === "latest" && page.noChannels
                text: qsTr("Add a channel")
                filled: true
                onClicked: page.addChannelRequested()
            }
            Act {
                visible: shelf.modelData === "latest" && !page.noChannels
                text: qsTr("Refresh")
                glyph: "qrc:/icons/refresh.svg"
                quiet: true
                enabled: !page.loading
                onClicked: page.refreshRequested()
            }
        }
    }
}
