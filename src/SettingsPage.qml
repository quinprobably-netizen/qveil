import QtQuick
import Qveil

// Everything that can be set, in one place. A setting applies as soon as it
// is chosen, and what it means for privacy is said in the engine's words.
Flickable {
    id: page
    objectName: "settings"

    // How thumbnails are shown ("frosted" or "clear"), and whether Shorts
    // are in the feed: the window's own settings, which it keeps.
    property string thumbs: "frosted"
    property bool shorts: false
    readonly property var config: backend.config
    readonly property var pages: backend.privacy.pages
    // Where focus belongs when the page is shown.
    readonly property Item content: page
    signal thumbsPicked(string mode)
    signal shortsPicked(bool shown)

    contentHeight: column.height + Style.gutter * 2
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    onVisibleChanged: if (visible) chooser.refresh()

    component Choice: TabWord {
        // What picking it sends to the engine.
        property var patch: ({})
        onClicked: backend.setConfig(patch)
    }

    Column {
        id: column
        x: Style.gutter
        y: Style.u * 3
        width: Math.min(page.width - 2 * Style.gutter, 720)
        spacing: Style.u * 3

        Text {
            text: qsTr("Settings")
            textFormat: Text.PlainText
            color: Style.fg
            font.family: Style.font
            font.pixelSize: Style.head
            font.bold: true
        }

        SectionHead { width: parent.width; text: qsTr("Route") }
        RouteChooser {
            id: chooser
            width: parent.width
        }
        Repeater {
            model: backend.privacy.points
            delegate: Notice {
                required property string modelData
                width: column.width
                text: modelData
                color: Style.faint
            }
        }

        SectionHead { width: parent.width; text: qsTr("History") }
        Row {
            spacing: 2
            Choice { objectName: "history-off"; text: qsTr("Off"); current: !page.config.history; patch: ({ history: false }) }
            Choice { objectName: "history-on"; text: qsTr("On"); current: !!page.config.history; patch: ({ history: true }) }
        }
        Notice {
            objectName: "set-history-note"
            width: parent.width
            text: (page.config.history ? page.pages.history_on : page.pages.history_off) || ""
        }

        SectionHead { width: parent.width; text: qsTr("Resume") }
        Row {
            id: resume
            objectName: "set-resume"
            readonly property int days: page.config.resumeDays === undefined ? 7 : page.config.resumeDays
            spacing: 2
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
        Notice {
            objectName: "set-resume-note"
            width: parent.width
            text: page.pages.resume || ""
        }
        Act {
            objectName: "forget-positions"
            visible: backend.resume.length > 0
            text: qsTr("Forget all positions")
            filled: true
            onClicked: backend.clearResume()
        }

        SectionHead { width: parent.width; text: qsTr("Thumbnails") }
        Row {
            readonly property bool off: page.config.thumbnails === false
            spacing: 2
            TabWord {
                objectName: "thumbs-frosted"
                text: qsTr("Frosted")
                current: !parent.off && page.thumbs === "frosted"
                onClicked: {
                    page.thumbsPicked("frosted")
                    if (parent.off)
                        backend.setConfig({ thumbnails: true })
                }
            }
            TabWord {
                objectName: "thumbs-clear"
                text: qsTr("Clear")
                current: !parent.off && page.thumbs === "clear"
                onClicked: {
                    page.thumbsPicked("clear")
                    if (parent.off)
                        backend.setConfig({ thumbnails: true })
                }
            }
            TabWord {
                objectName: "thumbs-off"
                text: qsTr("Off")
                current: parent.off
                onClicked: backend.setConfig({ thumbnails: false })
            }
        }
        Notice {
            width: parent.width
            text: qsTr("Frosted thumbnails are blurred until you point at them. Off loads none.")
        }

        SectionHead { width: parent.width; text: qsTr("Quality") }
        Row {
            spacing: 2
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
        Notice {
            width: parent.width
            text: qsTr("The most a video is played at. It applies from the next video.")
        }

        SectionHead { width: parent.width; text: qsTr("Shorts") }
        Row {
            spacing: 2
            TabWord { objectName: "shorts-hidden"; text: qsTr("Hidden"); current: !page.shorts; onClicked: page.shortsPicked(false) }
            TabWord { objectName: "shorts-shown"; text: qsTr("In the feed"); current: page.shorts; onClicked: page.shortsPicked(true) }
        }

        SectionHead { width: parent.width; text: qsTr("File") }
        Notice {
            width: parent.width
            text: qsTr("Settings are kept in %1").arg(page.config.configPath || "")
            color: Style.faint
        }
    }
}
