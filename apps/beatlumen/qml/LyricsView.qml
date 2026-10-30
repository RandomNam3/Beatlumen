import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The current song's text over the visual. Synced text (LRC) follows the
// playback position, highlights the current line (and word, with word
// timing) and seeks when a line is tapped; plain text scrolls freely.
Item {
    id: lv
    property bool controlsVisible: true
    signal addLyrics()
    // Timing by hand (tap: tap along line by line).
    signal editTiming(bool tap)

    property var info: ({ status: "none" })
    readonly property string curId: Core.state.current ? Core.state.current.id : ""
    readonly property int rev: Core.state.lyricsRev || 0
    readonly property bool ready: info.status === "ready"
    readonly property bool synced: ready && info.synced === true
    readonly property var lines: ready ? info.lines : []
    readonly property bool compact: width < 600
    // The server can time the text by speech recognition.
    readonly property bool canSync: info.canSync === true
    // Or this device, with its own speech model (Settings).
    readonly property var local: Core.state.speechLocal || ({})
    readonly property var localRun: local.running && local.running.id === curId ? local.running : null
    readonly property bool wordTimed: ready && info.wordTimed === true
    readonly property bool canLocal: !canSync && local.ready === true && !wordTimed && !local.running
    readonly property var sync: info.sync || (localRun ? { state: "running", progress: localRun.progress, local: true } : null)
    function startSync() { Core.call(canSync ? "lyrics.sync" : "speech.run") }
    // Translation into the app's language, under each line (on the device).
    readonly property var translation: info.translation || ({})
    readonly property bool canTranslate: ready && translation.available !== false && translation.on !== undefined
    // Small lead: a line lights up as it starts being sung.
    readonly property real lead: 0.15
    property int currentLine: -1
    // Following the song pauses while the user scrolls.
    property bool follow: true

    // Voices: their own tint (the first stays white); backing voices (in
    // parentheses) dimmer and slanted.
    function voiceColor(v) {
        return v === 2 ? "#a8e8ff" : v === 3 ? "#ffd59a" : v >= 4 ? "#d6b8ff" : "white"
    }
    // A singer's name before the line: the voice's tint, dimmed.
    function nameColor(v) {
        return v === 2 ? "#99a8e8ff" : v === 3 ? "#99ffd59a" : v >= 4 ? "#99d6b8ff" : "#99ffffff"
    }
    function esc(s) {
        return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    }
    // A line as styled text (everything from the file is escaped).
    function styled(line) {
        const parts = line.parts || [{ s: line.text, b: false }]
        let out = parts.map(p => p.b ? "<i><font color=\"#b3ffffff\">" + esc(p.s) + "</font></i>" : esc(p.s)).join("")
        if (line.who)
            out = "<font color=\"" + nameColor(line.v || 1) + "\" size=\"2\">" + esc(line.who) + " · </font>" + out
        return out
    }

    function load() {
        if (visible)
            info = Core.call("lyrics.get") || { status: "none" }
    }
    onCurIdChanged: { currentLine = -1; follow = true; load() }
    onRevChanged: load()
    onVisibleChanged: load()
    Component.onCompleted: load()

    function lineAt(t) {
        let lo = 0, hi = lines.length - 1, found = -1
        while (lo <= hi) {
            const mid = (lo + hi) >> 1
            if (lines[mid].t <= t) { found = mid; lo = mid + 1 } else hi = mid - 1
        }
        return found
    }
    Timer {
        interval: 100
        repeat: true
        running: lv.visible && lv.synced
        onTriggered: {
            const i = lv.lineAt(Core.position + lv.lead)
            if (i !== lv.currentLine)
                lv.currentLine = i
            if (lv.follow && i >= 0)
                list.currentIndex = i
        }
    }
    Timer {
        id: resumeFollow
        interval: 4000
        onTriggered: lv.follow = true
    }

    ListView {
        id: list
        anchors.fill: parent
        visible: lv.ready
        clip: true
        model: lv.lines
        spacing: lv.compact ? 10 : 14
        highlightRangeMode: lv.synced && lv.follow ? ListView.ApplyRange : ListView.NoHighlightRange
        preferredHighlightBegin: height * 0.3
        preferredHighlightEnd: height * 0.45
        highlightMoveDuration: 450
        highlightFollowsCurrentItem: lv.synced
        boundsBehavior: Flickable.StopAtBounds
        onMovementStarted: { lv.follow = false; resumeFollow.stop() }
        onMovementEnded: resumeFollow.restart()
        header: Item { height: lv.synced ? list.height * 0.3 : 8 }
        footer: Item { height: lv.synced ? list.height * 0.5 : 8 }

        delegate: Item {
            id: row
            required property var modelData
            required property int index
            readonly property bool isCurrent: lv.synced && index === lv.currentLine
            readonly property bool past: lv.synced && index < lv.currentLine
            width: ListView.view.width
            height: Math.max(lineText.implicitHeight, words.implicitHeight)
                + (trText.visible ? trText.implicitHeight + 2 : 0)
            opacity: !lv.synced ? 0.95 : isCurrent ? 1 : past ? 0.4 : 0.6
            Behavior on opacity { NumberAnimation { duration: 250 } }

            Label {
                id: lineText
                visible: !(row.isCurrent && words.active)
                width: parent.width
                textFormat: Text.StyledText
                text: row.modelData.text === "" && lv.synced ? "♪" : lv.styled(row.modelData)
                wrapMode: Text.WordWrap
                color: lv.voiceColor(row.modelData.v || 0)
                font.pixelSize: lv.synced ? (lv.compact ? 22 : 30) : (lv.compact ? 17 : 21)
                font.weight: row.isCurrent ? Font.Bold : Font.DemiBold
                style: Text.Raised
                styleColor: Qt.rgba(0, 0, 0, 0.4)
            }
            // Word timing: each word (or piece of a long note) fills with the
            // accent colour from left to right while it is sung, so how long
            // a sound is held shows.
            Flow {
                id: words
                readonly property bool active: row.isCurrent && (row.modelData.words || []).length > 0
                visible: active
                width: parent.width
                Label {
                    visible: !!row.modelData.who
                    textFormat: Text.PlainText
                    text: (row.modelData.who || "") + " · "
                    color: lv.nameColor(row.modelData.v || 1)
                    font.pixelSize: lineText.font.pixelSize * 0.6
                    style: Text.Raised
                    styleColor: Qt.rgba(0, 0, 0, 0.4)
                }
                Repeater {
                    model: words.active ? row.modelData.words : []
                    Item {
                        id: piece
                        required property var modelData
                        readonly property real fill: {
                            const p = Core.position + lv.lead - modelData.t
                            const d = Math.max(0.05, (modelData.e || modelData.t) - modelData.t)
                            return Math.max(0, Math.min(1, p / d))
                        }
                        implicitWidth: base.implicitWidth
                        implicitHeight: base.implicitHeight
                        Label {
                            id: base
                            textFormat: Text.PlainText
                            text: piece.modelData.w
                            color: lv.voiceColor(row.modelData.v || 0)
                            opacity: piece.modelData.b ? 0.75 : 1
                            font.italic: piece.modelData.b === true
                            font.pixelSize: lineText.font.pixelSize
                            font.weight: lineText.font.weight
                            style: Text.Raised
                            styleColor: Qt.rgba(0, 0, 0, 0.4)
                        }
                        Item {
                            clip: true
                            width: base.implicitWidth * piece.fill
                            height: base.implicitHeight
                            visible: piece.fill > 0
                            Label {
                                textFormat: Text.PlainText
                                text: piece.modelData.w
                                color: Theme.accent
                                font.italic: piece.modelData.b === true
                                font.pixelSize: lineText.font.pixelSize
                                font.weight: lineText.font.weight
                                style: Text.Raised
                                styleColor: Qt.rgba(0, 0, 0, 0.4)
                            }
                        }
                    }
                }
            }
            // The translation, smaller and dimmer under the line.
            Label {
                id: trText
                visible: !!row.modelData.tr
                y: Math.max(lineText.implicitHeight, words.implicitHeight) + 2
                width: parent.width
                textFormat: Text.PlainText
                text: row.modelData.tr || ""
                wrapMode: Text.WordWrap
                color: Qt.rgba(1, 1, 1, 0.7)
                font.pixelSize: lineText.font.pixelSize * 0.62
                font.italic: true
                style: Text.Raised
                styleColor: Qt.rgba(0, 0, 0, 0.4)
            }
            // Takes the tap, so it does not also toggle the controls.
            MouseArea {
                anchors.fill: parent
                enabled: lv.synced && row.modelData.t !== undefined
                onClicked: {
                    Core.call("seek", { position: row.modelData.t })
                    lv.follow = true
                }
            }
        }
    }

    ColumnLayout {
        anchors.centerIn: parent
        width: Math.min(parent.width, 420)
        visible: lv.info.status === "none" && lv.curId !== "" && lv.controlsVisible && !lv.sync
        spacing: 12
        Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: Theme.tr("No song text for this song")
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            color: Qt.rgba(1, 1, 1, 0.8)
            font.pixelSize: 17
            style: Text.Raised
            styleColor: Qt.rgba(0, 0, 0, 0.4)
        }
        Button {
            Layout.alignment: Qt.AlignHCenter
            text: Theme.tr("Add song text…")
            icon.source: Theme.icon("add")
            onClicked: lv.addLyrics()
        }
        Button {
            Layout.alignment: Qt.AlignHCenter
            text: Theme.tr("Paste text and tap along…")
            icon.source: Theme.icon("edit")
            onClicked: lv.editTiming(true)
        }
        Button {
            Layout.alignment: Qt.AlignHCenter
            visible: lv.canSync || lv.canLocal
            text: Theme.tr("Recognize song text")
            icon.source: Theme.icon("auto_awesome")
            onClicked: lv.startSync()
        }
    }

    // Plain text: offer timing it; while the server works, its progress.
    Pane {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 8
        // Plain text: the main action is timing it; word timing is in the menu.
        visible: (lv.sync !== null || (lv.ready && !lv.synced && (lv.canSync || lv.canLocal))) && (lv.controlsVisible || lv.sync !== null)
        padding: lv.sync ? 12 : 0
        Material.elevation: 2
        background: Rectangle {
            radius: height / 2
            color: lv.sync ? Qt.rgba(0, 0, 0, 0.55) : "transparent"
        }
        RowLayout {
            spacing: 10
            Button {
                visible: !lv.sync
                text: Theme.tr("Sync automatically")
                icon.source: Theme.icon("auto_awesome")
                onClicked: lv.startSync()
            }
            BusyIndicator {
                visible: lv.sync !== null
                running: visible
                Layout.preferredWidth: 28
                Layout.preferredHeight: 28
                Material.accent: "white"
            }
            Label {
                visible: lv.sync !== null
                textFormat: Text.PlainText
                color: "white"
                text: !lv.sync ? ""
                    : lv.sync.local
                        ? Theme.tr("Timing the song text on this device…") + " " + Math.round(lv.sync.progress * 100) + " %"
                    : lv.sync.state === "running"
                        ? Theme.tr("Timing the song text…") + " " + Math.round(lv.sync.progress * 100) + " %"
                        : Theme.tr("Waiting for the server…")
            }
        }
    }

    // Where the text comes from and where it is kept.
    Rectangle {
        id: originPill
        x: 8
        y: 8
        width: Math.min(originLabel.implicitWidth + 20, parent.width - 176)
        height: originLabel.implicitHeight + 8
        radius: height / 2
        color: Qt.rgba(0, 0, 0, 0.5)
        visible: lv.ready && lv.controlsVisible && (lv.info.origin || "") !== "" && !lv.sync
        Label {
            id: originLabel
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 10
            verticalAlignment: Text.AlignVCenter
            textFormat: Text.PlainText
            text: lv.info.origin || ""
            elide: Text.ElideRight
            color: Qt.rgba(1, 1, 1, 0.8)
            font.pixelSize: 12
        }
    }

    // Translating: the model download, the progress or why not.
    Rectangle {
        x: 8
        y: 8 + (originPill.visible ? originPill.height + 6 : 0)
        width: Math.min(trState.implicitWidth + 20, parent.width - 176)
        height: trState.implicitHeight + 8
        radius: height / 2
        color: Qt.rgba(0, 0, 0, 0.5)
        visible: lv.ready && lv.translation.on === true && !!lv.translation.state
            && (lv.controlsVisible || lv.translation.state !== "failed")
        Label {
            id: trState
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 10
            verticalAlignment: Text.AlignVCenter
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: Qt.rgba(1, 1, 1, 0.8)
            font.pixelSize: 12
            readonly property int pct: Math.round((lv.translation.progress || 0) * 100)
            text: lv.translation.state === "downloading"
                ? Theme.tr("Loading the translation model…") + " " + pct + " %"
                : lv.translation.state === "running"
                    ? Theme.tr("Translating…") + " " + pct + " %"
                    : lv.translation.state === "failed"
                        ? Theme.tr("No translation:") + " " + (lv.translation.error || "")
                        : ""
        }
    }

    // Song text actions: translating (often switched) and the rest in a menu.
    Row {
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        visible: lv.controlsVisible && lv.curId !== "" && !lv.sync
        IconButton {
            visible: lv.canTranslate
            iconName: "translate"
            iconColor: lv.translation.on === true ? Theme.accent : "white"
            tip: lv.translation.on === true ? Theme.tr("Hide translation") : Theme.tr("Translate song text")
            onClicked: Core.call("translate.set", { on: lv.translation.on !== true })
        }
        IconButton {
            id: menuButton
            visible: lv.ready
            iconName: "more_vert"
            iconColor: "white"
            tip: Theme.tr("Song text")
            // Opens above the button, right-aligned with it.
            onClicked: textMenu.popup(menuButton, menuButton.width - textMenu.width, -textMenu.implicitHeight)
        }
    }
    Menu {
        id: textMenu
        width: 320
        MenuItem {
            visible: lv.synced && !lv.wordTimed && (lv.canSync || lv.canLocal)
            height: visible ? implicitHeight : 0
            text: Theme.tr("Time word by word")
            icon.source: Theme.icon("auto_awesome")
            onTriggered: lv.startSync()
        }
        MenuItem {
            visible: !lv.synced && (lv.canSync || lv.canLocal)
            height: visible ? implicitHeight : 0
            text: Theme.tr("Sync automatically")
            icon.source: Theme.icon("auto_awesome")
            onTriggered: lv.startSync()
        }
        MenuItem {
            text: lv.synced ? Theme.tr("Correct the timing by hand") : Theme.tr("Tap along")
            icon.source: Theme.icon("edit")
            onTriggered: lv.editTiming(!lv.synced)
        }
        MenuItem {
            text: Theme.tr("Add song text…")
            icon.source: Theme.icon("add")
            onTriggered: lv.addLyrics()
        }
        MenuItem {
            visible: lv.info.canRemove === true
            height: visible ? implicitHeight : 0
            text: Theme.tr("Remove added song text")
            icon.source: Theme.icon("delete")
            onTriggered: Core.call("lyrics.remove")
        }
    }
}
