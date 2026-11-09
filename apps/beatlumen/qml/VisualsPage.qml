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

    // Wiping goes through the tabs first.
    property var tabs: null
    function swipe(dir) {
        if (stack.depth > 1)
            return dir < 0 && back()
        const to = tabs ? tabs.currentIndex + dir : -1
        if (to < 0 || to >= tabs.count)
            return false
        tabs.currentIndex = to
        return true
    }
    function back() {
        if (stack.depth > 1) {
            stack.pop()
            return true
        }
        return false
    }

    // Sync measurement: tap along to the beat you hear.
    AppDialog {
        id: tapDialog
        readonly property var tempo: Core.state.tempo || ({})
        anchors.centerIn: Overlay.overlay
        width: Math.min(420, page.width - 32)
        title: Theme.tr("Tap along to the beat")
        standardButtons: Dialog.NoButton
        ColumnLayout {
            anchors.fill: parent
            spacing: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: Theme.tr("Play a song with a clear beat and tap on every beat you hear.")
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                color: Theme.text
                text: (tapDialog.tempo.bpm ? Math.round(tapDialog.tempo.bpm) + " BPM · " : Theme.tr("Listening for the beat…") + " · ")
                      + Theme.tr("%1 of %2 taps").arg(tapDialog.tempo.taps || 0).arg(tapDialog.tempo.needed || 6)
                      + (tapDialog.tempo.measuredMs !== undefined && tapDialog.tempo.measuredMs !== null ? " · " + Math.round(tapDialog.tempo.measuredMs) + " ms" : "")
            }
            AccentButton {
                Layout.fillWidth: true
                Layout.preferredHeight: 96
                text: Theme.tr("Tap")
                highlighted: true
                enabled: !!tapDialog.tempo.bpm
                onPressed: Core.call("sync.tap")
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button { text: Theme.tr("Cancel"); flat: true; onClicked: tapDialog.close() }
                AccentButton {
                    text: Theme.tr("Apply")
                    highlighted: true
                    enabled: tapDialog.tempo.measuredMs !== undefined && tapDialog.tempo.measuredMs !== null
                    onClicked: { Core.call("sync.apply"); tapDialog.close() }
                }
            }
        }
    }

    AppStackView {
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
                Component.onCompleted: page.tabs = tabs
                TabButton { text: Theme.tr("Effects") }
                TabButton { text: Theme.tr("Backgrounds") }
            }
            StackLayout {
                id: tabPages
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabs.currentIndex
                SwitchMotion { stack: tabPages }

                // ---- effects --------------------------------------------------
                ScrollView {
                    // Vertical only: an (invisible) horizontal bar would catch taps
                    // along the bottom edge.
                    ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                    id: fxScroll
                    clip: true
                    contentWidth: availableWidth
                    // Side by side where there is room.
                    GridLayout {
                        readonly property bool wide: fxScroll.availableWidth >= 1040
                        width: wide ? fxScroll.availableWidth : Math.min(fxScroll.availableWidth, 720)
                        columns: wide ? 2 : 1
                        columnSpacing: 24
                        rowSpacing: 8

                        ColumnLayout {
                            Layout.fillWidth: true
                            Layout.preferredWidth: 1
                            Layout.alignment: Qt.AlignTop
                            spacing: 8

                            SwitchRow {
                                Layout.fillWidth: true
                                title: Theme.tr("Epilepsy mode")
                                detail: Theme.tr("Fewer flashes and colour changes. Lowers the risk, cannot rule it out.")
                                on: page.settings.photosensitive === true
                                onSwitched: (v) => Core.call("settings.set", { key: "photosensitive", value: v })
                            }

                            SectionLabel { text: Theme.tr("Animation") }
                            Flow {
                                Layout.fillWidth: true
                                Layout.leftMargin: 8
                                Layout.rightMargin: 8
                                spacing: 8
                                Repeater {
                                    model: [
                                        { v: "", l: Theme.tr("Pictures") },
                                        { v: "flow", l: Theme.tr("Flow") },
                                        { v: "tunnel", l: Theme.tr("Tunnel") },
                                        { v: "stars", l: Theme.tr("Stars") },
                                        { v: "aurora", l: Theme.tr("Aurora") },
                                        { v: "waves", l: Theme.tr("Waves") },
                                        { v: "cycle", l: Theme.tr("A new one per song") }
                                    ]
                                    delegate: AccentButton {
                                        text: modelData.l
                                        checkable: true
                                        checked: (page.settings.animation || "") === modelData.v
                                        highlighted: checked
                                        flat: !checked
                                        onClicked: Core.call("settings.set", { key: "animation", value: modelData.v })
                                    }
                                }
                            }

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
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            Layout.preferredWidth: 1
                            Layout.alignment: Qt.AlignTop
                            spacing: 8

                            SectionLabel { text: Theme.tr("Intensity") }
                            RowLayout {
                                Layout.fillWidth: true
                                Layout.leftMargin: 16
                                Layout.rightMargin: 16
                                Label { textFormat: Text.PlainText; text: Theme.tr("Calm"); color: Theme.textDim }
                                Slider {
                                    Layout.fillWidth: true
                                    from: 0; to: 2; stepSize: 0.05
                                    value: page.settings.intensity !== undefined ? page.settings.intensity : 1
                                    onMoved: Core.call("settings.set", { key: "intensity", value: value })
                                    Accessible.name: Theme.tr("Intensity")
                                }
                                Label { textFormat: Text.PlainText; text: Theme.tr("Wild"); color: Theme.textDim }
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                Layout.leftMargin: 16
                                Layout.rightMargin: 16
                                spacing: 0
                                Label { textFormat: Text.PlainText; text: Theme.tr("Sync with the music"); color: Theme.text }
                                RowLayout {
                                    Layout.fillWidth: true
                                    Label { textFormat: Text.PlainText; text: Theme.tr("Later"); color: Theme.textDim }
                                    Slider {
                                        id: syncSlider
                                        Layout.fillWidth: true
                                        from: -300; to: 300; stepSize: 10
                                        value: page.settings.syncOffsetMs || 0
                                        onMoved: Core.call("settings.set", { key: "syncOffsetMs", value: value })
                                        Accessible.name: Theme.tr("Sync with the music")
                                    }
                                    Label { textFormat: Text.PlainText; text: Theme.tr("Earlier"); color: Theme.textDim }
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    text: (syncSlider.value > 0 ? "+" : "") + Math.round(syncSlider.value) + " ms · " + Theme.tr("Earlier if effects come after the beat (e.g. Bluetooth).")
                                    color: Theme.textFaint
                                    font.pixelSize: 12
                                    wrapMode: Text.WordWrap
                                }
                                Button {
                                    text: Theme.tr("Measure by tapping along…")
                                    flat: true
                                    icon.source: Theme.icon("graphic_eq")
                                    onClicked: { Core.call("sync.reset"); tapDialog.open() }
                                }
                            }
                            SwitchRow {
                                Layout.fillWidth: true
                                title: Theme.tr("Predict the beat")
                                detail: Theme.tr("For live music: effects land on the coming beats.")
                                on: page.settings.beatPrediction !== false
                                onSwitched: (on) => Core.call("settings.set", { key: "beatPrediction", value: on })
                            }
                            SwitchRow {
                                Layout.fillWidth: true
                                title: Theme.tr("Adaptive")
                                detail: Theme.tr("Effects follow the energy of the song.")
                                on: page.settings.adaptive === true
                                onSwitched: (on) => Core.call("settings.set", { key: "adaptive", value: on })
                            }

                            SectionLabel { text: Theme.tr("Overlay") }
                            Flow {
                                Layout.fillWidth: true
                                Layout.leftMargin: 8
                                Layout.rightMargin: 8
                                spacing: 8
                                Repeater {
                                    model: [
                                        { v: "Spectrum", l: Theme.tr("Spectrum") },
                                        { v: "Wave", l: Theme.tr("Frequency wave") },
                                        { v: "Mirror", l: Theme.tr("Mirrored bars") },
                                        { v: "Circle", l: Theme.tr("Circle") },
                                        { v: "Waveform", l: Theme.tr("Waveform") },
                                        { v: "None", l: Theme.tr("Off") }
                                    ]
                                    delegate: AccentButton {
                                        text: modelData.l
                                        checkable: true
                                        checked: page.settings.visualizer === modelData.v
                                        highlighted: checked
                                        flat: !checked
                                        onClicked: Core.call("settings.set", { key: "visualizer", value: modelData.v })
                                    }
                                }
                            }

                            SwitchRow {
                                Layout.fillWidth: true
                                title: Theme.tr("Fill the visual")
                                detail: Theme.tr("Full size instead of a strip below the title.")
                                on: page.settings.overlayFull === true
                                onSwitched: (v) => Core.call("settings.set", { key: "overlayFull", value: v })
                            }

                            ItemDelegate {
                                Accessible.role: Accessible.Button
                                Accessible.name: Theme.tr("Advanced editor")
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
                                        Label { textFormat: Text.PlainText; text: Theme.tr("Advanced editor"); font.pixelSize: 15; font.weight: Font.Medium; color: Theme.text }
                                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Customize every effect of the current style."); color: Theme.textDim; font.pixelSize: 13; wrapMode: Text.WordWrap }
                                    }
                                    Icon { name: "chevron_right"; color: Theme.textFaint }
                                }
                            }
                            Item { Layout.preferredHeight: 16 }
                        }
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
