import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Background images shown behind the effects.
ScrollView {
    // Vertical only: an (invisible) horizontal bar would catch taps
    // along the bottom edge.
    ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
    id: view
    signal addImages()
    signal addImageFolder()
    clip: true
    contentWidth: availableWidth
    readonly property var settings: Core.state.settings || ({})
    readonly property int rev: Core.state.backgroundsRev || 0
    readonly property int imagesRev: Core.state.imagesRev || 0
    property var items: []
    property var serverImages: []
    // Images from the vault (decrypted thumbnails, in memory only).
    property var vaultImages: []
    readonly property bool vaultOpen: Core.state.vault && Core.state.vault.state === "ready"
    function loadImages() {
        serverImages = Core.call("images.list") || []
        vaultImages = vaultOpen ? (Core.call("vault.images") || []) : []
    }
    onRevChanged: items = Core.call("backgrounds.list") || []
    onImagesRevChanged: loadImages()
    onVaultOpenChanged: loadImages()
    Component.onCompleted: { items = Core.call("backgrounds.list") || []; loadImages() }

    component Thumb: AbstractButton {
        id: t
        property string thumb
        property string label
        property bool current: false
        implicitHeight: width * 0.62
        background: Rectangle {
            radius: Theme.smallRadius
            color: Theme.surfaceVariant
            border.width: t.current ? 3 : 0
            border.color: Theme.accent
            clip: true
            Image {
                anchors.fill: parent
                anchors.margins: t.current ? 3 : 0
                source: t.thumb
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                sourceSize: Qt.size(320, 200)
            }
            Icon { anchors.centerIn: parent; visible: t.thumb === ""; name: "wallpaper"; color: Theme.textFaint }
        }
    }

    ColumnLayout {
        width: view.availableWidth
        spacing: 12

        Flow {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            spacing: 8
            AccentButton { text: Theme.tr("Add images"); icon.source: Theme.icon("image"); highlighted: true; onClicked: view.addImages() }
            Button { text: Theme.tr("Add folder"); icon.source: Theme.icon("create_new_folder"); visible: Core.platform !== "web"; onClicked: view.addImageFolder() }
            IconButton { iconName: "skip_previous"; tip: Theme.tr("Previous"); enabled: view.items.length > 1; onClicked: Core.call("backgrounds.prev") }
            IconButton { iconName: "skip_next"; tip: Theme.tr("Next"); enabled: view.items.length > 1; onClicked: Core.call("backgrounds.next") }
            IconButton { iconName: "shuffle"; tip: Theme.tr("Shuffle"); active: Core.state.backgroundShuffle === true; onClicked: Core.call("backgrounds.shuffle") }
            IconButton { iconName: "delete"; tip: Theme.tr("Clear"); enabled: view.items.length > 0; onClicked: Core.call("backgrounds.clear") }
        }

        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            visible: view.items.length === 0
            text: Theme.tr("No backgrounds yet. The album cover of the current song is used.")
            wrapMode: Text.WordWrap
            color: Theme.textDim
        }

        GridLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            columns: Math.max(2, Math.floor(width / 150))
            columnSpacing: 8
            rowSpacing: 8
            Repeater {
                model: view.items
                delegate: Thumb {
                    Layout.fillWidth: true
                    thumb: modelData.thumb
                    label: modelData.label
                    current: modelData.current
                    onClicked: Core.call("backgrounds.select", { pos: modelData.pos })
                    onPressAndHold: Core.call("backgrounds.remove", { pos: modelData.pos })
                    ToolTip.text: modelData.label
                    ToolTip.visible: hovered && !Core.mobile
                }
            }
        }

        SectionLabel { text: Theme.tr("Change background") }
        ColumnLayout {
            Layout.leftMargin: 8
            spacing: 0
            Repeater {
                model: [{ v: "Manual", l: Theme.tr("Manually") }, { v: "OnTrackFinish", l: Theme.tr("When a song ends") }, { v: "OnTrackChange", l: Theme.tr("On every song change") }]
                delegate: RadioButton {
                    text: modelData.l
                    checked: view.settings.backgroundMode === modelData.v
                    onClicked: Core.call("settings.set", { key: "backgroundMode", value: modelData.v })
                }
            }
        }
        SwitchRow {
            Layout.fillWidth: true
            title: Theme.tr("Use album cover when empty")
            on: view.settings.coverFallback !== false
            onSwitched: (on) => Core.call("settings.set", { key: "coverFallback", value: on })
        }

        SectionLabel { text: Theme.tr("Server images"); visible: view.serverImages.length > 0 }
        GridLayout {
            visible: view.serverImages.length > 0
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            columns: Math.max(2, Math.floor(width / 150))
            columnSpacing: 8
            rowSpacing: 8
            Repeater {
                model: view.serverImages
                delegate: Thumb {
                    Layout.fillWidth: true
                    thumb: modelData.thumb
                    label: modelData.name
                    onClicked: Core.call("backgrounds.add", { sources: [modelData.source], show: true })
                }
            }
        }
        SectionLabel { text: Theme.tr("Encrypted images"); visible: view.vaultImages.length > 0 }
        GridLayout {
            visible: view.vaultImages.length > 0
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            columns: Math.max(2, Math.floor(width / 150))
            columnSpacing: 8
            rowSpacing: 8
            Repeater {
                model: view.vaultImages
                delegate: Thumb {
                    Layout.fillWidth: true
                    thumb: modelData.thumb
                    label: modelData.name
                    onClicked: Core.call("backgrounds.add", { sources: [modelData.source], show: true })
                    ToolTip.text: modelData.name
                    ToolTip.visible: hovered && !Core.mobile
                }
            }
        }
        Item { Layout.preferredHeight: 16 }
    }
}
