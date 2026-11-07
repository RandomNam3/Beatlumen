import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// What was removed: songs of the library and encrypted files, until their time
// is up. A song from one of the server's own folders can always come back.
Item {
    id: view
    property var items: []
    readonly property string rev: [Core.state.libraryRev, (Core.state.vault || {}).rev, Core.state.trashCount].join()

    function reload() {
        const days = (Core.state.vault || {}).trashDays || 29
        const now = Date.now() / 1000
        const songs = (Core.call("library.trash") || []).map(t => ({
            id: t.id, file: false, title: t.title, artist: t.artist, left: t.secondsLeft, lasting: t.lasting === true
        }))
        const files = (Core.call("vault.trash") || []).map(f => ({
            id: f.id, file: true, title: f.title || f.name, artist: f.artist,
            left: days * 86400 - (now - f.trashed), lasting: false
        }))
        items = songs.concat(files)
    }
    function timeLeft(t) {
        if (t.lasting)
            return Theme.tr("Can always come back")
        const days = Math.max(1, Math.ceil(t.left / 86400))
        return days === 1 ? Theme.tr("1 day left") : Theme.tr("%1 days left").arg(days)
    }
    onRevChanged: reload()
    onVisibleChanged: if (visible) reload()
    Component.onCompleted: reload()

    ListView {
        anchors.fill: parent
        visible: view.items.length > 0
        clip: true
        model: view.items
        header: Label {
            textFormat: Text.PlainText
            width: ListView.view.width
            padding: 16
            wrapMode: Text.WordWrap
            color: Theme.textDim
            font.pixelSize: 13
            text: Theme.tr("What you delete stays here for 29 days, then it is gone for you.")
        }
        delegate: ItemDelegate {
            id: row
            required property var modelData
            width: ListView.view.width
            height: 64
            contentItem: RowLayout {
                spacing: 12
                Icon { name: row.modelData.file ? "lock" : "music_note"; size: 22; color: Theme.textDim }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: row.modelData.title || ""; font.pixelSize: 15; elide: Text.ElideRight; color: Theme.text }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: (row.modelData.artist ? row.modelData.artist + " · " : "") + view.timeLeft(row.modelData)
                        font.pixelSize: 13
                        elide: Text.ElideRight
                        color: Theme.textDim
                    }
                }
                IconButton {
                    iconName: "undo"
                    tip: Theme.tr("Restore")
                    onClicked: {
                        if (row.modelData.file)
                            Core.call("vault.restore", { id: row.modelData.id })
                        else
                            Core.call("library.restore", { ids: [row.modelData.id] })
                    }
                }
                IconButton {
                    visible: row.modelData.file
                    iconName: "delete"
                    tip: Theme.tr("Delete for good")
                    onClicked: {
                        forever.fileId = row.modelData.id
                        forever.open()
                    }
                }
            }
        }
    }
    EmptyState {
        anchors.centerIn: parent
        width: Math.min(parent.width - 48, 420)
        visible: view.items.length === 0
        iconName: "delete"
        title: Theme.tr("The trash is empty")
        text: Theme.tr("What you delete stays here for 29 days, then it is gone for you.")
    }

    AppDialog {
        id: forever
        property string fileId
        parent: Overlay.overlay
        anchors.centerIn: parent
        width: Math.min(420, parent ? parent.width - 32 : 420)
        title: Theme.tr("Delete for good?")
        standardButtons: Dialog.Cancel | Dialog.Ok
        Label {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.WordWrap
            text: Theme.tr("You cannot get this file back.")
        }
        onAccepted: Core.call("vault.deleteForever", { id: fileId })
    }
}
