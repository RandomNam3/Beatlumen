import QtQuick
import QtQuick.Controls
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
    signal openNowPlaying()
    signal openQueue()

    readonly property var cur: Core.state.current || null
    readonly property bool playing: Core.state.play === "playing"
    readonly property real duration: Core.state.duration || 0
    readonly property string repeatMode: Core.state.repeat || "None"

    implicitHeight: mode === "mini" ? 72 : mode === "expanded" ? 196 : 92

    component Transport: RowLayout {
        property int big: 56
        spacing: bar.mode === "expanded" ? 12 : 4
        IconButton {
            iconName: "shuffle"; tip: Theme.tr("Shuffle"); active: Core.state.shuffle === true
            onClicked: Core.call("shuffle")
        }
        IconButton {
            iconName: "skip_previous"; tip: Theme.tr("Previous"); iconSize: 30
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
            icon.color: "white"
            Material.background: Theme.accent
            Accessible.name: bar.playing ? Theme.tr("Pause") : Theme.tr("Play")
            onClicked: Core.call("toggle")
        }
        IconButton {
            iconName: "skip_next"; tip: Theme.tr("Next"); iconSize: 30
            onClicked: Core.call("next")
        }
        IconButton {
            iconName: bar.repeatMode === "Item" ? "repeat_one" : "repeat"
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
            value: pressed ? value : Core.position
            onMoved: if (!pressed) Core.call("seek", { position: value })
            onPressedChanged: if (!pressed) Core.call("seek", { position: value })
            Accessible.name: "Position"
        }
        RowLayout {
            visible: parent.showTimes
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            Layout.topMargin: -10
            Label { text: Theme.time(seek.pressed ? seek.value : Core.position); color: Theme.textDim; font.pixelSize: 12; font.features: { "tnum": 1 } }
            Item { Layout.fillWidth: true }
            Label { text: Theme.time(bar.duration); color: Theme.textDim; font.pixelSize: 12; font.features: { "tnum": 1 } }
        }
    }

    component TrackInfo: RowLayout {
        property int coverSize: 48
        spacing: 12
        CoverImage {
            Layout.preferredWidth: parent.coverSize
            Layout.preferredHeight: parent.coverSize
            source: bar.cur ? Covers.url(bar.cur.id, bar.cur.cover) : ""
            Component.onCompleted: if (bar.cur && bar.cur.local) Covers.request(bar.cur.id)
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            Label {
                Layout.fillWidth: true
                text: bar.cur ? bar.cur.title : Theme.tr("Nothing playing")
                font.pixelSize: 15
                font.weight: Font.DemiBold
                elide: Text.ElideRight
                color: Theme.text
            }
            Label {
                Layout.fillWidth: true
                text: bar.cur ? (bar.cur.artist || Theme.tr("Unknown artist")) : Theme.tr("Pick something from your library")
                font.pixelSize: 13
                elide: Text.ElideRight
                color: Theme.textDim
            }
        }
    }

    Connections {
        target: Core
        function onStateChanged() {
            if (bar.cur && bar.cur.local && !bar.cur.cover)
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
        MouseArea {
            anchors.fill: parent
            onClicked: bar.openNowPlaying()
        }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 4
            spacing: 4
            TrackInfo { Layout.fillWidth: true; coverSize: 48 }
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
    ColumnLayout {
        anchors.fill: parent
        anchors.leftMargin: 16
        anchors.rightMargin: 16
        visible: bar.mode === "expanded"
        spacing: 4
        Seek { Layout.fillWidth: true }
        Transport { Layout.alignment: Qt.AlignHCenter; big: 72 }
        RowLayout {
            Layout.fillWidth: true
            Item { Layout.fillWidth: true }
            IconButton {
                iconName: "queue_music"; tip: Theme.tr("Queue")
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
                Layout.preferredWidth: Math.min(360, bar.width * 0.3)
                coverSize: 52
            }
            Label {
                text: Theme.time(Core.position) + " / " + Theme.time(bar.duration)
                color: Theme.textDim
                font.pixelSize: 12
                font.features: { "tnum": 1 }
            }
            Item { Layout.fillWidth: true }
            Transport { big: 52 }
            Item { Layout.fillWidth: true }
            IconButton {
                iconName: Core.state.muted ? "volume_off" : "volume_up"
                tip: Theme.tr("Mute")
                onClicked: Core.call("mute")
            }
            Slider {
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
        }
    }
}
