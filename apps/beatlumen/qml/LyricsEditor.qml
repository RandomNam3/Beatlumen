import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Timing a song text by hand: tap along line by line while the song plays
// (Space, or the big button; Backspace goes back a line), or move single
// lines in the list (a tenth of a second, or to where the song is now) and
// change their text. Saved as the song's own text (on the server for its
// songs); a line keeps its word times, moved with it, while its text stays.
Rectangle {
    id: ed
    color: Qt.rgba(0, 0, 0, 0.9)
    visible: false

    // [{t, text, from}]: start (seconds, undefined when not yet timed),
    // text, index of the line it was (-1 for new ones).
    property var lines: []
    property int current: 0
    property bool tapping: true
    property bool pasting: false
    property var undoStack: []
    property bool changed: false
    // Taps come a little after the singing starts.
    readonly property real reaction: 0.12
    readonly property bool compact: width < 600

    function open(info, tap) {
        const l = info && info.status === "ready" ? info.lines : []
        lines = l.map((x, i) => ({ t: x.t, text: (x.who ? x.who + ": " : "") + x.text, from: i }))
        pasting = lines.length === 0
        tapping = tap || lines.every(x => x.t === undefined)
        current = 0
        undoStack = []
        changed = false
        pasteArea.text = ""
        visible = true
        Theme.lyricsEditing = true
        focusCatcher.forceActiveFocus()
    }
    function close() {
        visible = false
        Theme.lyricsEditing = false
    }
    function snapshot() {
        undoStack = undoStack.concat([JSON.stringify({ lines: lines, current: current })]).slice(-80)
        changed = true
    }
    function undo() {
        if (undoStack.length === 0)
            return
        const s = JSON.parse(undoStack[undoStack.length - 1])
        undoStack = undoStack.slice(0, -1)
        lines = s.lines
        current = s.current
    }
    function setLine(i, change) {
        snapshot()
        const copy = lines.slice()
        copy[i] = Object.assign({}, copy[i], change)
        lines = copy
    }
    // Tapping along starts at the song's start (from the first line).
    function startTapping() {
        current = 0
        list.positionViewAtBeginning()
        Core.call("seek", { position: 0 })
        if (Core.state.play !== "playing")
            Core.call("toggle")
    }
    function tap() {
        if (current >= lines.length)
            return
        setLine(current, { t: Math.max(0, Core.position - reaction) })
        current++
        list.positionViewAtIndex(current, ListView.Center)
    }
    function back() {
        if (current > 0) {
            snapshot()
            current--
            list.positionViewAtIndex(current, ListView.Center)
        }
    }
    function nudge(i, d) {
        const t = lines[i].t === undefined ? Core.position : lines[i].t
        setLine(i, { t: Math.max(0, t + d) })
    }
    function addLine() {
        snapshot()
        const copy = lines.slice()
        let at = copy.findIndex(x => x.t !== undefined && x.t > Core.position)
        if (at < 0)
            at = copy.length
        copy.splice(at, 0, { t: Core.position, text: "", from: -1 })
        lines = copy
        list.positionViewAtIndex(at, ListView.Center)
    }
    function removeLine(i) {
        snapshot()
        const copy = lines.slice()
        copy.splice(i, 1)
        lines = copy
        current = Math.min(current, lines.length)
    }
    function usePasted() {
        const l = pasteArea.text.split("\n").map(x => x.trim())
        while (l.length && l[l.length - 1] === "")
            l.pop()
        lines = l.map(x => ({ t: undefined, text: x, from: -1 }))
        pasting = false
        tapping = true
        changed = true
        focusCatcher.forceActiveFocus()
        startTapping()
    }
    function save() {
        // Untimed lines in a timed text take the time of the line before.
        const timed = lines.some(x => x.t !== undefined)
        let last = 0
        const out = lines.map(x => {
            const t = timed ? (x.t === undefined ? last : x.t) : undefined
            if (t !== undefined)
                last = t
            return { t: t, text: x.text, from: x.from }
        })
        const r = Core.call("lyrics.edit", { lines: out })
        if (r && r.error) {
            Core.call("toast", { text: r.error })
            return
        }
        close()
    }
    function fmt(t) {
        if (t === undefined)
            return "–:––.–"
        const m = Math.floor(t / 60)
        const s = t - m * 60
        return m + ":" + (s < 10 ? "0" : "") + s.toFixed(1)
    }

    // Takes the keys while the editor is open (and no text field is).
    Item {
        id: focusCatcher
        focus: true
        Keys.onPressed: (e) => {
            if (ed.pasting)
                return
            if (e.key === Qt.Key_Space) {
                if (ed.tapping)
                    ed.tap()
                else
                    Core.call("toggle")
            } else if (e.key === Qt.Key_Backspace && ed.tapping) {
                ed.back()
            } else if (e.key === Qt.Key_Z && (e.modifiers & Qt.ControlModifier)) {
                ed.undo()
            } else if (e.key === Qt.Key_Left) {
                Core.call("seekBy", { delta: -3 })
            } else if (e.key === Qt.Key_Right) {
                Core.call("seekBy", { delta: 3 })
            } else if (e.key === Qt.Key_Escape) {
                ed.close()
            } else {
                return
            }
            e.accepted = true
        }
    }
    // Clicks on the backdrop stay here.
    MouseArea { anchors.fill: parent; onClicked: focusCatcher.forceActiveFocus() }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: ed.compact ? 12 : 24
        spacing: 10

        RowLayout {
            Layout.fillWidth: true
            spacing: 6
            Label {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: Theme.tr("Time the song text")
                color: "white"
                font.pixelSize: 20
                font.weight: Font.DemiBold
                elide: Text.ElideRight
            }
            Button {
                visible: !ed.pasting
                flat: true
                text: ed.tapping ? Theme.tr("List") : Theme.tr("Tap along")
                Material.foreground: "white"
                onClicked: {
                    ed.tapping = !ed.tapping
                    if (ed.tapping)
                        ed.startTapping()
                    focusCatcher.forceActiveFocus()
                }
            }
            IconButton {
                iconName: "undo"
                iconColor: "white"
                tip: Theme.tr("Undo")
                enabled: ed.undoStack.length > 0
                onClicked: { ed.undo(); focusCatcher.forceActiveFocus() }
            }
            IconButton {
                iconName: "close"
                iconColor: "white"
                tip: Theme.tr("Close without saving")
                onClicked: ed.close()
            }
        }

        // No text yet: paste one first.
        ColumnLayout {
            visible: ed.pasting
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8
            Label {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.WordWrap
                color: Qt.rgba(1, 1, 1, 0.8)
                text: Theme.tr("Paste the song text, one sung line per line. Then tap along while the song plays.")
            }
            ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                TextArea {
                    id: pasteArea
                    wrapMode: TextEdit.Wrap
                    color: "white"
                    placeholderText: Theme.tr("Song text")
                    background: Rectangle { color: Qt.rgba(1, 1, 1, 0.08); radius: 8 }
                }
            }
            Button {
                Layout.alignment: Qt.AlignRight
                enabled: pasteArea.text.trim() !== ""
                highlighted: true
                text: Theme.tr("Continue")
                onClicked: ed.usePasted()
            }
        }

        ListView {
            id: list
            visible: !ed.pasting
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: ed.lines
            spacing: 2
            delegate: Rectangle {
                id: row
                required property var modelData
                required property int index
                readonly property bool isCurrent: ed.tapping && index === ed.current
                width: ListView.view.width
                height: ed.tapping ? lineLabel.implicitHeight + 14 : Math.max(Theme.row, rowLayout.implicitHeight + 8)
                radius: 6
                color: isCurrent ? Qt.rgba(1, 1, 1, 0.16) : "transparent"
                RowLayout {
                    id: rowLayout
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 4
                    spacing: 4
                    // The time: tapped, it plays from there.
                    Label {
                        textFormat: Text.PlainText
                        text: ed.fmt(row.modelData.t)
                        color: row.modelData.t === undefined ? Qt.rgba(1, 1, 1, 0.4) : Theme.accent
                        font.features: { "tnum": 1 }
                        Layout.preferredWidth: 64
                        MouseArea {
                            anchors.fill: parent
                            enabled: row.modelData.t !== undefined
                            onClicked: Core.call("seek", { position: row.modelData.t })
                        }
                    }
                    Label {
                        id: lineLabel
                        visible: ed.tapping
                        Layout.fillWidth: true
                        textFormat: Text.PlainText
                        text: row.modelData.text === "" ? "♪" : row.modelData.text
                        wrapMode: Text.WordWrap
                        color: row.index < ed.current ? Qt.rgba(1, 1, 1, 0.55) : "white"
                        font.pixelSize: row.isCurrent ? 20 : 16
                        font.weight: row.isCurrent ? Font.DemiBold : Font.Normal
                    }
                    TextField {
                        visible: !ed.tapping
                        Layout.fillWidth: true
                        text: row.modelData.text
                        color: "white"
                        onEditingFinished: if (text !== row.modelData.text) ed.setLine(row.index, { text: text })
                    }
                    Button {
                        visible: !ed.tapping
                        flat: true
                        text: "−0,1"
                        Material.foreground: "white"
                        Layout.preferredWidth: 52
                        onClicked: ed.nudge(row.index, -0.1)
                    }
                    Button {
                        visible: !ed.tapping
                        flat: true
                        text: "+0,1"
                        Material.foreground: "white"
                        Layout.preferredWidth: 52
                        onClicked: ed.nudge(row.index, 0.1)
                    }
                    Button {
                        visible: !ed.tapping
                        flat: true
                        text: Theme.tr("Now")
                        Material.foreground: Theme.accent
                        onClicked: ed.setLine(row.index, { t: Core.position })
                    }
                    IconButton {
                        visible: !ed.tapping
                        iconName: "delete"
                        iconColor: "white"
                        tip: Theme.tr("Remove line")
                        onClicked: ed.removeLine(row.index)
                    }
                }
            }
            footer: Item {
                width: list.width
                height: ed.tapping ? list.height / 2 : 56
                Button {
                    visible: !ed.tapping
                    flat: true
                    text: Theme.tr("Add a line here")
                    icon.source: Theme.icon("add")
                    Material.foreground: "white"
                    onClicked: ed.addLine()
                }
            }
        }

        // Playback and the big tap button.
        RowLayout {
            visible: !ed.pasting
            Layout.fillWidth: true
            spacing: 10
            RoundButton {
                icon.source: Theme.icon(Core.state.play === "playing" ? "pause" : "play_arrow")
                onClicked: { Core.call("toggle"); focusCatcher.forceActiveFocus() }
            }
            Label {
                textFormat: Text.PlainText
                text: ed.fmt(Core.position)
                color: "white"
                font.features: { "tnum": 1 }
            }
            Item { Layout.fillWidth: true }
            Button {
                visible: ed.tapping
                text: Theme.tr("Back")
                flat: true
                Material.foreground: "white"
                enabled: ed.current > 0
                onClicked: { ed.back(); focusCatcher.forceActiveFocus() }
            }
            Button {
                visible: ed.tapping
                highlighted: true
                text: ed.current < ed.lines.length ? Theme.tr("Tap: line starts") : Theme.tr("All lines timed")
                enabled: ed.current < ed.lines.length
                Layout.preferredHeight: 64
                Layout.preferredWidth: ed.compact ? 180 : 260
                onClicked: { ed.tap(); focusCatcher.forceActiveFocus() }
            }
            Button {
                highlighted: !ed.tapping || ed.current >= ed.lines.length
                text: Theme.tr("Save")
                enabled: ed.changed
                onClicked: ed.save()
            }
        }
    }
}
