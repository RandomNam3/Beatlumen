import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

// Editor row for one schema field (see the core's `effect.form`).
ColumnLayout {
    id: fc
    property var field: ({})
    signal changed(var value)
    spacing: 0

    readonly property bool bounded: field.min !== null && field.max !== null && field.min !== undefined && field.max !== undefined && (field.max - field.min) <= 1000
    readonly property bool numeric: field.kind === "number" || field.kind === "int"

    function fmt(v) {
        if (field.kind === "int")
            return Math.round(v) + (field.unit || "")
        const s = Number(v).toFixed(3).replace(/\.?0+$/, "")
        return (s === "-0" ? "0" : s) + (field.unit || "")
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 8
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: fc.field.label || ""
            color: Theme.text
            elide: Text.ElideRight
        }
        Label {
            textFormat: Text.PlainText
            visible: fc.numeric
            text: fc.fmt(slider.pressed ? slider.value : fc.field.value)
            color: Theme.textDim
            font.features: { "tnum": 1 }
        }
        // Unbounded numbers: stepper buttons.
        IconButton {
            visible: fc.numeric && !fc.bounded
            iconName: "keyboard_arrow_down"
            onClicked: fc.changed(Math.max(fc.field.min !== null ? fc.field.min : -1e9, fc.field.value - fc.field.step))
        }
        IconButton {
            visible: fc.numeric && !fc.bounded
            iconName: "keyboard_arrow_up"
            onClicked: fc.changed(Math.min(fc.field.max !== null ? fc.field.max : 1e9, fc.field.value + fc.field.step))
        }
        ComboBox {
            visible: fc.field.kind === "enum" || fc.field.kind === "intenum"
            Layout.preferredWidth: 180
            model: fc.field.options || []
            textRole: "label"
            valueRole: "value"
            currentIndex: (fc.field.options || []).findIndex(o => o.value === fc.field.value)
            onActivated: (i) => fc.changed(fc.field.options[i].value)
        }
    }
    Slider {
        id: slider
        visible: fc.numeric && fc.bounded
        Layout.fillWidth: true
        Layout.leftMargin: 8
        Layout.rightMargin: 8
        from: (fc.field.min !== null && fc.field.min !== undefined) ? fc.field.min : 0
        to: (fc.field.max !== null && fc.field.max !== undefined) ? fc.field.max : 1
        stepSize: fc.field.step || 0.01
        value: fc.field.value || 0
        onMoved: fc.changed(fc.field.kind === "int" ? Math.round(value) : value)
        Accessible.name: fc.field.label || ""
    }
    Flow {
        visible: fc.field.kind === "color"
        Layout.fillWidth: true
        Layout.leftMargin: 16
        spacing: 8
        Repeater {
            model: [0xffffff, 0xfff1d6, 0xffd1e8, 0xd6e8ff, 0xc9ffe5, 0xffb38a, 0xa98bff, 0x66d9ff, 0xff5c8a, 0xffe066]
            delegate: AbstractButton {
                width: 36; height: 36
                onClicked: fc.changed(modelData)
                background: Rectangle {
                    radius: 18
                    color: "#" + ("000000" + modelData.toString(16)).slice(-6)
                    border.width: fc.field.value === modelData ? 3 : 1
                    border.color: fc.field.value === modelData ? Theme.accent : Theme.outline
                }
            }
        }
    }
}
