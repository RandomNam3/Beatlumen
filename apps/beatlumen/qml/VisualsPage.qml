import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Look and feel of the visualizer: style (preset), intensity, adaptive
// mode, overlay, backgrounds and the advanced effect editor.
Item {
    id: page
    signal addImages()
    signal addImageFolder()
    readonly property var settings: Core.state.settings || ({})
    readonly property int presetsRev: Core.state.presetsRev || 0
    property var presets: []
    function reload() { presets = Core.call("presets.list") || [] }
    onPresetsRevChanged: reload()
    Component.onCompleted: reload()

    function back() {
        if (stack.depth > 1) {
            stack.pop()
            return true
        }
        return false
    }

    StackView {
        id: stack
        anchors.fill: parent
        initialItem: mainView
    }

    Component {
        id: mainView
        ColumnLayout {
            spacing: 8
            PageHeader {
                Layout.fillWidth: true
                title: Theme.tr("Visuals")
            }
            TabBar {
                id: tabs
                Layout.fillWidth: true
                background: null
                TabButton { text: Theme.tr("Effects") }
                TabButton { text: Theme.tr("Backgrounds") }
            }
            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabs.currentIndex

                // ---- effects --------------------------------------------------
                ScrollView {
                    id: fxScroll
                    clip: true
                    contentWidth: availableWidth
                    ColumnLayout {
                        width: fxScroll.availableWidth
                        spacing: 16

                        SectionLabel { text: Theme.tr("Style") }
                        GridLayout {
                            Layout.fillWidth: true
                            Layout.leftMargin: 8
                            Layout.rightMargin: 8
                            columns: Math.max(2, Math.floor(width / 160))
                            columnSpacing: 10
                            rowSpacing: 10
                            Repeater {
                                model: page.presets
                                delegate: PresetCard {
                                    Layout.fillWidth: true
                                    name: modelData.name
                                    origin: modelData.origin
                                    active: modelData.name === Core.state.preset
                                    onClicked: Core.call("preset.load", { name: modelData.name })
                                    onDeleteRequested: Core.call("preset.delete", { name: modelData.name })
                                }
                            }
                        }

                        SectionLabel { text: Theme.tr("Intensity") }
                        RowLayout {
                            Layout.fillWidth: true
                            Layout.leftMargin: 16
                            Layout.rightMargin: 16
                            Label { text: Theme.tr("Calm"); color: Theme.textDim }
                            Slider {
                                Layout.fillWidth: true
                                from: 0; to: 2; stepSize: 0.05
                                value: page.settings.intensity !== undefined ? page.settings.intensity : 1
                                onMoved: Core.call("settings.set", { key: "intensity", value: value })
                                Accessible.name: Theme.tr("Intensity")
                            }
                            Label { text: Theme.tr("Wild"); color: Theme.textDim }
                        }
                        SwitchRow {
                            Layout.fillWidth: true
                            title: Theme.tr("Adaptive")
                            detail: Theme.tr("Effects follow the energy of the song: calm in quiet parts, strong in drops.")
                            on: page.settings.adaptive === true
                            onSwitched: (on) => Core.call("settings.set", { key: "adaptive", value: on })
                        }

                        SectionLabel { text: Theme.tr("Overlay") }
                        RowLayout {
                            Layout.leftMargin: 8
                            spacing: 8
                            Repeater {
                                model: [{ v: "Spectrum", l: Theme.tr("Spectrum") }, { v: "Waveform", l: Theme.tr("Waveform") }, { v: "None", l: Theme.tr("Off") }]
                                delegate: Button {
                                    text: modelData.l
                                    checkable: true
                                    checked: page.settings.visualizer === modelData.v
                                    highlighted: checked
                                    flat: !checked
                                    onClicked: Core.call("settings.set", { key: "visualizer", value: modelData.v })
                                }
                            }
                        }

                        ItemDelegate {
                            Layout.fillWidth: true
                            Layout.topMargin: 8
                            height: Theme.row + 8
                            onClicked: stack.push(advancedView)
                            contentItem: RowLayout {
                                spacing: 12
                                Icon { name: "tune"; color: Theme.accent }
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    Label { text: Theme.tr("Advanced editor"); font.pixelSize: 15; font.weight: Font.Medium; color: Theme.text }
                                    Label { Layout.fillWidth: true; text: Theme.tr("Customize every effect of the current style."); color: Theme.textDim; font.pixelSize: 13; wrapMode: Text.WordWrap }
                                }
                                Icon { name: "chevron_right"; color: Theme.textFaint }
                            }
                        }
                        Item { Layout.preferredHeight: 16 }
                    }
                }

                // ---- backgrounds ----------------------------------------------
                BackgroundsView {
                    onAddImages: page.addImages()
                    onAddImageFolder: page.addImageFolder()
                }
            }
        }
    }

    Component {
        id: advancedView
        EffectsEditor {
            onBack: stack.pop()
        }
    }
}
