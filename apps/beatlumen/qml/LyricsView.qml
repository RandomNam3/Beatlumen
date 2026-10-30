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
    // Small lead: a line lights up as it starts being sung.
    readonly property real lead: 0.15
    property int currentLine: -1
    // Following the song pauses while the user scrolls.
    property bool follow: true

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
            opacity: !lv.synced ? 0.95 : isCurrent ? 1 : past ? 0.4 : 0.6
            Behavior on opacity { NumberAnimation { duration: 250 } }

            Label {
                id: lineText
                visible: !(row.isCurrent && words.active)
                width: parent.width
                textFormat: Text.PlainText
                text: row.modelData.text === "" && lv.synced ? "♪" : row.modelData.text
                wrapMode: Text.WordWrap
                color: "white"
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
                            color: "white"
                            font: lineText.font
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
                                font: lineText.font
                                style: Text.Raised
                                styleColor: Qt.rgba(0, 0, 0, 0.4)
                            }
                        }
                    }
                }
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
        visible: (lv.sync !== null || (lv.ready && (lv.canSync || lv.canLocal))) && (lv.controlsVisible || lv.sync !== null)
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
                // Line times already: words get theirs.
                text: lv.synced ? Theme.tr("Time word by word") : Theme.tr("Sync automatically")
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
    Label {
        anchors.left: parent.left
        anchors.right: removeButton.visible ? removeButton.left : parent.right
        anchors.bottom: parent.bottom
        anchors.margins: 8
        visible: lv.ready && lv.controlsVisible && (lv.info.origin || "") !== "" && !lv.sync
        textFormat: Text.PlainText
        text: lv.info.origin || ""
        elide: Text.ElideRight
        color: Qt.rgba(1, 1, 1, 0.7)
        font.pixelSize: 12
        style: Text.Raised
        styleColor: Qt.rgba(0, 0, 0, 0.4)
    }

    IconButton {
        id: removeButton
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        visible: lv.ready && lv.info.canRemove === true && lv.controlsVisible
        iconName: "delete"
        iconColor: "white"
        tip: Theme.tr("Remove added song text")
        onClicked: Core.call("lyrics.remove")
    }
}
