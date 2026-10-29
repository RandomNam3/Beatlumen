import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// In-app file and folder browser: where the system has no file dialog
// (Android needs real paths for library scanning; Linux without the XDG
// desktop portal).
AppDialog {
    id: dlg
    // "folder", "file" (one file) or "files" (several).
    property string mode: "folder"
    // Extensions of the files shown (lowercase, "*": all files).
    property var extensions: ["*"]
    // Folder mode: images count as content (only matters for listing).
    property bool images: false
    property string acceptText: mode === "folder" ? Theme.tr("Select folder") : Theme.tr("Open")
    property string path: ""
    property var entries: []
    property var roots: []
    property var selected: []
    readonly property bool pickFiles: mode !== "folder"
    // Icon of the files shown.
    property string fileIcon: "music_note"
    signal picked(var paths)

    title: mode === "folder" ? Theme.tr("Choose folder") : Theme.tr("Open files")
    modal: true
    // Always sized against the window (the dialog may be declared inside a
    // zero-sized helper item).
    parent: Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(560, (parent ? parent.width : 560) - 24)
    height: Math.min(680, (parent ? parent.height : 680) - 48)
    padding: 0
    standardButtons: Dialog.NoButton

    function show() {
        if (Core.platform === "android")
            Core.requestMediaPermission()
        roots = Core.call("fs.roots") || []
        selected = []
        // Files: start in the first place (the home folder) with its content.
        if (pickFiles && roots.length > 0)
            go(roots[0].path)
        else {
            path = ""
            entries = []
        }
        open()
    }
    function go(p) {
        path = p
        const args = { path: p, images: images }
        if (pickFiles)
            args.exts = extensions
        const r = Core.call("fs.list", args)
        entries = Array.isArray(r) ? r.filter(e => e.dir || pickFiles) : []
        list.positionViewAtBeginning()
    }
    function up() {
        const parentPath = Core.call("fs.parent", { path: path })
        const isRoot = roots.some(r => r.path === path)
        if (isRoot || !parentPath || parentPath === path) { path = ""; entries = [] } else go(parentPath)
    }
    function toggle(p) {
        const s = selected.slice()
        const i = s.indexOf(p)
        if (i >= 0) s.splice(i, 1); else s.push(p)
        selected = s
    }
    function accept() {
        const paths = mode === "folder" ? [path] : selected
        if (paths.length === 0 || paths[0] === "")
            return
        close()
        picked(paths)
    }
    // The path from the place it is in ("Home / Documents").
    function displayPath(p) {
        let best = null
        for (const r of roots)
            if ((p === r.path || p.startsWith(r.path.endsWith("/") || r.path.endsWith("\\") ? r.path : r.path + "/")
                 || p.startsWith(r.path + "\\")) && (!best || r.path.length > best.path.length))
                best = r
        if (!best || best.path === "/")
            return p
        const rest = p.substring(best.path.length).replace(/^[\/\\]+/, "").split(/[\/\\]/).filter(x => x !== "")
        return [Theme.tr(best.label)].concat(rest).join("  ›  ")
    }
    function sizeText(n) {
        if (n >= 1048576) return (n / 1048576).toFixed(1) + " MB"
        if (n >= 1024) return Math.round(n / 1024) + " KB"
        return n + " B"
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Places, as chips.
        Flow {
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            Layout.topMargin: 4
            spacing: 8
            Repeater {
                model: dlg.roots
                delegate: Rectangle {
                    required property var modelData
                    readonly property bool current: dlg.path === modelData.path
                    height: 32
                    width: chipRow.implicitWidth + 24
                    radius: 16
                    color: current ? Qt.rgba(Theme.accent.r, Theme.accent.g, Theme.accent.b, 0.22)
                         : chipArea.containsMouse ? Theme.surfaceVariant : "transparent"
                    border.width: 1
                    border.color: current ? Theme.accent : Theme.outline
                    RowLayout {
                        id: chipRow
                        anchors.centerIn: parent
                        spacing: 6
                        Icon {
                            name: modelData.path === dlg.roots[0].path ? "folder_open" : (modelData.label.startsWith("/") || modelData.label.endsWith(":\\") ? "storage" : "folder")
                            size: 18
                            color: current ? Theme.accent : Theme.textDim
                        }
                        Label {
                            textFormat: Text.PlainText
                            text: Theme.tr(modelData.label)
                            color: current ? Theme.text : Theme.textDim
                            font.pixelSize: 13
                        }
                    }
                    MouseArea {
                        id: chipArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: dlg.go(modelData.path)
                    }
                    Accessible.role: Accessible.Button
                    Accessible.name: Theme.tr(modelData.label)
                }
            }
        }

        // Where we are.
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.rightMargin: 16
            Layout.topMargin: 8
            visible: dlg.path !== ""
            IconButton {
                iconName: "arrow_back"
                tip: Theme.tr("Back")
                onClicked: dlg.up()
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: dlg.displayPath(dlg.path)
                elide: Text.ElideLeft
                color: Theme.textDim
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.outline; Layout.topMargin: 4 }

        ListView {
            id: list
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: dlg.path === "" ? dlg.roots : dlg.entries
            ScrollBar.vertical: ScrollBar { }
            delegate: ItemDelegate {
                id: row
                required property var modelData
                readonly property bool isDir: dlg.path === "" || modelData.dir === true
                readonly property bool isSelected: !isDir && dlg.selected.indexOf(modelData.path) >= 0
                width: ListView.view.width
                height: Theme.row - 8
                highlighted: isSelected
                contentItem: RowLayout {
                    spacing: 16
                    Icon {
                        name: dlg.path === "" ? "storage" : row.isDir ? "folder" : row.isSelected ? "check" : dlg.fileIcon
                        color: row.isSelected ? Theme.accent : row.isDir ? Theme.text : Theme.textDim
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: dlg.path === "" ? Theme.tr(row.modelData.label) : row.modelData.name
                        elide: Text.ElideMiddle
                        color: Theme.text
                    }
                    Label {
                        visible: !row.isDir
                        textFormat: Text.PlainText
                        text: dlg.sizeText(row.modelData.size || 0)
                        color: Theme.textDim
                        font.pixelSize: 12
                    }
                    Icon { visible: row.isDir; name: "chevron_right"; color: Theme.textDim; size: 20 }
                }
                Accessible.name: dlg.path === "" ? Theme.tr(modelData.label) : modelData.name
                onClicked: {
                    if (isDir)
                        dlg.go(modelData.path)
                    else if (dlg.mode === "file") {
                        dlg.selected = [modelData.path]
                        dlg.accept()
                    } else
                        dlg.toggle(modelData.path)
                }
                onDoubleClicked: {
                    if (!isDir && dlg.mode === "files") {
                        dlg.selected = [modelData.path]
                        dlg.accept()
                    }
                }
            }
            Label {
                anchors.centerIn: parent
                visible: dlg.path !== "" && dlg.entries.length === 0
                textFormat: Text.PlainText
                text: dlg.pickFiles ? Theme.tr("No matching files here") : Theme.tr("No folders here")
                color: Theme.textDim
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.outline }
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            Layout.leftMargin: 20
            Label {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                visible: dlg.mode === "files"
                text: dlg.selected.length > 0 ? Theme.tr("%1 selected").arg(dlg.selected.length) : ""
                color: Theme.textDim
            }
            Item { Layout.fillWidth: true; visible: dlg.mode !== "files" }
            Button { text: Theme.tr("Cancel"); flat: true; onClicked: dlg.close() }
            AccentButton {
                visible: dlg.mode !== "file"
                text: dlg.acceptText
                highlighted: true
                enabled: dlg.mode === "folder" ? dlg.path !== "" : dlg.selected.length > 0
                onClicked: dlg.accept()
            }
        }
    }
}
