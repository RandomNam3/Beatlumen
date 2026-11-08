import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Karaoke: pick a song, give each singer a microphone (and a voice in
// duets), sing along with the text and its note bars while each singer's
// pitch is drawn and scored, then the results. Microphones are only open
// while a round runs; nothing is recorded.
Item {
    id: page
    signal importFile()
    signal exportFile()

    // "pick", "setup", "sing", "results"
    property string stage: "setup"
    readonly property var current: Core.state.current || null
    readonly property string songId: current ? String(current.id) : ""
    property var info: ({})
    readonly property var track: info.status === "ready" ? info.track : null
    readonly property bool duet: info.duet === true

    // ---- karaoke data of the current song ----
    function reload() {
        if (!songId) {
            info = {}
            return
        }
        // The song text is what the notes are computed from.
        Core.call("lyrics.get")
        info = Core.call("karaoke.get") || {}
    }
    readonly property int karaokeRev: Core.state.karaokeRev || 0
    onKaraokeRevChanged: reload()
    onSongIdChanged: { reload(); if (stage === "results" && !live.finished) stage = "setup" }
    // The song text must be loaded for computing; it loads with the song.
    readonly property int lyricsRev: Core.state.lyricsRev || 0
    onLyricsRevChanged: Qt.callLater(reload)
    Timer {
        interval: 500
        repeat: true
        running: page.visible && (page.info.status === "loading" || !!page.info.computing
                 || (page.info.vocals && ["running", "downloading"].indexOf(page.info.vocals.state) >= 0))
        onTriggered: page.reload()
    }

    // ---- singers and microphones ----
    readonly property var colors: ["#4fc3f7", "#ffb74d", "#81c784", "#f06292", "#ba68c8", "#fff176", "#4db6ac", "#e57373"]
    property var devices: []
    property var singers: [{ name: "", device: "", voice: 0 }]
    property string difficulty: "medium"
    // How loud the song's own singing plays during a round (1: as recorded).
    property real vocals: 1
    readonly property var vocalLevels: [1, 0.25, 0]
    function setVocals(level) {
        vocals = level
        if (songId)
            Core.call("karaoke.vocals", { level: level })
        reload()
    }

    // ---- latency calibration of a singer's microphone ----
    property int calibrating: -1
    property var calStream: 0
    property string calSong: ""
    property var calResult: ({})
    function calibrate(i) {
        const s = singers[i]
        if (!s || !s.device)
            return
        closeMics()
        const r = Core.call("mic.open", { device: s.device }) || {}
        if (!r.stream) {
            Core.call("toast", { text: (r.error || Theme.tr("The microphone could not be opened")), error: true })
            return
        }
        opened = [r.stream]
        calSong = songId
        calStream = r.stream
        calibrating = i
        calResult = {}
        Core.call("karaoke.calibrate", { stream: r.stream })
    }
    Timer {
        interval: 200
        repeat: true
        running: page.calibrating >= 0
        onTriggered: {
            const c = Core.call("karaoke.calibration") || {}
            if (c.state === "running")
                return
            const i = page.calibrating
            page.calibrating = -1
            page.calResult = Object.assign({ index: i, state: c.state }, c.result || {})
            page.closeMics()
            // Back to the song.
            if (page.calSong) {
                Core.call("play", { ids: [page.calSong], start: 0 })
                Core.call("pause")
            }
        }
    }
    // Streams this page opened.
    property var opened: []
    function listMics() {
        const r = Core.call("mic.list") || {}
        devices = r.devices || []
        // A new singer gets the next free microphone.
        let changed = false
        const used = singers.map(s => s.device)
        const s = singers.map(x => Object.assign({}, x))
        for (const x of s) {
            if (!x.device || !devices.some(d => d.id === x.device)) {
                const free = devices.find(d => used.indexOf(d.id) < 0) || devices.find(d => d.default) || devices[0]
                if (free && x.device !== free.id) {
                    x.device = free.id
                    used.push(free.id)
                    changed = true
                }
            }
        }
        if (changed)
            singers = s
    }
    Timer { id: micsAgain; interval: 700; onTriggered: page.listMics() }
    function setSinger(i, key, value) {
        const s = singers.map(x => Object.assign({}, x))
        s[i][key] = value
        singers = s
    }
    function addSinger() {
        if (singers.length >= 8)
            return
        singers = singers.concat([{ name: "", device: "", voice: duet ? (singers.length % 2) + 1 : 0 }])
        listMics()
    }
    function removeSinger(i) {
        singers = singers.filter((_, j) => j !== i)
    }
    function closeMics() {
        for (const s of opened)
            Core.call("mic.close", { stream: s })
        opened = []
    }
    onVisibleChanged: {
        if (visible) {
            reload()
            listMics()
            micsAgain.restart()
        } else if (stage !== "sing") {
            closeMics()
        }
    }
    Component.onDestruction: closeMics()

    function start() {
        closeMics()
        const list = []
        const streams = []
        for (let i = 0; i < singers.length; i++) {
            const s = singers[i]
            if (!s.device)
                continue
            const r = Core.call("mic.open", { device: s.device }) || {}
            if (!r.stream) {
                Core.call("toast", { text: (r.error || Theme.tr("The microphone could not be opened")), error: true })
                continue
            }
            if (streams.indexOf(r.stream) < 0)
                streams.push(r.stream)
            list.push({ stream: r.stream, voice: duet ? (s.voice || 1) : 0, name: s.name || (Theme.tr("Singer") + " " + (i + 1)) })
        }
        opened = streams
        const r = Core.call("karaoke.start", { singers: list, difficulty: difficulty }) || {}
        if (r.error) {
            Core.call("toast", { text: r.error, error: true })
            closeMics()
            return
        }
        live.reset()
        Core.call("seek", { position: 0 })
        Core.call("karaoke.vocals", { level: vocals })
        Core.call("play")
        stage = "sing"
    }
    function stop() {
        Core.call("pause")
        results = Core.call("karaoke.stop") || {}
        closeMics()
        Core.call("karaoke.vocals", { level: 1 })
        stage = "results"
    }
    property var results: ({})

    // ---- the round as it runs ----
    QtObject {
        id: live
        property real songMs: 0
        property bool finished: false
        property var singers: []
        // Per singer: flat [time, pitch, hit] triples of the last seconds.
        property var trails: []
        property real since: -1e15
        function reset() {
            songMs = 0; finished = false; singers = []; trails = []; since = -1e15
        }
        function poll() {
            const r = Core.call("karaoke.live", { since: since }) || {}
            if (!r.singers)
                return
            songMs = r.songMs || 0
            const t = trails.length === r.singers.length ? trails : r.singers.map(() => [])
            let newest = since
            for (let i = 0; i < r.singers.length; i++) {
                const add = r.singers[i].trail || []
                let tr = t[i].concat(add)
                // Keep the last 8 seconds.
                let cut = 0
                while (cut < tr.length && tr[cut] < songMs - 8000)
                    cut += 3
                if (cut > 0)
                    tr = tr.slice(cut)
                t[i] = tr
                if (add.length >= 3)
                    newest = Math.max(newest, add[add.length - 3])
            }
            since = newest
            trails = t
            singers = r.singers
            if (r.finished && !finished) {
                finished = true
                page.results = Core.call("karaoke.results") || {}
                page.closeMics()
                Core.call("karaoke.vocals", { level: 1 })
                page.stage = "results"
            }
        }
    }
    Timer {
        interval: 33
        repeat: true
        running: page.visible && page.stage === "sing"
        onTriggered: { live.poll(); lane.requestPaint() }
    }

    // Lines of the track with their times (ms).
    readonly property var lines: {
        if (!track || !track.lines)
            return []
        return track.lines.map(l => {
            const notes = l.notes || []
            return {
                voice: l.v || 0,
                start: notes.length ? notes[0].t * 1000 : 0,
                end: notes.length ? (notes[notes.length - 1].t + notes[notes.length - 1].d) * 1000 : 0,
                notes: notes
            }
        })
    }
    // Index of the line sung at `ms` (or the next one).
    function lineAt(ms, voice) {
        let best = -1
        for (let i = 0; i < lines.length; i++) {
            const l = lines[i]
            if (voice > 0 && l.voice > 0 && l.voice !== voice)
                continue
            if (l.end + 300 < ms)
                continue
            best = i
            break
        }
        return best
    }
    function ratingText(r) {
        return ({ perfect: Theme.tr("Perfect"), great: Theme.tr("Great"), good: Theme.tr("Good"),
                  ok: Theme.tr("Okay"), weak: Theme.tr("Weak") })[r] || ""
    }
    // "E minor" as German writes it: "e-Moll"; "Bb major": "B-Dur".
    function keyName(k) {
        if (!k || Theme.language !== "de")
            return k || ""
        const [tonic, mode] = k.split(" ")
        const de = ({ "B": "H", "Bb": "B", "Eb": "Es", "Ab": "As", "C#": "Cis", "F#": "Fis" })[tonic] || tonic
        return mode === "minor" ? de.toLowerCase() + "-Moll" : de + "-Dur"
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 8

        PageHeader {
            Layout.fillWidth: true
            title: Theme.tr("Karaoke")
            subtitle: page.current ? [page.current.artist, page.current.title].filter(x => x).join(" – ") : ""
            IconButton {
                visible: page.stage !== "sing"
                iconName: "search"
                tip: Theme.tr("Choose a song")
                onClicked: page.stage = page.stage === "pick" ? "setup" : "pick"
            }
            IconButton {
                id: menuButton
                visible: page.stage !== "sing" && !!page.current
                iconName: "more_vert"
                tip: Theme.tr("More")
                onClicked: songMenu.popup(menuButton, 0, menuButton.height)
                Menu {
                    id: songMenu
                    MenuItem {
                        text: Theme.tr("Import UltraStar file…")
                        icon.source: Theme.icon("upload")
                        onTriggered: page.importFile()
                    }
                    MenuItem {
                        text: Theme.tr("Export as UltraStar file…")
                        icon.source: Theme.icon("download")
                        enabled: !!page.track
                        onTriggered: page.exportFile()
                    }
                    MenuItem {
                        text: Theme.tr("Compute again")
                        icon.source: Theme.icon("refresh")
                        enabled: page.info.canCompute === true && !!page.track
                        onTriggered: Core.call("karaoke.compute")
                    }
                    MenuItem {
                        text: Theme.tr("Remove karaoke data")
                        icon.source: Theme.icon("delete")
                        enabled: page.info.canRemove === true
                        onTriggered: Core.call("karaoke.remove")
                    }
                }
            }
        }

        // ---- choosing a song ----
        ColumnLayout {
            visible: page.stage === "pick"
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 6
            InputField {
                id: searchField
                Layout.fillWidth: true
                Layout.leftMargin: 8
                Layout.rightMargin: 8
                placeholderText: Theme.tr("Search for a song")
                onTextChanged: searchTimer.restart()
                Timer {
                    id: searchTimer
                    interval: 250
                    onTriggered: songList.model = searchField.text.trim().length >= 2
                        ? (Core.call("library.search", { query: searchField.text.trim() }) || []).slice(0, 100) : []
                }
            }
            ListView {
                id: songList
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                model: []
                ScrollBar.vertical: ScrollBar { }
                delegate: TrackDelegate {
                    required property var modelData
                    width: ListView.view.width
                    track: modelData
                    showMenu: false
                    onPlay: {
                        Core.call("play", { ids: [modelData.id], start: 0 })
                        Core.call("pause")
                        page.stage = "setup"
                    }
                }
            }
            EmptyState {
                visible: songList.count === 0
                Layout.fillWidth: true
                iconName: "mic"
                title: Theme.tr("Choose a song")
                text: Theme.tr("Songs with a timed song text can be sung. Their notes are computed from the singing in the song.")
            }
        }

        // ---- before a round ----
        Flickable {
            visible: page.stage === "setup"
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentHeight: setupCol.implicitHeight + 16
            clip: true
            ScrollBar.vertical: ScrollBar { }
            ColumnLayout {
                id: setupCol
                width: parent.width
                spacing: 10

                EmptyState {
                    visible: !page.current
                    Layout.fillWidth: true
                    Layout.topMargin: 24
                    iconName: "mic"
                    title: Theme.tr("Karaoke")
                    text: Theme.tr("Choose a song to sing.")
                }
                AccentButton {
                    visible: !page.current
                    Layout.alignment: Qt.AlignHCenter
                    highlighted: true
                    text: Theme.tr("Choose a song")
                    icon.source: Theme.icon("search")
                    onClicked: page.stage = "pick"
                }
                // The song's karaoke data.
                Rectangle {
                    visible: !!page.current
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    implicitHeight: dataCol.implicitHeight + 24
                    radius: Theme.radius
                    color: Theme.surfaceVariant
                    ColumnLayout {
                        id: dataCol
                        anchors.fill: parent
                        anchors.margins: 12
                        spacing: 6
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.text
                            font.weight: Font.DemiBold
                            text: page.info.status === "loading" ? Theme.tr("Loading…")
                                : page.track ? Theme.tr("Ready to sing")
                                : Theme.tr("No karaoke data yet")
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.textDim
                            font.pixelSize: 13
                            text: {
                                if (page.track) {
                                    const where = ({ server: Theme.tr("saved on the server"), offline: Theme.tr("in the offline copy"),
                                                     device: Theme.tr("saved on this device") })[page.info.place] || ""
                                    const what = page.info.source === "computed" ? Theme.tr("Computed from the singing") : Theme.tr("Imported")
                                    const notes = page.lines.reduce((n, l) => n + l.notes.length, 0)
                                    return [what + ", " + where, notes + " " + Theme.tr("notes"),
                                            page.info.key ? Theme.tr("Key") + " " + page.keyName(page.info.key) : "",
                                            page.duet ? Theme.tr("Duet") : ""].filter(x => x).join(" · ")
                                }
                                if (page.info.computing)
                                    return ""
                                if (page.info.lyricsReady === false)
                                    return Theme.tr("Karaoke data needs a song text with times. Time the song text first, or import an UltraStar file.")
                                return page.info.canCompute
                                    ? Theme.tr("The notes can be computed from the singing in the song (the vocals are separated from the music first; the first time downloads the separation model, 36 MB).")
                                    : Theme.tr("Import an UltraStar file for this song.")
                            }
                        }
                        // A computation under way.
                        ColumnLayout {
                            visible: !!page.info.computing
                            Layout.fillWidth: true
                            spacing: 4
                            Label {
                                textFormat: Text.PlainText
                                Layout.fillWidth: true
                                color: Theme.textDim
                                font.pixelSize: 13
                                readonly property var c: page.info.computing || ({})
                                text: (c.state === "downloading" ? Theme.tr("Downloading the separation model…")
                                      : c.state === "queued" ? Theme.tr("Waiting…")
                                      : c.place === "server" ? Theme.tr("Computing on the server…") : Theme.tr("Computing on this device…"))
                                      + (c.progress > 0 ? " " + Math.round(c.progress * 100) + " %" : "")
                            }
                            ProgressBar {
                                Layout.fillWidth: true
                                from: 0; to: 1
                                value: page.info.computing ? page.info.computing.progress || 0 : 0
                                indeterminate: !page.info.computing || !(page.info.computing.progress > 0)
                            }
                        }
                        Label {
                            visible: !!page.info.error
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.danger
                            font.pixelSize: 12
                            text: page.info.error || ""
                        }
                        RowLayout {
                            visible: !page.track && page.info.status !== "loading"
                            spacing: 8
                            AccentButton {
                                visible: page.info.canCompute === true
                                highlighted: true
                                text: Theme.tr("Compute notes")
                                icon.source: Theme.icon("graphic_eq")
                                onClicked: {
                                    const r = Core.call("karaoke.compute") || {}
                                    if (r.error)
                                        Core.call("toast", { text: r.error, error: true })
                                    page.reload()
                                }
                            }
                            Button {
                                flat: true
                                text: Theme.tr("Import UltraStar file…")
                                icon.source: Theme.icon("upload")
                                onClicked: page.importFile()
                            }
                        }
                    }
                }

                // Singers.
                SectionLabel { visible: !!page.track; text: Theme.tr("Singers") }
                Label {
                    visible: !!page.track && page.devices.length === 0
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textDim
                    text: (Core.state.mic && Core.state.mic.available) ? Theme.tr("No microphones found") : Theme.tr("Microphones are not available here")
                }
                Repeater {
                    model: page.track ? page.singers : []
                    RowLayout {
                        id: singerRow
                        required property var modelData
                        required property int index
                        Layout.fillWidth: true
                        Layout.leftMargin: 12
                        Layout.rightMargin: 12
                        spacing: 8
                        Rectangle {
                            width: 14; height: 14; radius: 7
                            color: page.colors[singerRow.index % page.colors.length]
                        }
                        InputField {
                            Layout.preferredWidth: 140
                            Layout.fillWidth: page.width < 600
                            placeholderText: Theme.tr("Singer") + " " + (singerRow.index + 1)
                            text: singerRow.modelData.name
                            onEditingFinished: page.setSinger(singerRow.index, "name", text)
                        }
                        Dropdown {
                            Layout.fillWidth: true
                            Layout.maximumWidth: 320
                            model: page.devices.map(d => d.name)
                            currentIndex: page.devices.findIndex(d => d.id === singerRow.modelData.device)
                            onActivated: (i) => page.setSinger(singerRow.index, "device", page.devices[i].id)
                            Accessible.name: Theme.tr("Microphone")
                        }
                        Dropdown {
                            visible: page.duet
                            model: [Theme.tr("Voice 1"), Theme.tr("Voice 2")]
                            currentIndex: Math.max(0, (singerRow.modelData.voice || 1) - 1)
                            onActivated: (i) => page.setSinger(singerRow.index, "voice", i + 1)
                        }
                        IconButton {
                            iconName: "tune"
                            tip: Theme.tr("Measure the microphone's delay")
                            enabled: page.calibrating < 0 && !!singerRow.modelData.device
                            onClicked: page.calibrate(singerRow.index)
                        }
                        IconButton {
                            visible: page.singers.length > 1
                            iconName: "close"
                            tip: Theme.tr("Remove")
                            onClicked: page.removeSinger(singerRow.index)
                        }
                    }
                }
                Label {
                    visible: !!page.track && (page.calibrating >= 0 || page.calResult.index !== undefined)
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    font.pixelSize: 13
                    color: page.calResult.state === "failed" ? Theme.danger : Theme.textDim
                    text: page.calibrating >= 0
                        ? Theme.tr("Clicks are playing: hold the microphone near the speaker, or clap or sing \"ta\" on each click.")
                        : page.calResult.state === "done"
                            ? Theme.tr("Delay of the microphone: %1 ms (%2 clicks heard)").replace("%1", Math.round(page.calResult.offsetMs)).replace("%2", page.calResult.heard)
                            : Theme.tr("Not enough clicks heard. Turn the sound up or hold the microphone closer to the speaker.")
                }
                RowLayout {
                    visible: !!page.track
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    spacing: 8
                    Button {
                        flat: true
                        enabled: page.singers.length < 8
                        text: Theme.tr("Add singer")
                        icon.source: Theme.icon("add")
                        onClicked: page.addSinger()
                    }
                    Item { Layout.fillWidth: true }
                    Label { textFormat: Text.PlainText; text: Theme.tr("Singing in the song"); color: Theme.textDim }
                    Dropdown {
                        model: [Theme.tr("As recorded"), Theme.tr("Quieter"), Theme.tr("Off")]
                        currentIndex: Math.max(0, page.vocalLevels.indexOf(page.vocals))
                        onActivated: (i) => page.setVocals(page.vocalLevels[i])
                    }
                    Label { textFormat: Text.PlainText; text: Theme.tr("Difficulty"); color: Theme.textDim }
                    Dropdown {
                        model: [Theme.tr("Easy"), Theme.tr("Medium"), Theme.tr("Hard")]
                        currentIndex: ["easy", "medium", "hard"].indexOf(page.difficulty)
                        onActivated: (i) => page.difficulty = ["easy", "medium", "hard"][i]
                    }
                }
                Label {
                    readonly property var v: page.info.vocals || ({})
                    visible: !!page.track && (v.state === "running" || v.state === "downloading")
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    color: Theme.textDim
                    font.pixelSize: 13
                    text: (v.state === "downloading" ? Theme.tr("Downloading the separation model…") : Theme.tr("Separating the singing from the music…"))
                          + (v.progress > 0 ? " " + Math.round(v.progress * 100) + " %" : "")
                }
                Label {
                    visible: !!page.track
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                    text: Theme.tr("Easy: within two semitones, medium: one, hard: half a semitone. The octave does not count; holding a long note on pitch earns a bonus.")
                }
                AccentButton {
                    visible: !!page.track
                    Layout.alignment: Qt.AlignHCenter
                    Layout.topMargin: 8
                    highlighted: true
                    enabled: page.singers.some(s => !!s.device)
                    text: Theme.tr("Start singing")
                    icon.source: Theme.icon("mic")
                    onClicked: page.start()
                }
            }
        }

        // ---- singing ----
        ColumnLayout {
            visible: page.stage === "sing"
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8

            // Points.
            Flow {
                Layout.fillWidth: true
                Layout.leftMargin: 8
                Layout.rightMargin: 8
                spacing: 16
                Repeater {
                    model: live.singers
                    RowLayout {
                        id: scoreItem
                        required property var modelData
                        required property int index
                        spacing: 6
                        Rectangle { width: 12; height: 12; radius: 6; color: page.colors[scoreItem.index % page.colors.length] }
                        Label { textFormat: Text.PlainText; text: scoreItem.modelData.name; color: Theme.text }
                        Label {
                            textFormat: Text.PlainText
                            text: Math.round(scoreItem.modelData.points || 0)
                            color: Theme.text
                            font.pixelSize: 20
                            font.weight: Font.Bold
                            font.features: { "tnum": 1 }
                        }
                        Label {
                            textFormat: Text.PlainText
                            visible: !!scoreItem.modelData.lastLine
                            text: scoreItem.modelData.lastLine ? page.ratingText(scoreItem.modelData.lastLine.rating) : ""
                            color: page.colors[scoreItem.index % page.colors.length]
                            font.pixelSize: 13
                        }
                    }
                }
            }

            // Note bars and the singers' pitch: two seconds back, six ahead.
            Canvas {
                id: lane
                Layout.fillWidth: true
                Layout.fillHeight: true
                Layout.minimumHeight: 120
                readonly property real back: 2000
                readonly property real ahead: 6000
                onPaint: {
                    const ctx = getContext("2d")
                    ctx.reset()
                    const w = width, h = height
                    const now = live.songMs
                    const t0 = now - back, t1 = now + ahead
                    const x = (t) => (t - t0) / (t1 - t0) * w
                    // Pitch range of the notes in view (at least an octave and a half).
                    let lo = 1e9, hi = -1e9
                    for (const l of page.lines) {
                        if (l.end < t0 || l.start > t1)
                            continue
                        for (const n of l.notes) {
                            if (n.k === "F")
                                continue
                            lo = Math.min(lo, n.p); hi = Math.max(hi, n.p)
                        }
                    }
                    if (lo > hi) { lo = 55; hi = 72 }
                    const mid = (lo + hi) / 2
                    const span = Math.max(18, hi - lo + 4)
                    lo = mid - span / 2; hi = mid + span / 2
                    const y = (p) => h - (p - lo) / (hi - lo) * h
                    const barH = Math.max(6, h / span * 0.8)
                    // Grid of octaves.
                    ctx.strokeStyle = Qt.alpha(Theme.text, 0.08)
                    ctx.lineWidth = 1
                    for (let p = Math.ceil(lo); p <= hi; p++) {
                        if (((p % 12) + 12) % 12 !== 0)
                            continue
                        ctx.beginPath(); ctx.moveTo(0, y(p)); ctx.lineTo(w, y(p)); ctx.stroke()
                    }
                    // Notes.
                    for (const l of page.lines) {
                        if (l.end < t0 || l.start > t1)
                            continue
                        const voiceColor = l.voice === 2 ? "#ffb74d" : Theme.accent
                        for (const n of l.notes) {
                            const a = n.t * 1000, b = (n.t + n.d) * 1000
                            if (b < t0 || a > t1)
                                continue
                            const golden = n.k === "*" || n.k === "G"
                            const free = n.k === "F"
                            ctx.fillStyle = free ? Qt.alpha(Theme.text, 0.12)
                                : golden ? Qt.alpha("#ffd54f", b < now ? 0.45 : 0.85)
                                : Qt.alpha(voiceColor, b < now ? 0.3 : 0.6)
                            const bx = x(a), bw = Math.max(2, x(b) - bx - 1)
                            ctx.fillRect(bx, y(n.p) - barH / 2, bw, barH)
                        }
                    }
                    // Now.
                    ctx.strokeStyle = Qt.alpha(Theme.text, 0.5)
                    ctx.beginPath(); ctx.moveTo(x(now), 0); ctx.lineTo(x(now), h); ctx.stroke()
                    // What each singer sang.
                    for (let i = 0; i < live.trails.length; i++) {
                        const tr = live.trails[i]
                        const color = page.colors[i % page.colors.length]
                        for (let k = 0; k + 2 < tr.length; k += 3) {
                            const t = tr[k], p = tr[k + 1], hit = tr[k + 2]
                            if (p === null || t < t0)
                                continue
                            const yy = y(Math.max(lo, Math.min(hi, p)))
                            ctx.fillStyle = hit ? color : Qt.alpha(color, 0.35)
                            const s = hit ? 5 : 3
                            ctx.fillRect(x(t) - s / 2, yy - s / 2, s, s)
                        }
                    }
                }
            }

            // The text: the line sung now (sung syllables lit), the next one dimmer.
            Repeater {
                model: 2
                Label {
                    id: lyricLine
                    required property int index
                    readonly property int li: {
                        const at = page.lineAt(live.songMs, 0)
                        return at < 0 ? -1 : at + index
                    }
                    readonly property var line: li >= 0 && li < page.lines.length ? page.lines[li] : null
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.WordWrap
                    textFormat: Text.StyledText
                    font.pixelSize: index === 0 ? (Theme.fingers ? 26 : 24) : 18
                    font.weight: index === 0 ? Font.DemiBold : Font.Normal
                    color: index === 0 ? Theme.textDim : Theme.textFaint
                    function esc(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;") }
                    text: {
                        if (!line)
                            return " "
                        let lit = "", rest = ""
                        for (const n of line.notes) {
                            // "~" marks a syllable going on at another pitch.
                            const s = (n.s || "").replace(/^~/, "")
                            if (index === 0 && n.t * 1000 <= live.songMs)
                                lit += esc(s)
                            else
                                rest += esc(s)
                        }
                        const color = line.voice === 2 ? "#ffb74d" : Theme.accentText
                        return (lit ? "<font color=\"" + color + "\">" + lit + "</font>" : "") + rest
                    }
                }
            }

            RowLayout {
                Layout.alignment: Qt.AlignHCenter
                spacing: 8
                Button {
                    flat: true
                    text: Core.state.play === "playing" ? Theme.tr("Pause") : Theme.tr("Continue")
                    icon.source: Theme.icon(Core.state.play === "playing" ? "pause" : "play_arrow")
                    onClicked: Core.call(Core.state.play === "playing" ? "pause" : "play")
                }
                Button {
                    flat: true
                    text: Theme.tr("End")
                    icon.source: Theme.icon("close")
                    onClicked: page.stop()
                }
            }
        }

        // ---- results ----
        Flickable {
            visible: page.stage === "results"
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentHeight: resultCol.implicitHeight + 16
            clip: true
            ColumnLayout {
                id: resultCol
                width: parent.width
                spacing: 10
                SectionLabel { text: Theme.tr("Results") }
                Label {
                    visible: page.results.textScoring === "off"
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                    text: Theme.tr("Without points for the text: they need a speech model with a CTC head on this device.")
                }
                Repeater {
                    model: page.results.singers || []
                    Rectangle {
                        id: resultCard
                        required property var modelData
                        required property int index
                        readonly property int slot: (live.singers || []).findIndex(s => s.stream === modelData.stream)
                        Layout.fillWidth: true
                        Layout.leftMargin: 8
                        Layout.rightMargin: 8
                        implicitHeight: resultRow.implicitHeight + 24
                        radius: Theme.radius
                        color: Theme.surfaceVariant
                        RowLayout {
                            id: resultRow
                            anchors.fill: parent
                            anchors.margins: 12
                            spacing: 12
                            Label {
                                textFormat: Text.PlainText
                                text: (resultCard.index + 1) + "."
                                font.pixelSize: 22
                                font.weight: Font.Bold
                                color: Theme.textDim
                            }
                            Rectangle {
                                width: 14; height: 14; radius: 7
                                color: page.colors[Math.max(0, resultCard.slot) % page.colors.length]
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    text: resultCard.modelData.name
                                    font.pixelSize: 16
                                    font.weight: Font.DemiBold
                                    color: Theme.text
                                    elide: Text.ElideRight
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    wrapMode: Text.WordWrap
                                    font.pixelSize: 12
                                    color: Theme.textDim
                                    text: [page.ratingText(resultCard.modelData.rating),
                                           Math.round((resultCard.modelData.accuracy || 0) * 100) + " % " + Theme.tr("on pitch"),
                                           Theme.tr("Notes") + " " + Math.round(resultCard.modelData.notes || 0),
                                           Theme.tr("Held notes") + " " + (resultCard.modelData.heldNotes || 0) + "/" + (resultCard.modelData.longNotes || 0),
                                           resultCard.modelData.text !== null && resultCard.modelData.text !== undefined
                                               ? Theme.tr("Text") + " " + Math.round(resultCard.modelData.text) : ""
                                          ].filter(x => x).join(" · ")
                                }
                            }
                            Label {
                                textFormat: Text.PlainText
                                text: Math.round(resultCard.modelData.points || 0)
                                font.pixelSize: 28
                                font.weight: Font.Bold
                                font.features: { "tnum": 1 }
                                color: Theme.text
                            }
                        }
                    }
                }
                RowLayout {
                    Layout.alignment: Qt.AlignHCenter
                    spacing: 8
                    AccentButton {
                        highlighted: true
                        text: Theme.tr("Sing again")
                        icon.source: Theme.icon("refresh")
                        onClicked: page.stage = "setup"
                    }
                    Button {
                        flat: true
                        text: Theme.tr("Another song")
                        icon.source: Theme.icon("search")
                        onClicked: page.stage = "pick"
                    }
                }
            }
        }
    }
}
