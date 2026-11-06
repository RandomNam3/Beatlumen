import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Bit-perfect playback: a sound card in direct use, and which one.
ColumnLayout {
    id: panel
    readonly property var settings: Core.state.settings || ({})
    readonly property var audio: Core.state.audio || ({})
    readonly property bool on: settings.audioDirect === true
    property var devices: []
    spacing: 8
    visible: audio.directAvailable === true

    function refresh() {
        devices = Core.call("audio.devices") || []
    }
    function choose(device) {
        Core.call("settings.set", { key: "audioDirectName", value: device.name })
        Core.call("settings.set", { key: "audioDirectDevice", value: device.id })
    }
    Component.onCompleted: if (visible) refresh()
    onVisibleChanged: if (visible) refresh()

    SwitchRow {
        Layout.fillWidth: true
        title: Theme.tr("Bit-perfect playback")
        detail: Theme.tr("Hands the music on exactly as it is in the file, at its own rate. Equalizer, normalizing, crossfade and skipping silence are off. Set the volume to 100 % and turn it down on your amplifier.")
        on: panel.on
        onSwitched: (on) => {
            panel.refresh()
            // The way that leaves other programs audible, where there is one.
            if (on && !panel.settings.audioDirectDevice && panel.devices.length > 0)
                panel.choose(panel.devices.find(d => d.shared) || panel.devices[0])
            Core.call("settings.set", { key: "audioDirect", value: on })
        }
    }

    ColumnLayout {
        visible: panel.on
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        spacing: 8

        RowLayout {
            Layout.fillWidth: true
            spacing: 12
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Output"); color: Theme.text; font.pixelSize: 15; wrapMode: Text.WordWrap }
            ComboBox {
                id: deviceBox
                Layout.maximumWidth: panel.width - 120
                Accessible.name: Theme.tr("Output")
                implicitContentWidthPolicy: ComboBox.WidestText
                model: panel.devices.map(d => d.shared ? Theme.tr("%1: other programs stay audible").arg(d.name) : Theme.tr("%1 alone").arg(d.name))
                displayText: currentIndex >= 0 ? currentText : Theme.tr("Please choose")
                currentIndex: panel.devices.findIndex(d => d.id === panel.settings.audioDirectDevice)
                onActivated: (i) => panel.choose(panel.devices[i])
            }
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: text !== ""
            text: {
                const d = panel.devices.find(x => x.id === panel.settings.audioDirectDevice)
                if (!d)
                    return ""
                return d.shared
                    ? Theme.tr("The sound server runs at each song's rate, for other programs too. Bit-perfect as long as nothing else makes sound: then it has to mix, and the signal path says so.")
                    : Theme.tr("Only this app is on the card: other programs move to another output or stay silent, until you pause for 30 s or switch this off. Exact whatever else is going on.")
            }
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: panel.devices.length === 0
            text: Theme.tr("No sound card found.")
            color: Theme.textDim
            font.pixelSize: 13
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: !!panel.audio.directError
            text: panel.audio.directError || ""
            color: Theme.danger
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
    }

    ColumnLayout {
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        spacing: 4
        Label { textFormat: Text.PlainText; text: Theme.tr("Signal path"); color: Theme.text; font.pixelSize: 15 }
        SignalPath { Layout.fillWidth: true }
    }
}
