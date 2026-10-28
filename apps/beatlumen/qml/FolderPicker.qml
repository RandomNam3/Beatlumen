import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// In-app folder browser (Android needs real paths for library scanning).
Dialog {
    id: dlg
    property string path: ""
    property var entries: []
    property var roots: []
    property bool images: false
    signal chosen(string path)

    title: Theme.tr("Choose folder")
    modal: true
    // Always sized against the window (the dialog may be declared inside a
    // zero-sized helper item).
    parent: Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(520, (parent ? parent.width : 520) - 24)
    height: Math.min(640, (parent ? parent.height : 640) - 48)
    padding: 0
    standardButtons: Dialog.NoButton

    function show() {
        if (Core.platform === "android")
            Core.requestMediaPermission()
        roots = Core.call("fs.roots") || []
        path = ""
        entries = []
        open()
    }
    function go(p) {
        path = p
        const r = Core.call("fs.list", { path: p, images: images })
        entries = Array.isArray(r) ? r.filter(e => e.dir) : []
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            visible: dlg.path !== ""
            IconButton {
                iconName: "arrow_back"
                tip: Theme.tr("Back")
                onClicked: {
                    const parentPath = Core.call("fs.parent", { path: dlg.path })
                    const isRoot = dlg.roots.some(r => r.path === dlg.path)
                    if (isRoot || !parentPath) { dlg.path = ""; dlg.entries = [] } else dlg.go(parentPath)
                }
            }
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: dlg.path; elide: Text.ElideLeft; color: Theme.textDim }
        }
        ListView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: dlg.path === "" ? dlg.roots : dlg.entries
            ScrollBar.vertical: ScrollBar { }
            delegate: ItemDelegate {
                width: ListView.view.width
                height: Theme.row - 8
                contentItem: RowLayout {
                    spacing: 16
                    Icon { name: dlg.path === "" ? "storage" : "folder"; color: Theme.text }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: modelData.label || modelData.name
                        elide: Text.ElideMiddle
                        color: Theme.text
                    }
                }
                Accessible.name: modelData.label || modelData.name
                onClicked: dlg.go(modelData.path)
            }
        }
        RowLayout {
            Layout.alignment: Qt.AlignRight
            Layout.margins: 12
            Button { text: Theme.tr("Cancel"); flat: true; onClicked: dlg.close() }
            AccentButton {
                text: Theme.tr("Select folder")
                highlighted: true
                enabled: dlg.path !== ""
                onClicked: { dlg.chosen(dlg.path); dlg.close() }
            }
        }
    }
}
