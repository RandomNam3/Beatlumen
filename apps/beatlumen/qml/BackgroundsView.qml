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
    // Folders that stay in sync with the list.
    readonly property var folders: Core.state.backgroundFolders || []
    function loadItems() {
        items = Core.call("backgrounds.list") || []
        // The list can change while selecting (a synced folder): what is
        // gone leaves the selection.
        if (selecting) {
            const next = {}
            for (const it of items)
                if (selected[it.source])
                    next[it.source] = true
            selected = next
            selectedCount = Object.keys(next).length
            if (items.length === 0)
                clearSelection()
        }
    }
    onRevChanged: loadItems()
    onImagesRevChanged: loadImages()
    onVaultOpenChanged: loadImages()
    // Opening the list looks for changes in the synced folders.
    onVisibleChanged: if (visible) Core.call("backgrounds.sync")
    Component.onCompleted: { loadItems(); loadImages(); Core.call("backgrounds.sync") }
    Component.onDestruction: if (selecting) clearSelection()

    // Selection mode (a long press or the select button): taps select,
    // and the selected images can be removed from the list together.
    property bool selecting: false
    property var selected: ({})
    property int selectedCount: 0
    function setSelected(next) {
        selected = next
        selectedCount = Object.keys(next).length
    }
    function startSelection() {
        selecting = true
        const win = ApplicationWindow.window
        if (win && "selectionOwner" in win)
            win.selectionOwner = view
    }
    // Also called by the window when going back.
    function clearSelection() {
        setSelected({})
        selecting = false
        const win = ApplicationWindow.window
        if (win && "selectionOwner" in win && win.selectionOwner === view)
            win.selectionOwner = null
    }
    function toggle(source) {
        const next = Object.assign({}, selected)
        if (next[source])
            delete next[source]
        else
            next[source] = true
        setSelected(next)
    }
    function selectAll() {
        const next = {}
        for (const it of items)
            next[it.source] = true
        setSelected(next)
    }
    readonly property bool allSelected: items.length > 0 && selectedCount === items.length
    function selectedSources() {
        return items.filter(it => selected[it.source]).map(it => it.source)
    }

    component Thumb: AbstractButton {
        id: t
        property string thumb
        property string label
        property bool current: false
        property bool selecting: false
        property bool selected: false
        implicitHeight: width * 0.62
        Accessible.name: selected ? label + ", " + Theme.tr("Selected") : label
        background: Rectangle {
            radius: Theme.smallRadius
            color: Theme.surfaceVariant
            border.width: t.current ? 3 : 0
            border.color: Theme.accent
            clip: true
            Image {
                anchors.fill: parent
                anchors.margins: t.current ? 3 : 0
                source: Covers.safe(t.thumb)
                fillMode: Image.PreserveAspectCrop
                asynchronous: true
                sourceSize: Qt.size(320, 200)
                // The core keeps thumbnails of local files (and makes them
                // anew when a file changes); Qt's cache would show the old one.
                cache: !t.thumb.startsWith("image://")
                opacity: t.selecting && !t.selected ? 0.6 : 1
            }
            Icon { anchors.centerIn: parent; visible: t.thumb === ""; name: "wallpaper"; color: Theme.textFaint }
            // The check mark of the selection mode.
            Rectangle {
                visible: t.selecting
                anchors.top: parent.top
                anchors.right: parent.right
                anchors.margins: 6
                width: 26
                height: 26
                radius: 13
                color: t.selected ? Theme.accent : Qt.rgba(0, 0, 0, 0.45)
                border.width: 2
                border.color: "white"
                Icon {
                    anchors.centerIn: parent
                    visible: t.selected
                    name: "check"
                    size: 18
                    color: "white"
                }
            }
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
            // The icon buttons wrap together, not one by one.
            Row {
                IconButton { iconName: "skip_previous"; tip: Theme.tr("Previous"); enabled: view.items.length > 1; onClicked: Core.call("backgrounds.prev") }
                IconButton { iconName: "skip_next"; tip: Theme.tr("Next"); enabled: view.items.length > 1; onClicked: Core.call("backgrounds.next") }
                IconButton { iconName: "shuffle"; tip: Theme.tr("Shuffle"); active: Core.state.backgroundShuffle === true; onClicked: Core.call("backgrounds.shuffle") }
                IconButton { iconName: "check"; tip: Theme.tr("Select"); enabled: view.items.length > 0; active: view.selecting; onClicked: view.selecting ? view.clearSelection() : view.startSelection() }
            }
        }

        // What to do with the selected images.
        Rectangle {
            id: selectionBar
            visible: view.selecting
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            implicitHeight: Theme.touch + 12
            radius: Theme.radius
            color: Theme.surfaceStrong
            border.width: 1
            border.color: Theme.outline
            Accessible.role: Accessible.ToolBar
            Accessible.name: selectionCount.text
            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 4
                anchors.rightMargin: 4
                spacing: 2
                IconButton {
                    iconName: "close"
                    tip: Theme.tr("Done")
                    onClicked: view.clearSelection()
                }
                Label {
                    id: selectionCount
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: view.selectedCount > 0 ? Theme.tr("%1 selected").arg(view.selectedCount) : Theme.tr("Tap images to select them")
                    color: Theme.text
                    elide: Text.ElideRight
                }
                IconButton {
                    iconName: "check"
                    active: view.allSelected
                    tip: view.allSelected ? Theme.tr("Clear selection") : Theme.tr("Select all")
                    onClicked: view.allSelected ? view.setSelected({}) : view.selectAll()
                }
                IconButton {
                    iconName: "delete"
                    tip: Theme.tr("Remove from the list")
                    enabled: view.selectedCount > 0
                    onClicked: confirmRemove.ask()
                }
            }
        }

        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            visible: view.items.length === 0
            text: Theme.tr("No backgrounds yet; the album cover is used.")
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
                    selecting: view.selecting
                    selected: view.selected[modelData.source] === true
                    onClicked: view.selecting ? view.toggle(modelData.source) : Core.call("backgrounds.select", { pos: modelData.pos })
                    // A long press starts the selection mode; it removes nothing.
                    onPressAndHold: {
                        if (!view.selecting)
                            view.startSelection()
                        view.toggle(modelData.source)
                    }
                    ToolTip.text: modelData.label
                    ToolTip.visible: hovered && !Core.mobile
                }
            }
        }

        // Synced folders: new images join the list, deleted ones leave it.
        SectionLabel { text: Theme.tr("Synced folders"); visible: view.folders.length > 0 }
        ColumnLayout {
            visible: view.folders.length > 0
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 8
            spacing: 0
            Repeater {
                model: view.folders
                delegate: RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    Icon { name: "folder"; color: Theme.textDim; Layout.leftMargin: 8 }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.name; color: Theme.text; elide: Text.ElideRight }
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.path; color: Theme.textDim; font.pixelSize: 13; elide: Text.ElideMiddle }
                    }
                    IconButton {
                        iconName: "close"
                        tip: Theme.tr("Remove folder")
                        onClicked: { confirmFolder.path = modelData.path; confirmFolder.name = modelData.name; confirmFolder.open() }
                    }
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

        AppDialog {
            id: confirmRemove
            anchors.centerIn: Overlay.overlay
            width: Math.min(420, (parent ? parent.width : 420) - 32)
            // Set when asked (a binding to the count loops the dialog's height).
            function ask() {
                title = view.selectedCount === 1 ? Theme.tr("Remove the image from the list?") : Theme.tr("Remove %1 images from the list?").arg(view.selectedCount)
                open()
            }
            standardButtons: Dialog.Cancel | Dialog.Ok
            onAboutToShow: Theme.nameButtons(this)
            Label {
                textFormat: Text.PlainText
                width: parent.width
                wrapMode: Text.WordWrap
                text: Theme.tr("The files stay where they are. Images from a synced folder do not come back on their own.")
            }
            onAccepted: {
                Core.call("backgrounds.remove", { sources: view.selectedSources() })
                view.clearSelection()
            }
        }
        AppDialog {
            id: confirmFolder
            property string path
            property string name
            anchors.centerIn: Overlay.overlay
            width: Math.min(420, (parent ? parent.width : 420) - 32)
            title: Theme.tr("Remove the folder \"%1\"?").arg(name)
            standardButtons: Dialog.Cancel | Dialog.Ok
            onAboutToShow: Theme.nameButtons(this)
            Label {
                textFormat: Text.PlainText
                width: parent.width
                wrapMode: Text.WordWrap
                text: Theme.tr("Its images leave the list and changes in it are no longer followed. The files stay where they are.")
            }
            onAccepted: Core.call("backgrounds.removeFolder", { path: path })
        }
    }
}
