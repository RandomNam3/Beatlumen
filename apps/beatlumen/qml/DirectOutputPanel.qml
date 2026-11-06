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
        detail: Theme.tr("Hands the music to a sound card exactly as it is in the file: no converting, no mixing. While it runs, other programs have no sound on this card. Equalizer, normalizing, crossfade and skipping silence are off. Set the volume to 100 % and turn it down on your amplifier.")
        on: panel.on
        onSwitched: (on) => {
            panel.refresh()
            if (on && !panel.settings.audioDirectDevice && panel.devices.length === 1)
                panel.choose(panel.devices[0])
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
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Sound card"); color: Theme.text; font.pixelSize: 15; wrapMode: Text.WordWrap }
            ComboBox {
                id: deviceBox
                Layout.maximumWidth: panel.width - 120
                Accessible.name: Theme.tr("Sound card")
                implicitContentWidthPolicy: ComboBox.WidestText
                model: panel.devices.map(d => d.name)
                displayText: currentIndex >= 0 ? currentText : Theme.tr("Please choose")
                currentIndex: panel.devices.findIndex(d => d.id === panel.settings.audioDirectDevice)
                onActivated: (i) => panel.choose(panel.devices[i])
            }
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
