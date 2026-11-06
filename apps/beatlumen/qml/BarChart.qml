import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A titled bar chart of one series (seconds listened): thin bars on a common
// base, a tooltip per bar, a few labels below. The same numbers can be read as
// a table, which is also what a screen reader gets.
ColumnLayout {
    id: chart
    property string title
    property var values: []
    property var labels: []
    // Show a label below every n-th bar.
    property int labelEvery: 1
    property int barHeight: 110
    property bool asTable: false
    readonly property real peak: Math.max(1, ...values)

    spacing: 6
    Layout.fillWidth: true

    function summary() {
        return values.map((v, i) => labels[i] + ": " + Theme.fmtDuration(v)).join(", ")
    }

    RowLayout {
        Layout.fillWidth: true
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: chart.title
            color: Theme.text
            font.pixelSize: 15
            font.weight: Font.DemiBold
            elide: Text.ElideRight
        }
        Button {
            flat: true
            text: chart.asTable ? Theme.tr("Show as chart") : Theme.tr("Show as table")
            font.pixelSize: 12
            onClicked: chart.asTable = !chart.asTable
        }
    }

    // ---- chart --------------------------------------------------------------
    Item {
        id: plot
        visible: !chart.asTable
        Layout.fillWidth: true
        Layout.preferredHeight: chart.barHeight + 22
        Accessible.role: Accessible.Chart
        Accessible.name: chart.title
        Accessible.description: chart.summary()

        // Quiet base line.
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            y: chart.barHeight
            height: 1
            color: Theme.outline
        }
        Row {
            id: bars
            anchors.fill: parent
            spacing: 2
            readonly property real barWidth: chart.values.length > 0
                ? Math.max(2, (width - (chart.values.length - 1) * spacing) / chart.values.length) : 0
            Repeater {
                model: chart.values
                Item {
                    id: slot
                    required property var modelData
                    required property int index
                    width: bars.barWidth
                    height: bars.height
                    // Thin bars, rounded at the data end, standing on the base.
                    Rectangle {
                        anchors.horizontalCenter: parent.horizontalCenter
                        width: Math.min(parent.width, 18)
                        height: slot.modelData > 0 ? Math.max(3, chart.barHeight * slot.modelData / chart.peak) : 0
                        y: chart.barHeight - height
                        radius: Math.min(4, width / 2)
                        color: hover.hovered ? Qt.lighter(Theme.accent, 1.2) : Theme.accent
                        // Square feet on the base line.
                        Rectangle {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.bottom: parent.bottom
                            height: Math.min(parent.height, parent.radius)
                            color: parent.color
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        anchors.horizontalCenter: parent.horizontalCenter
                        y: chart.barHeight + 4
                        visible: slot.index % chart.labelEvery === 0
                        text: chart.labels[slot.index] || ""
                        color: Theme.textDim
                        font.pixelSize: 11
                    }
                    HoverHandler { id: hover }
                    ToolTip.visible: hover.hovered
                    ToolTip.delay: 0
                    ToolTip.text: (chart.labels[slot.index] || "") + ": " + Theme.fmtDuration(slot.modelData)
                }
            }
        }
    }

    // ---- the same as a table ------------------------------------------------
    GridLayout {
        visible: chart.asTable
        Layout.fillWidth: true
        columns: Math.max(1, Math.floor(chart.width / 180))
        columnSpacing: 16
        rowSpacing: 2
        Repeater {
            model: chart.asTable ? chart.values : []
            RowLayout {
                required property var modelData
                required property int index
                Layout.fillWidth: true
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: chart.labels[parent.index] || ""
                    color: Theme.textDim
                    font.pixelSize: 13
                }
                Label {
                    textFormat: Text.PlainText
                    text: Theme.fmtDuration(parent.modelData)
                    color: Theme.text
                    font.pixelSize: 13
                    font.features: { "tnum": 1 }
                }
            }
        }
    }
}
