import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The equalizer: ten bands, a preamp, a few starting points and the curve that
// results. Boosts lower the level by themselves, so nothing clips.
ColumnLayout {
    id: panel
    readonly property var settings: Core.state.settings || ({})
    readonly property bool on: settings.eqEnabled === true
    readonly property var bands: settings.eqBands || [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    readonly property var names: ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"]
    readonly property var presets: [
        { name: Theme.tr("Flat"), bands: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
        { name: Theme.tr("More bass"), bands: [6, 5, 4, 2, 0, 0, 0, 0, 0, 0] },
        { name: Theme.tr("More treble"), bands: [0, 0, 0, 0, 0, 0, 1, 3, 5, 6] },
        { name: Theme.tr("Voices"), bands: [-3, -2, -1, 0, 2, 3, 3, 2, 0, -1] },
        { name: Theme.tr("Warm"), bands: [3, 3, 2, 1, 0, 0, -1, -1, -2, -3] },
        { name: Theme.tr("Bright and full"), bands: [4, 3, 1, 0, -1, -1, 0, 1, 3, 4] },
        { name: Theme.tr("Quiet listening"), bands: [6, 4, 2, 0, 0, 0, 0, 1, 3, 4] }
    ]
    // The chosen starting point; -1: the bands were moved by hand.
    readonly property int presetIndex: presets.findIndex(p => sameBands(p.bands, bands))
    spacing: 8

    function sameBands(a, b) {
        return a.length === b.length && a.every((v, i) => Math.abs(v - b[i]) < 0.01)
    }
    function setBand(index, value) {
        const next = panel.bands.slice()
        next[index] = value
        Core.call("settings.set", { key: "eqBands", value: next })
    }
    function dbText(v) {
        return (v > 0 ? "+" : "") + v.toFixed(1).replace(/\.0$/, "") + " dB"
    }

    SwitchRow {
        Layout.fillWidth: true
        title: Theme.tr("Equalizer")
        detail: Theme.tr("Raises or lowers the bass, mids and treble. Applies at once, also on speakers in the network.")
        on: panel.on
        onSwitched: (on) => Core.call("settings.set", { key: "eqEnabled", value: on })
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        visible: panel.on && (Core.state.audio || {}).directActive === true
        text: Theme.tr("Not applied during bit-perfect playback.")
        color: Theme.textDim
        font.pixelSize: 13
        wrapMode: Text.WordWrap
    }

    ColumnLayout {
        visible: panel.on
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        spacing: 8

        RowLayout {
            Layout.fillWidth: true
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Sound"); color: Theme.text; font.pixelSize: 15 }
            ComboBox {
                implicitContentWidthPolicy: ComboBox.WidestText
                Accessible.name: Theme.tr("Sound")
                model: panel.presets.map(p => p.name).concat(panel.presetIndex < 0 ? [Theme.tr("Your own")] : [])
                currentIndex: panel.presetIndex < 0 ? panel.presets.length : panel.presetIndex
                onActivated: (i) => {
                    if (i < panel.presets.length)
                        Core.call("settings.set", { key: "eqBands", value: panel.presets[i].bands })
                }
            }
        }

        // The curve that results from the bands.
        Canvas {
            id: curve
            Layout.fillWidth: true
            Layout.preferredHeight: 72
            readonly property color line: Theme.accent
            readonly property color grid: Theme.outline
            Accessible.role: Accessible.Graphic
            Accessible.name: Theme.tr("Equalizer") + ": " + panel.bands.map((v, i) => panel.names[i] + " " + panel.dbText(v)).join(", ")
            onLineChanged: requestPaint()
            onGridChanged: requestPaint()
            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()
            onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                const w = width, h = height
                const y = (db) => h / 2 - db / 12 * (h / 2 - 2)
                ctx.strokeStyle = grid
                ctx.lineWidth = 1
                ctx.beginPath()
                ctx.moveTo(0, y(0))
                ctx.lineTo(w, y(0))
                ctx.stroke()
                const values = Core.call("eq.curve", { columns: 96 }) || []
                if (values.length < 2)
                    return
                ctx.strokeStyle = line
                ctx.lineWidth = 2
                ctx.beginPath()
                values.forEach((v, i) => {
                    const px = i / (values.length - 1) * w
                    if (i === 0)
                        ctx.moveTo(px, y(v))
                    else
                        ctx.lineTo(px, y(v))
                })
                ctx.stroke()
            }
            Connections {
                target: panel
                function onBandsChanged() { curve.requestPaint() }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: 0
            Repeater {
                model: 10
                delegate: Item {
                    id: bandItem
                    required property int index
                    Layout.fillWidth: true
                    implicitWidth: 28
                    implicitHeight: 172
                    Slider {
                        anchors.top: parent.top
                        anchors.horizontalCenter: parent.horizontalCenter
                        width: 28
                        height: 150
                        padding: 0
                        orientation: Qt.Vertical
                        from: -12; to: 12; stepSize: 0.5
                        value: panel.bands[bandItem.index] || 0
                        onMoved: panel.setBand(bandItem.index, value)
                        Accessible.name: panel.names[bandItem.index].endsWith("k") ? panel.names[bandItem.index].replace("k", " kHz") : panel.names[bandItem.index] + " Hz"
                        Accessible.description: panel.dbText(value)
                        ToolTip.visible: pressed
                        ToolTip.text: panel.dbText(value)
                    }
                    Label {
                        textFormat: Text.PlainText
                        anchors.bottom: parent.bottom
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: panel.names[bandItem.index]
                        color: Theme.textDim
                        font.pixelSize: 11
                    }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Level"); color: Theme.text; font.pixelSize: 15 }
            Label { textFormat: Text.PlainText; text: panel.dbText(levelSlider.value); color: Theme.textDim }
        }
        Slider {
            id: levelSlider
            Layout.fillWidth: true
            from: -12; to: 12; stepSize: 0.5
            value: panel.settings.eqPreamp || 0
            onMoved: Core.call("settings.set", { key: "eqPreamp", value: value })
            Accessible.name: Theme.tr("Level")
            Accessible.description: panel.dbText(value)
        }
        Button {
            flat: true
            text: Theme.tr("Reset")
            visible: !panel.sameBands(panel.bands, panel.presets[0].bands) || (panel.settings.eqPreamp || 0) !== 0
            onClicked: {
                Core.call("settings.set", { key: "eqBands", value: panel.presets[0].bands })
                Core.call("settings.set", { key: "eqPreamp", value: 0 })
            }
        }
    }
}
