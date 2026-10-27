import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Advanced editor: every effect of the current style plus the audio
// analysis settings.
Item {
    id: ed
    signal back()
    readonly property int presetRev: Core.state.presetRev || 0
    property var effects: []
    property var types: Core.call("effect.types") || []
    function reload() { effects = Core.call("preset.effects") || [] }
    onPresetRevChanged: reload()
    Component.onCompleted: reload()

    TextPrompt {
        id: saveAs
        title: Theme.tr("Save style as…")
        placeholder: Theme.tr("Style name")
        acceptText: Theme.tr("Save")
        onSubmitted: (t) => Core.call("preset.save", { name: t, server: Core.state.server === "connected" })
    }

    StackView {
        id: stack
        anchors.fill: parent
        initialItem: listView
    }

    Component {
        id: listView
        ColumnLayout {
            spacing: 4
            PageHeader {
                Layout.fillWidth: true
                back: true
                title: Core.state.preset || ""
                subtitle: Core.state.presetDirty ? "●" : ""
                onBackClicked: ed.back()
                IconButton { iconName: "undo"; tip: Theme.tr("Undo"); enabled: Core.state.canUndo === true; onClicked: Core.call("preset.undo") }
                IconButton { iconName: "redo"; tip: Theme.tr("Redo"); enabled: Core.state.canRedo === true; onClicked: Core.call("preset.redo") }
                IconButton {
                    iconName: "save"
                    tip: Theme.tr("Save style")
                    active: Core.state.presetDirty === true
                    onClicked: Core.state.presetOrigin === "builtin" ? saveAs.open() : Core.call("preset.save")
                }
                IconButton { iconName: "more_vert"; tip: Theme.tr("More"); onClicked: moreMenu.popup() }
                Menu {
                    id: moreMenu
                    MenuItem { text: Theme.tr("Save style as…"); onTriggered: saveAs.open() }
                }
            }
            TabBar {
                id: tabs
                Layout.fillWidth: true
                background: null
                TabButton { text: Theme.tr("Effects") }
                TabButton { text: Theme.tr("Analysis") }
            }
            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabs.currentIndex
                ListView {
                    clip: true
                    model: ed.effects
                    ScrollBar.vertical: ScrollBar { }
                    footer: Button {
                        x: 8
                        text: Theme.tr("Add effect")
                        icon.source: Theme.icon("add")
                        onClicked: addMenu.popup()
                    }
                    delegate: ItemDelegate {
                        width: ListView.view.width
                        height: Theme.row
                        opacity: modelData.shadowed ? 0.55 : 1
                        onClicked: stack.push(formView, { effectId: modelData.id })
                        contentItem: RowLayout {
                            spacing: 8
                            Switch {
                                checked: modelData.enabled
                                onToggled: Core.call("effect.toggle", { id: modelData.id, enabled: checked })
                                Accessible.name: Theme.tr("Enabled")
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Label { Layout.fillWidth: true; text: modelData.name; font.pixelSize: 15; font.weight: Font.Medium; elide: Text.ElideRight; color: Theme.text }
                                Label { Layout.fillWidth: true; text: modelData.typeLabel + (modelData.trigger ? " · " + modelData.trigger : ""); color: Theme.textDim; font.pixelSize: 13; elide: Text.ElideRight }
                            }
                            IconButton {
                                id: m
                                iconName: "more_vert"
                                tip: Theme.tr("More")
                                iconColor: Theme.textDim
                                onClicked: effectMenu.popup(m)
                                Menu {
                                    id: effectMenu
                                    MenuItem { text: Theme.tr("Duplicate"); onTriggered: Core.call("effect.duplicate", { id: modelData.id }) }
                                    MenuItem { text: Theme.tr("Move up"); onTriggered: Core.call("effect.moveUp", { id: modelData.id }) }
                                    MenuItem { text: Theme.tr("Move down"); onTriggered: Core.call("effect.moveDown", { id: modelData.id }) }
                                    MenuItem { text: Theme.tr("Delete"); onTriggered: Core.call("effect.remove", { id: modelData.id }) }
                                }
                            }
                        }
                    }
                    Menu {
                        id: addMenu
                        Repeater {
                            model: ed.types
                            MenuItem {
                                text: modelData.group + " · " + modelData.label
                                onTriggered: {
                                    const r = Core.call("effect.add", { type: modelData.type, kind: modelData.kind })
                                    if (r && r.id)
                                        stack.push(formView, { effectId: r.id })
                                }
                            }
                        }
                    }
                }
                AnalysisView { }
            }
        }
    }

    Component {
        id: formView
        ColumnLayout {
            id: fv
            property string effectId
            property var form: ({ sections: [] })
            readonly property int rev: Core.state.presetRev || 0
            function reload() { form = Core.call("effect.form", { id: effectId }) || ({ sections: [] }) }
            onRevChanged: reload()
            Component.onCompleted: reload()
            spacing: 4
            PageHeader {
                Layout.fillWidth: true
                back: true
                title: fv.form.name || ""
                subtitle: fv.form.description || ""
                onBackClicked: stack.pop()
                Switch {
                    checked: fv.form.enabled === true
                    onToggled: Core.call("effect.toggle", { id: fv.effectId, enabled: checked })
                }
            }
            ScrollView {
                id: sv
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                contentWidth: availableWidth
                ColumnLayout {
                    width: sv.availableWidth
                    spacing: 6
                    Repeater {
                        model: fv.form.sections
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
                                    onChanged: (v) => Core.call("effect.set", { id: fv.effectId, block: block, key: modelData.key, value: v })
                                }
                            }
                        }
                    }
                    Item { Layout.preferredHeight: 24 }
                }
            }
        }
    }
}
