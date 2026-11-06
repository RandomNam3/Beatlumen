import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Bit-perfect playback: a sound card in direct use, and which one. Android below
// version 14 has no way to do it, and it says so.
ColumnLayout {
    id: panel
    readonly property var settings: Core.state.settings || ({})
    readonly property var audio: Core.state.audio || ({})
    readonly property bool available: audio.directAvailable === true
    readonly property bool android: Core.platform === "android"
    readonly property bool on: settings.audioDirect === true && available
    property var devices: []
    spacing: 8
    visible: available || android

    function refresh() {
        devices = Core.call("audio.devices") || []
    }
    function choose(device) {
        Core.call("settings.set", { key: "audioDirectName", value: device.name })
        Core.call("settings.set", { key: "audioDirectDevice", value: device.id })
    }
    Component.onCompleted: if (visible) refresh()
    onVisibleChanged: if (visible) refresh()

    ColumnLayout {
        visible: !panel.available
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        spacing: 2
        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Bit-perfect playback"); color: Theme.text; font.pixelSize: 15; wrapMode: Text.WordWrap }
        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Not available: this needs Android 14 or newer."); color: Theme.textDim; font.pixelSize: 13; wrapMode: Text.WordWrap }
    }

    SwitchRow {
        visible: panel.available
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
                implicitContentWidthPolicy: panel.devices.length > 0 ? ComboBox.WidestText : ComboBox.ContentItemImplicitWidth
                model: panel.devices.map(d => d.shared ? Theme.tr("%1: other programs stay audible").arg(d.name) : Theme.tr("%1 alone").arg(d.name))
                displayText: currentIndex >= 0 ? currentText : Theme.tr("Please choose")
                currentIndex: panel.devices.findIndex(d => d.id === panel.settings.audioDirectDevice)
                onActivated: (i) => panel.choose(panel.devices[i])
                // Sound cards come and go.
                Connections {
                    target: deviceBox.popup
                    function onAboutToShow() { panel.refresh() }
                }
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
                if (panel.android)
                    return Theme.tr("Android hands the music to the USB device as it is, without mixing. Other apps and system sounds may stay silent on it until you pause for 30 s or switch this off. The volume is set on the device itself.")
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
            text: panel.android ? Theme.tr("No USB sound card with a bit-perfect mode found.") : Theme.tr("No sound card found.")
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
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
