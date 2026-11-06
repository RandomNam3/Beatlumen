import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The way of the music as it is right now: the file, what opened, what changes
// the samples. "Bit-perfect" only appears when all of it is true.
ColumnLayout {
    id: path
    readonly property var route: (Core.state.audio || {}).route || null
    readonly property string verdict: route ? route.verdict : ""
    spacing: 2

    function khz(hz) {
        return (hz / 1000).toLocaleString(Qt.locale(Theme.language), "f", hz % 1000 === 0 ? 0 : 1) + " kHz"
    }
    function verdictText() {
        switch (path.verdict) {
        case "bitperfect": return Theme.tr("Bit-perfect")
        case "unverified": return Theme.tr("Direct, cannot be verified")
        case "unprocessed": return Theme.tr("Direct, nothing changed")
        case "changed": return Theme.tr("Direct, but not bit-perfect")
        case "network": return Theme.tr("On a speaker in the network")
        case "shared": return Theme.tr("Through the system's mixer")
        default: return ""
        }
    }
    function verdictDetail() {
        switch (path.verdict) {
        case "bitperfect": return route.shared
            ? Theme.tr("Exact, as long as no other program makes sound.")
            : Core.platform === "android"
            ? Theme.tr("Exact as far as Android tells: its bit-perfect mixer is set, and the music plays on this device in that format.")
            : Theme.tr("Every sample arrives as it is in the file.")
        case "unverified": return Theme.tr("What happens after the sound server could not be found out.")
        case "unprocessed": return Theme.tr("A lossy or float file has no exact samples to keep.")
        case "network": return Theme.tr("The speaker takes a stream at its own rate.")
        case "shared": return Theme.tr("The system may mix and convert it.")
        default: return ""
        }
    }
    function sourceText() {
        if (!route)
            return ""
        const parts = [route.codec.toUpperCase()]
        if (route.bits)
            parts.push(route.bits + " " + Theme.tr("bit"))
        parts.push(khz(route.rate))
        parts.push(route.channels === 1 ? Theme.tr("Mono") : route.channels === 2 ? Theme.tr("Stereo") : Theme.tr("%1 channels").arg(route.channels))
        return parts.join(" · ")
    }
    function outputText() {
        if (!route)
            return ""
        if (route.speaker)
            return route.speaker
        if (!route.card)
            return khz(route.outRate)
        const parts = [route.card, khz(route.outRate), route.format]
        if (route.cardChannels > 2)
            parts.push(Theme.tr("music on channels 1 and 2 of %1").arg(route.cardChannels))
        return parts.join(" · ")
    }
    function changeText(c) {
        switch (c.kind) {
        case "resampled": return Theme.tr("Converted from %1 to %2").arg(khz(c.from)).arg(khz(c.to))
        case "downmixed": return Theme.tr("%1 channels mixed down to stereo").arg(c.channels)
        case "narrowed": return Theme.tr("%1 bits reduced to %2").arg(c.from).arg(c.to)
        case "volume": return Theme.tr("Volume below 100 %")
        case "systemVolume": return Theme.tr("System volume below 100 %")
        case "mixed": return Theme.tr("Mixed with other programs (%1)").arg(c.programs)
        case "notACard": return Theme.tr("Not a sound card (Bluetooth, a filter or similar)")
        case "equalizer": return Theme.tr("Equalizer")
        case "normalized": return Theme.tr("Normalized volume")
        case "crossfade": return Theme.tr("Crossfade")
        default: return c.kind
        }
    }

    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: path.route === null
        text: Theme.tr("Nothing is playing.")
        color: Theme.textDim
        font.pixelSize: 12
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: path.route !== null
        text: path.verdictText()
        color: path.verdict === "bitperfect" ? Theme.accentText : Theme.text
        font.pixelSize: 14
        font.weight: Font.DemiBold
        wrapMode: Text.WordWrap
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: text !== ""
        text: path.verdictDetail()
        color: Theme.textDim
        font.pixelSize: 12
        wrapMode: Text.WordWrap
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: path.route !== null
        text: Theme.tr("File") + ": " + path.sourceText()
        color: Theme.textDim
        font.pixelSize: 12
        wrapMode: Text.WordWrap
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: path.route !== null
        text: Theme.tr("Output") + ": " + path.outputText()
        color: Theme.textDim
        font.pixelSize: 12
        wrapMode: Text.WordWrap
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: path.route !== null && path.route.changes.length > 0
        text: Theme.tr("Changes the samples") + ": " + (path.route ? path.route.changes.map(c => path.changeText(c)).join(", ") : "")
        color: Theme.text
        font.pixelSize: 12
        wrapMode: Text.WordWrap
    }
}
