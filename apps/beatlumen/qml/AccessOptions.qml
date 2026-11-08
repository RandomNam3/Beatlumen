import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Options for accessibility, in the setup and in the settings.
ColumnLayout {
    id: options
    property bool showEpilepsy: true
    readonly property var settings: Core.state.settings || ({})
    readonly property var ui: settings.ui || ({})
    spacing: 4

    SwitchRow {
        Layout.fillWidth: true
        visible: options.showEpilepsy
        title: Theme.tr("Epilepsy mode")
        detail: Theme.tr("Fewer flashes and colour changes. Lowers the risk, cannot rule it out.")
        on: options.settings.photosensitive === true
        onSwitched: (v) => Core.call("settings.set", { key: "photosensitive", value: v })
    }
    SwitchRow {
        Layout.fillWidth: true
        title: Theme.tr("Reduce motion")
        detail: Theme.tr("Less sliding and fading in the interface.")
        on: options.ui.reduceMotion === true
        onSwitched: (v) => Core.call("settings.set", { key: "ui.reduceMotion", value: v })
    }
    SwitchRow {
        Layout.fillWidth: true
        title: Theme.tr("High contrast")
        detail: Theme.tr("Stronger text and solid backgrounds.")
        on: options.ui.highContrast === true
        onSwitched: (v) => Core.call("settings.set", { key: "ui.highContrast", value: v })
    }
    SwitchRow {
        Layout.fillWidth: true
        title: Theme.tr("Keep the controls visible")
        detail: Theme.tr("Controls stay visible during playback.")
        on: options.ui.keepControls === true
        onSwitched: (v) => Core.call("settings.set", { key: "ui.keepControls", value: v })
    }
    ColumnLayout {
        Layout.fillWidth: true
        Layout.leftMargin: 16
        Layout.rightMargin: 16
        Layout.topMargin: 4
        visible: !Core.mobile
        spacing: 4
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: Theme.tr("Size of everything")
            color: Theme.text
            font.pixelSize: 15
        }
        Flow {
            Layout.fillWidth: true
            spacing: 8
            Repeater {
                model: [1, 1.15, 1.3, 1.5]
                AccentButton {
                    required property var modelData
                    text: Math.round(modelData * 100) + " %"
                    checkable: true
                    checked: (options.ui.uiScale || 1) === modelData
                    highlighted: checked
                    flat: !checked
                    onClicked: {
                        Core.call("settings.set", { key: "ui.uiScale", value: modelData })
                        Core.call("save")
                    }
                }
            }
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: Theme.tr("Takes effect at the next start of the app.")
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
    }
    // Screen readers are recognised by themselves (see Accessibility.java).
    SwitchRow {
        Layout.fillWidth: true
        visible: Core.platform === "android"
        title: Theme.tr("Always support assistive services")
        detail: Theme.tr("Screen readers work without this. Turn it on if another assistant cannot read the app. Takes effect after a restart and can make the app slower.")
        on: Core.accessibilityAlways()
        onSwitched: (v) => { Core.setAccessibilityAlways(v); on = v }
    }
}
