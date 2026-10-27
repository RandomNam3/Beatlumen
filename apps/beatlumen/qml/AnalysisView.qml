import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Analysis settings of the style plus live graphs of the current song.
ScrollView {
    id: av
    clip: true
    contentWidth: availableWidth
    readonly property int rev: Core.state.presetRev || 0
    readonly property int analysisRev: Core.state.analysisRev || 0
    property var sections: []
    property var curves: null
    onRevChanged: sections = Core.call("analysis.form") || []
    Component.onCompleted: sections = Core.call("analysis.form") || []

    Timer {
        interval: 100
        repeat: true
        running: av.visible
        onTriggered: {
            av.curves = Core.call("analysis.curves", { window: 8, columns: Math.max(32, Math.floor(graphs.width / 3)) })
            energy.requestPaint(); novelty.requestPaint(); sectionsGraph.requestPaint()
        }
    }

    component Graph: Canvas {
        id: g
        property string title
        property var series: []        // [{data, color}]
        property bool bars: false
        Layout.fillWidth: true
        Layout.preferredHeight: 84
        onPaint: {
            const ctx = getContext("2d")
            ctx.reset()
            ctx.fillStyle = Qt.rgba(0, 0, 0, Theme.dark ? 0.25 : 0.06)
            ctx.fillRect(0, 0, width, height)
            ctx.fillStyle = Theme.textDim
            ctx.font = "12px sans-serif"
            ctx.fillText(g.title, 8, 16)
            const c = av.curves
            if (!c)
                return
            for (const s of g.series) {
                const d = s.data || []
                if (d.length === 0) continue
                const w = width / d.length
                ctx.fillStyle = s.color
                for (let i = 0; i < d.length; i++) {
                    const v = Math.max(0, Math.min(1, d[i] * (s.gain || 1)))
                    if (v <= 0) continue
                    const h = v * (height - 24)
                    if (g.bars) {
                        ctx.fillStyle = v >= 0.99 ? Theme.accent : "#64b5f6"
                        ctx.fillRect(i * w, height - h, Math.max(2, w), h)
                    } else {
                        ctx.fillRect(i * w, height - h, Math.max(1, w - 0.5), h)
                    }
                }
            }
            const px = (c.position - c.start) / c.window * width
            ctx.fillStyle = Theme.text
            ctx.fillRect(px, 0, 1.5, height)
        }
    }

    ColumnLayout {
        width: av.availableWidth
        spacing: 6
        RowLayout {
            Layout.leftMargin: 8
            Button { text: Theme.tr("Reset to defaults"); icon.source: Theme.icon("refresh"); flat: true; onClicked: Core.call("analysis.reset") }
            Label {
                text: Core.state.analysis === "working" ? Theme.tr("Analysing…") : Core.state.analysis === "ready" ? "" : Theme.tr("No analysis yet")
                color: Theme.textDim
            }
        }
        ColumnLayout {
            id: graphs
            Layout.fillWidth: true
            Layout.leftMargin: 12
            Layout.rightMargin: 12
            spacing: 8
            Graph {
                id: energy
                title: Theme.tr("Energy")
                series: av.curves ? [{ data: av.curves.energy, color: Theme.accent, gain: 4 }] : []
            }
            Graph {
                id: novelty
                title: Theme.tr("Beats (lows · mids · highs)")
                series: av.curves ? [
                    { data: av.curves.novelty[0], color: Qt.alpha(Theme.accent, 0.85) },
                    { data: av.curves.novelty[1], color: Qt.rgba(0.4, 0.7, 1, 0.85) },
                    { data: av.curves.novelty[2], color: Qt.rgba(0.5, 1, 0.7, 0.85) }] : []
            }
            Graph {
                id: sectionsGraph
                title: Theme.tr("Sections")
                bars: true
                series: av.curves ? [{ data: av.curves.transitions, color: Theme.accent }] : []
            }
        }
        Repeater {
            model: av.sections
            delegate: ColumnLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 6
                SectionLabel { text: modelData.title }
                Repeater {
                    model: modelData.fields
                    delegate: FieldControl {
                        required property var modelData
                        property string block: parent.modelData.block
                        Layout.fillWidth: true
                        field: modelData
                        onChanged: (v) => Core.call("analysis.set", { block: block, key: modelData.key, value: v })
                    }
                }
            }
        }
        Item { Layout.preferredHeight: 24 }
    }
}
