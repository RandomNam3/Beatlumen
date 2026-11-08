import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Playback controls. Three layouts:
//  - "mini":     compact bar while a page is open on phones
//  - "expanded": big controls on the phone's now-playing view
//  - "full":     desktop bar with everything in one row
Item {
    id: bar
    property string mode: "full"
    // Little height (a phone held sideways).
    property bool dense: false
    // The line being sung may show under the title (not while the whole
    // text is on screen anyway).
    property bool showLine: true
    signal openNowPlaying()
    signal openQueue()

    readonly property var cur: Core.state.current || null
    readonly property bool playing: Core.state.play === "playing"
    readonly property real duration: Core.state.duration || 0
    readonly property string repeatMode: Core.state.repeat || "None"

    // The expanded controls float over the visualizer: always light.
    readonly property bool overVisual: mode === "expanded"
    readonly property color fg: overVisual ? "white" : Theme.text
    readonly property color fgDim: overVisual ? Qt.rgba(1, 1, 1, 0.75) : Theme.textDim

    // ---- the line being sung ------------------------------------------------
    readonly property bool lineOn: showLine && visible && mode !== "expanded"
        && ((Core.state.settings || {}).ui || {}).barLyrics !== false
    readonly property string curId: cur ? cur.id : ""
    readonly property int lyricsRev: Core.state.lyricsRev || 0
    property var timedLines: []
    property string line: ""
    function loadLines() {
        line = ""
        const info = lineOn && curId !== "" ? (Core.call("lyrics.get") || {}) : {}
        timedLines = info.status === "ready" && info.synced === true ? info.lines : []
    }
    function lineAt(t) {
        let lo = 0, hi = timedLines.length - 1, found = -1
        while (lo <= hi) {
            const mid = (lo + hi) >> 1
            if (timedLines[mid].t <= t) { found = mid; lo = mid + 1 } else hi = mid - 1
        }
        return found < 0 ? "" : timedLines[found].text
    }
    onCurIdChanged: loadLines()
    onLyricsRevChanged: loadLines()
    onLineOnChanged: loadLines()
    Timer {
        interval: 200
        repeat: true
        // The same small lead as the full text: a line lights up as it starts.
        running: bar.lineOn && bar.timedLines.length > 0
        onTriggered: bar.line = bar.lineAt(Core.position + 0.15)
    }

    implicitHeight: mode === "mini" ? (Theme.fingers ? 72 : 56) : mode === "expanded" ? 196 : dense ? 64 : 76

    component Transport: RowLayout {
        property int big: 56
        spacing: bar.mode === "expanded" && bar.width >= 360 ? 12 : 4
        IconButton {
            iconName: "shuffle"; tip: Theme.tr("Shuffle"); active: Core.state.shuffle === true; iconColor: bar.fg
            onClicked: Core.call("shuffle")
        }
        IconButton {
            iconName: "skip_previous"; tip: Theme.tr("Previous"); iconSize: Theme.fingers ? 30 : 24; iconColor: bar.fg
            onClicked: Core.call("prev")
        }
        RoundButton {
            id: playButton
            Layout.preferredWidth: parent.big
            Layout.preferredHeight: parent.big
            highlighted: true
            icon.source: Theme.icon(bar.playing ? "pause" : "play_arrow")
            icon.width: parent.big * 0.5
            icon.height: parent.big * 0.5
            icon.color: Theme.onAccent
            Material.background: Theme.accent
            readonly property string tip: bar.playing ? Theme.tr("Pause") : Theme.tr("Play")
            Accessible.name: tip
            onClicked: Core.call("toggle")
        }
        IconButton {
            iconName: "skip_next"; tip: Theme.tr("Next"); iconSize: Theme.fingers ? 30 : 24; iconColor: bar.fg
            onClicked: Core.call("next")
        }
        IconButton {
            iconName: bar.repeatMode === "Item" ? "repeat_one" : "repeat"
            iconColor: bar.fg
            tip: Theme.tr("Repeat")
            active: bar.repeatMode !== "None"
            onClicked: Core.call("repeat")
        }
    }

    component Seek: ColumnLayout {
        property bool showTimes: true
        spacing: 0
        Slider {
            id: seek
            Layout.fillWidth: true
            from: 0
            to: Math.max(1, bar.duration)
            enabled: bar.duration > 0
            value: pressed ? value : Core.seekPosition
            onMoved: if (!pressed) Core.call("seek", { position: value })
            onPressedChanged: if (!pressed) Core.call("seek", { position: value })
            Accessible.name: Theme.tr("Position in the song")
        }
        RowLayout {
            visible: parent.showTimes
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            Layout.topMargin: -10
            Label { textFormat: Text.PlainText; text: Theme.fmtTime(seek.pressed ? seek.value : Core.position); color: bar.fgDim; font.pixelSize: 12; font.features: { "tnum": 1 } }
            Item { Layout.fillWidth: true }
            Label { textFormat: Text.PlainText; text: bar.duration > 0 || !bar.cur ? Theme.fmtTime(bar.duration) : Theme.tr("Live"); color: bar.fgDim; font.pixelSize: 12; font.features: { "tnum": 1 } }
        }
    }

    component TrackInfo: RowLayout {
        property int coverSize: 48
        spacing: 12
        // Cover and title lead to the visual.
        TapHandler {
            enabled: bar.mode === "full"
            onTapped: bar.openNowPlaying()
        }
        CoverImage {
            Layout.preferredWidth: parent.coverSize
            Layout.preferredHeight: parent.coverSize
            source: bar.cur ? Covers.url(bar.cur.id, bar.cur.cover) : ""
            Component.onCompleted: if (bar.cur && (bar.cur.local || bar.cur.source)) Covers.request(bar.cur.id)
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: bar.cur ? bar.cur.title : Theme.tr("Nothing playing")
                font.pixelSize: 15
                font.weight: Font.DemiBold
                elide: Text.ElideRight
                color: Theme.text
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: bar.line !== "" ? bar.line
                      : bar.cur ? (bar.cur.artist || Theme.tr("Unknown artist")) : Theme.tr("Pick something from your library")
                font.pixelSize: 13
                elide: Text.ElideRight
                color: Theme.textDim
            }
        }
        FavoriteButton {
            trackId: bar.cur ? bar.cur.id : ""
            visible: Favorites.canMark(trackId) && (bar.width >= 400 || on)
            iconColor: Theme.textDim
        }
    }

    Connections {
        target: Core
        function onStateChanged() {
            if (bar.cur && (bar.cur.local || bar.cur.source) && !bar.cur.cover)
                Covers.request(bar.cur.id)
        }
    }

    // ---- mini ------------------------------------------------------------
    Item {
        anchors.fill: parent
        visible: bar.mode === "mini"
        Rectangle {
            anchors.fill: parent
            color: Theme.surfaceStrong
            Rectangle {
                anchors.top: parent.top
                height: 2
                width: bar.duration > 0 ? parent.width * Math.min(1, Core.position / bar.duration) : 0
                color: Theme.accent
            }
        }
        AbstractButton {
            anchors.fill: parent
            focusPolicy: Qt.TabFocus
            Accessible.role: Accessible.Button
            Accessible.name: Theme.tr("Now playing") + (bar.cur ? ": " + bar.cur.title : "")
            onClicked: bar.openNowPlaying()
            Keys.onReturnPressed: clicked()
            Keys.onEnterPressed: clicked()
        }
        // A tap opens the visual; sideways goes to the next or the previous
        // song. The button above stays for the keyboard and screen readers.
        MouseArea {
            id: swipe
            anchors.fill: parent
            property real startX: 0
            property bool moved: false
            function sceneX(mouse) { return mapToItem(null, mouse.x, mouse.y).x }
            onPressed: (mouse) => { startX = sceneX(mouse); moved = false }
            onPositionChanged: (mouse) => {
                const dx = sceneX(mouse) - startX
                if (Math.abs(dx) > Qt.styleHints.startDragDistance)
                    moved = true
                if (moved)
                    miniInfo.shift = dx
            }
            onReleased: {
                if (Math.abs(miniInfo.shift) > Math.min(96, bar.width * 0.25) && bar.cur)
                    Core.call(miniInfo.shift < 0 ? "next" : "prev")
                miniInfo.shift = 0
            }
            onClicked: if (!moved) bar.openNowPlaying()
        }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 4
            spacing: 4
            TrackInfo {
                id: miniInfo
                Layout.fillWidth: true
                coverSize: Theme.fingers ? 48 : 40
                property real shift: 0
                opacity: 1 - Math.min(0.6, Math.abs(shift) / 300)
                transform: Translate { x: miniInfo.shift }
                Behavior on shift {
                    enabled: !swipe.pressed
                    NumberAnimation { duration: Theme.ms(180); easing.type: Easing.OutCubic }
                }
            }
            IconButton {
                iconName: bar.playing ? "pause" : "play_arrow"
                iconSize: 32
                tip: bar.playing ? Theme.tr("Pause") : Theme.tr("Play")
                onClicked: Core.call("toggle")
            }
            IconButton {
                iconName: "skip_next"; iconSize: 28; tip: Theme.tr("Next")
                onClicked: Core.call("next")
            }
        }
    }

    // ---- expanded (phone now playing) ------------------------------------
    // Scrim for contrast on bright visuals.
    Rectangle {
        visible: bar.mode === "expanded"
        anchors.fill: parent
        anchors.topMargin: -48
        gradient: Gradient {
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 0.45; color: Qt.rgba(0, 0, 0, 0.35) }
            GradientStop { position: 1; color: Qt.rgba(0, 0, 0, 0.55) }
        }
    }
    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: 16
        anchors.rightMargin: 16
        visible: bar.mode === "expanded"
        spacing: 4
        Seek { Layout.fillWidth: true }
        Transport { Layout.alignment: Qt.AlignHCenter; big: bar.width >= 360 ? 72 : 56 }
        RowLayout {
            Layout.fillWidth: true
            OutputButton { fg: bar.fg }
            Item { Layout.fillWidth: true }
            IconButton {
                iconName: "queue_music"; tip: Theme.tr("Queue"); iconColor: bar.fg
                onClicked: bar.openQueue()
            }
        }
    }

    // ---- full (desktop) ----------------------------------------------------
    Rectangle {
        anchors.fill: parent
        visible: bar.mode === "full"
        color: Theme.surfaceStrong
        Seek {
            id: fullSeek
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.topMargin: -14
            showTimes: false
        }
        RowLayout {
            anchors.fill: parent
            anchors.topMargin: 14
            anchors.leftMargin: 16
            anchors.rightMargin: 16
            spacing: 12
            TrackInfo {
                Layout.fillWidth: true
                Layout.preferredWidth: Math.min(360, bar.width * 0.3)
                Layout.maximumWidth: 360
                Layout.minimumWidth: 140
                coverSize: bar.dense ? 40 : 44
            }
            Label {
                visible: bar.width >= 1040
                textFormat: Text.PlainText
                text: Theme.fmtTime(Core.position) + " / " + (bar.duration > 0 || !bar.cur ? Theme.fmtTime(bar.duration) : Theme.tr("Live"))
                color: Theme.textDim
                font.pixelSize: 12
                font.features: { "tnum": 1 }
            }
            Item { Layout.fillWidth: true }
            Transport { big: bar.dense ? 40 : 44 }
            Item { Layout.fillWidth: true }
            OutputButton { }
            IconButton {
                iconName: Core.state.muted ? "volume_off" : "volume_up"
                tip: Theme.tr("Mute")
                onClicked: Core.call("mute")
            }
            Slider {
                visible: bar.width >= 940
                Layout.preferredWidth: 120
                from: 0; to: 1
                value: Core.state.muted ? 0 : (Core.state.volume || 0)
                onMoved: Core.call("volume", { value: value })
                Accessible.name: Theme.tr("Volume")
            }
            IconButton {
                iconName: "queue_music"; tip: Theme.tr("Queue")
                onClicked: bar.openQueue()
            }
            IconButton {
                iconName: "graphic_eq"; tip: Theme.tr("Now playing")
                onClicked: bar.openNowPlaying()
            }
        }
    }
}
