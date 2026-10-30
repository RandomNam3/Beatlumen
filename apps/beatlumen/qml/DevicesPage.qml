import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The other devices of this server login: what they play, remote control,
// taking their playback over, sending this device's playback there and
// playing together.
Item {
    id: page
    signal openSettings()

    readonly property bool connected: Core.state.server === "connected"
    readonly property int rev: Core.state.devicesRev || 0
    readonly property var me: (Core.state.current && String(Core.state.current.id).indexOf(":") < 0) ? Core.state.current : null
    property var devices: []
    function reload() {
        const all = Core.call("devices.list") || []
        // Online devices first, this one is not listed.
        devices = all.filter(d => !d.self).sort((a, b) => (b.online === true) - (a.online === true))
    }
    onRevChanged: reload()
    Component.onCompleted: { Core.call("devices.refresh"); reload() }
    // Positions move on while songs play.
    Timer {
        interval: 1000
        repeat: true
        running: page.visible && page.devices.some(d => d.online && d.state && d.state.play === "playing")
        onTriggered: page.reload()
    }

    function send(d, command) { Core.call("devices.command", { id: d.id, command: command }) }
    function time(s) {
        s = Math.max(0, Math.floor(s || 0))
        return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0")
    }
    function platformName(p) {
        return ({ android: "Android", linux: "Linux", windows: "Windows", web: Theme.tr("Browser"), macos: "macOS" })[p] || p || ""
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 8

        PageHeader {
            Layout.fillWidth: true
            title: Theme.tr("Devices")
            subtitle: page.connected ? Theme.tr("This device:") + " " + (Core.state.deviceName || "") : ""
            IconButton {
                visible: page.connected
                iconName: "refresh"
                tip: Theme.tr("Refresh")
                onClicked: Core.call("devices.refresh")
            }
            IconButton {
                iconName: "settings"
                tip: Theme.tr("Settings")
                onClicked: page.openSettings()
            }
        }

        EmptyState {
            visible: !page.connected
            Layout.fillWidth: true
            Layout.topMargin: 48
            iconName: "devices"
            title: Theme.tr("Devices")
            text: Theme.tr("Connect to a server: the apps logged in to it see each other, can control each other and take over each other's playback.")
        }
        EmptyState {
            visible: page.connected && page.devices.length === 0
            Layout.fillWidth: true
            Layout.topMargin: 48
            iconName: "devices"
            title: Theme.tr("No other devices yet")
            text: Theme.tr("Open Beatlumen on another phone, computer or in the browser and connect it to this server.")
        }

        ListView {
            id: list
            visible: page.connected && page.devices.length > 0
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: 10
            model: page.devices
            ScrollBar.vertical: ScrollBar { }
            delegate: Rectangle {
                id: card
                required property var modelData
                readonly property var st: modelData.state || ({})
                readonly property var track: st.track || null
                readonly property bool online: modelData.online === true
                readonly property bool remote: modelData.remote !== false
                readonly property bool playing: st.play === "playing"
                readonly property bool canControl: online && remote
                width: ListView.view.width
                height: col.implicitHeight + 28
                radius: Theme.radius
                color: Theme.surfaceVariant
                opacity: online ? 1 : 0.55

                ColumnLayout {
                    id: col
                    anchors.fill: parent
                    anchors.margins: 14
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 12
                        Rectangle {
                            width: 40; height: 40; radius: 20
                            color: card.online ? Qt.alpha(Theme.accent, 0.2) : Theme.surfaceVariant
                            Icon { anchors.centerIn: parent; name: "devices"; color: card.online ? Theme.accent : Theme.textDim }
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: card.modelData.name; font.pixelSize: 16; font.weight: Font.DemiBold; color: Theme.text; elide: Text.ElideRight }
                            Label {
                                textFormat: Text.PlainText
                                Layout.fillWidth: true
                                text: page.platformName(card.modelData.platform) + " · "
                                      + (!card.online ? Theme.tr("Offline") : card.playing ? Theme.tr("Playing") : card.st.play === "paused" ? Theme.tr("Paused") : Theme.tr("Online"))
                                font.pixelSize: 13
                                color: Theme.textDim
                                elide: Text.ElideRight
                            }
                        }
                    }

                    // What it plays.
                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: card.online && card.track !== null
                        spacing: 4
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: card.track ? card.track.title + (card.track.artist ? " · " + card.track.artist : "") : ""
                            color: Theme.text
                            elide: Text.ElideRight
                        }
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: 8
                            Label { textFormat: Text.PlainText; text: page.time(card.modelData.positionNow); font.pixelSize: 12; color: Theme.textDim }
                            Slider {
                                id: seek
                                Layout.fillWidth: true
                                enabled: card.canControl
                                from: 0
                                to: Math.max(1, card.track ? card.track.duration : 1)
                                value: pressed ? value : (card.modelData.positionNow || 0)
                                onMoved: {}
                                onPressedChanged: if (!pressed) page.send(card.modelData, { cmd: "seek", position: value })
                                Accessible.name: Theme.tr("Position")
                            }
                            Label { textFormat: Text.PlainText; text: page.time(card.track ? card.track.duration : 0); font.pixelSize: 12; color: Theme.textDim }
                        }
                    }

                    // Remote control.
                    RowLayout {
                        Layout.fillWidth: true
                        visible: card.canControl
                        spacing: 2
                        IconButton { iconName: "skip_previous"; tip: Theme.tr("Previous"); onClicked: page.send(card.modelData, { cmd: "prev" }) }
                        IconButton {
                            iconName: card.playing ? "pause" : "play_arrow"
                            tip: card.playing ? Theme.tr("Pause") : Theme.tr("Play")
                            active: true
                            onClicked: page.send(card.modelData, { cmd: "toggle" })
                        }
                        IconButton { iconName: "skip_next"; tip: Theme.tr("Next"); onClicked: page.send(card.modelData, { cmd: "next" }) }
                        Item { width: 8 }
                        Icon { name: card.st.muted ? "volume_off" : "volume_up"; color: Theme.textDim; size: 20 }
                        Slider {
                            Layout.fillWidth: true
                            from: 0; to: 1
                            value: pressed ? value : (card.st.volume ?? 0.6)
                            onPressedChanged: if (!pressed) page.send(card.modelData, { cmd: "volume", value: value })
                            Accessible.name: Theme.tr("Volume")
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: card.online && !card.remote
                        text: Theme.tr("Remote control is turned off on this device.")
                        wrapMode: Text.WordWrap
                        font.pixelSize: 13
                        color: Theme.textDim
                    }

                    // Moving playback between the devices.
                    Flow {
                        Layout.fillWidth: true
                        visible: card.canControl
                        spacing: 8
                        Button {
                            visible: card.track !== null && card.track.server === true
                            text: Theme.tr("Play here")
                            icon.source: Theme.icon("play_circle")
                            flat: true
                            onClicked: Core.call("devices.pull", { id: card.modelData.id })
                        }
                        Button {
                            visible: page.me !== null
                            text: Theme.tr("Move my music there")
                            icon.source: Theme.icon("cast")
                            flat: true
                            onClicked: Core.call("devices.transfer", { id: card.modelData.id })
                        }
                        Button {
                            visible: page.me !== null
                            text: Theme.tr("Play together")
                            icon.source: Theme.icon("speaker")
                            flat: true
                            onClicked: Core.call("devices.group", { ids: [card.modelData.id] })
                        }
                    }
                }
            }
        }
    }
}
