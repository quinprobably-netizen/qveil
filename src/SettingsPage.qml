import QtQuick
import Qveil

// Everything that can be set: categories down the side, and the rows of the
// one that is showing beside them. A setting applies as soon as it is
// chosen, and what it means for privacy is said in the engine's words.
FocusScope {
    id: page
    objectName: "settings"

    // How thumbnails are shown ("frosted" or "clear"), and whether Shorts
    // are in the feed: the window's own settings, which it keeps.
    property string thumbs: "clear"
    property bool shorts: false
    readonly property var config: backend.config
    readonly property var pages: backend.privacy.pages
    // Which category is showing.
    property string category: "privacy"
    readonly property var categories: [
        { key: "privacy", label: qsTr("Privacy") },
        { key: "playback", label: qsTr("Playback") },
        { key: "home", label: qsTr("Home") },
        { key: "about", label: qsTr("About") },
    ]
    // Where focus belongs when the page is shown.
    readonly property Item content: cats
    signal thumbsPicked(string mode)
    signal shortsPicked(bool shown)

    function step(by) {
        let at = 0
        for (let i = 0; i < categories.length; i++)
            if (categories[i].key === category)
                at = i
        category = categories[Math.max(0, Math.min(categories.length - 1, at + by))].key
    }

    onVisibleChanged: if (visible) chooser.refresh()

    component Choice: TabWord {
        // What picking it sends to the engine.
        property var patch: ({})
        onClicked: backend.setConfig(patch)
    }

    // The categories.
    Column {
        id: cats
        x: Style.gutter
        y: Style.u * 7
        width: 200
        spacing: 2
        focus: true
        Keys.onUpPressed: page.step(-1)
        Keys.onDownPressed: page.step(1)
        Keys.onPressed: event => {
            event.accepted = event.key === Qt.Key_J || event.key === Qt.Key_K
            if (event.accepted)
                page.step(event.key === Qt.Key_J ? 1 : -1)
        }

        Text {
            leftPadding: Style.u * 3
            bottomPadding: Style.u * 3
            text: qsTr("Settings")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.head
            font.weight: Style.strong
        }
        Repeater {
            model: page.categories
            delegate: Rectangle {
                id: cat
                required property var modelData
                readonly property bool current: page.category === modelData.key
                objectName: "settings-cat-" + modelData.key
                width: cats.width
                height: 36
                radius: Style.radius
                color: current ? Style.sel : catArea.containsMouse ? Style.hover : "transparent"
                Behavior on color { ColorAnimation { duration: Style.quick } }
                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: Style.u * 3
                    anchors.verticalCenter: parent.verticalCenter
                    text: cat.modelData.label
                    textFormat: Text.PlainText
                    color: cat.current || catArea.containsMouse ? Style.fg : Style.dim
                    font.family: Style.font
                    font.pixelSize: Style.body
                    font.weight: cat.current ? Style.strong : Font.Normal
                }
                MouseArea {
                    id: catArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: page.category = cat.modelData.key
                }
            }
        }
    }

    // The rows of the category that is showing.
    Flickable {
        id: pane
        anchors.left: cats.right
        anchors.leftMargin: Style.u * 8
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        width: Math.min(760, page.width - cats.width - cats.x - Style.u * 8 - Style.gutter)
        // The first row is level with the first category.
        topMargin: cats.y + 46
        bottomMargin: Style.gutter
        contentHeight: rows.height
        clip: true
        boundsBehavior: Flickable.StopAtBounds

        Item {
            id: rows
            width: pane.width
            height: Math.max(privacy.visible ? privacy.height : 0, playback.visible ? playback.height : 0,
                             home.visible ? home.height : 0, about.visible ? about.height : 0)

            Column {
                id: privacy
                width: parent.width
                spacing: Style.u * 2
                visible: page.category === "privacy"

                Rectangle {
                    objectName: "set-route"
                    width: parent.width
                    height: chooser.implicitHeight + Style.u * 7
                    color: Style.rest
                    radius: Style.radius
                    RouteChooser {
                        id: chooser
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.margins: Style.u * 4
                        anchors.verticalCenter: parent.verticalCenter
                        height: implicitHeight
                        beside: true
                    }
                }
                SettingRow {
                    objectName: "set-history"
                    width: parent.width
                    name: qsTr("History")
                    note: (page.config.history ? page.pages.history_on : page.pages.history_off) || ""
                    noteName: "set-history-note"
                    Choices {
                        Choice { objectName: "history-off"; text: qsTr("Off"); current: !page.config.history; patch: ({ history: false }) }
                        Choice { objectName: "history-on"; text: qsTr("On"); current: !!page.config.history; patch: ({ history: true }) }
                    }
                }
                SettingRow {
                    id: resume
                    objectName: "set-resume"
                    readonly property int days: page.config.resumeDays === undefined ? 7 : page.config.resumeDays
                    width: parent.width
                    name: qsTr("Resume")
                    note: page.pages.resume || ""
                    noteName: "set-resume-note"
                    Act {
                        objectName: "forget-positions"
                        anchors.verticalCenter: parent.verticalCenter
                        visible: backend.resume.length > 0
                        text: qsTr("Forget all positions")
                        quiet: true
                        onClicked: backend.clearResume()
                    }
                    Choices {
                        Choice { objectName: "resume-0"; text: qsTr("Off"); current: resume.days === 0; patch: ({ resumeDays: 0 }) }
                        Choice { objectName: "resume-1"; text: qsTr("1 day"); current: resume.days === 1; patch: ({ resumeDays: 1 }) }
                        Choice { objectName: "resume-7"; text: qsTr("7 days"); current: resume.days === 7; patch: ({ resumeDays: 7 }) }
                        Choice { objectName: "resume-30"; text: qsTr("30 days"); current: resume.days === 30; patch: ({ resumeDays: 30 }) }
                        // A number set in the config file is a choice of its own.
                        Choice {
                            visible: [0, 1, 7, 30].indexOf(resume.days) < 0
                            text: qsTr("%n days", "", resume.days)
                            current: true
                            patch: ({ resumeDays: resume.days })
                        }
                    }
                }
            }

            Column {
                id: playback
                width: parent.width
                spacing: Style.u * 2
                visible: page.category === "playback"

                SettingRow {
                    objectName: "set-quality"
                    width: parent.width
                    name: qsTr("Quality")
                    note: qsTr("The most a video is played at. It applies from the next video.")
                    Choices {
                        Repeater {
                            model: [480, 720, 1080, 1440, 2160]
                            delegate: Choice {
                                required property int modelData
                                objectName: "quality-" + modelData
                                text: modelData + "p"
                                current: (page.config.maxHeight || 1080) === modelData
                                patch: ({ maxHeight: modelData })
                            }
                        }
                    }
                }
            }

            Column {
                id: home
                width: parent.width
                spacing: Style.u * 2
                visible: page.category === "home"

                SettingRow {
                    id: pictures
                    objectName: "set-thumbs"
                    readonly property bool off: page.config.thumbnails === false
                    width: parent.width
                    name: qsTr("Thumbnails")
                    note: qsTr("Frosted ones are blurred until you point at them. Off loads none.")
                    Choices {
                        TabWord {
                            objectName: "thumbs-clear"
                            text: qsTr("Clear")
                            current: !pictures.off && page.thumbs === "clear"
                            onClicked: {
                                page.thumbsPicked("clear")
                                if (pictures.off)
                                    backend.setConfig({ thumbnails: true })
                            }
                        }
                        TabWord {
                            objectName: "thumbs-frosted"
                            text: qsTr("Frosted")
                            current: !pictures.off && page.thumbs === "frosted"
                            onClicked: {
                                page.thumbsPicked("frosted")
                                if (pictures.off)
                                    backend.setConfig({ thumbnails: true })
                            }
                        }
                        TabWord {
                            objectName: "thumbs-off"
                            text: qsTr("Off")
                            current: pictures.off
                            onClicked: backend.setConfig({ thumbnails: false })
                        }
                    }
                }
                SettingRow {
                    objectName: "set-topics"
                    width: parent.width
                    name: qsTr("Topic feeds")
                    // What it is now, and (before it is turned on) what
                    // turning it on would send, both in the engine's words.
                    note: (page.config.topicFeeds ? page.pages.topics_on : page.pages.topics_off) || ""
                    noteName: "set-topics-note"
                    would: page.config.topicFeeds ? "" : (page.pages.topics_on || "")
                    wouldName: "set-topics-would"
                    Choices {
                        Choice { objectName: "topics-off"; text: qsTr("Off"); current: !page.config.topicFeeds; patch: ({ topicFeeds: false }) }
                        Choice { objectName: "topics-on"; text: qsTr("On"); current: !!page.config.topicFeeds; patch: ({ topicFeeds: true }) }
                    }
                }
                SettingRow {
                    objectName: "set-shorts"
                    width: parent.width
                    name: qsTr("Shorts")
                    note: qsTr("Whether Shorts appear among the latest from your channels.")
                    Choices {
                        TabWord { objectName: "shorts-hidden"; text: qsTr("Hidden"); current: !page.shorts; onClicked: page.shortsPicked(false) }
                        TabWord { objectName: "shorts-shown"; text: qsTr("In the feed"); current: page.shorts; onClicked: page.shortsPicked(true) }
                    }
                }
            }

            Column {
                id: about
                width: parent.width
                spacing: Style.u * 2
                visible: page.category === "about"

                // What the engine says about what it does and keeps, sentence by sentence.
                Rectangle {
                    objectName: "set-privacy"
                    width: parent.width
                    height: account.height + Style.u * 8
                    color: Style.rest
                    radius: Style.radius
                    Column {
                        id: account
                        anchors.left: parent.left
                        anchors.right: parent.right
                        anchors.margins: Style.u * 4
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: Style.u * 3
                        Text {
                            text: qsTr("What qveil does and keeps")
                            textFormat: Text.PlainText
                            color: Style.fg
                            font.family: Style.font
                            font.pixelSize: Style.card
                            font.weight: Style.strong
                        }
                        Repeater {
                            // Only while the category is showing.
                            model: about.visible ? backend.privacy.points : []
                            delegate: Notice {
                                required property string modelData
                                objectName: "privacy-point"
                                width: account.width
                                text: modelData
                            }
                        }
                    }
                }
                SettingRow {
                    width: parent.width
                    name: qsTr("Settings file")
                    note: page.config.configPath || ""
                }
            }
        }
    }
}
