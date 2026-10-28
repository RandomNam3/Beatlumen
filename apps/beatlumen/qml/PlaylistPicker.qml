import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Adds songs to an existing or a new playlist.
Dialog {
    id: dlg
    property var ids: []
    property var playlists: []
    title: Theme.tr("Add to playlist…")
    modal: true
    anchors.centerIn: Overlay.overlay
    width: Math.min(420, (parent ? parent.width : 420) - 32)
    height: Math.min(520, (parent ? parent.height : 520) - 64)
    standardButtons: Dialog.Cancel

    function show(trackIds) {
        ids = trackIds
        playlists = (Core.call("playlists.list") || []).filter(p => p.kind === "track")
        open()
    }

    ListView {
        anchors.fill: parent
        clip: true
        model: [{ id: "", name: Theme.tr("New playlist"), count: -1 }].concat(dlg.playlists)
        delegate: ItemDelegate {
            width: ListView.view.width
            height: Theme.row - 8
            // Names may come from a server: shown as plain text.
            contentItem: RowLayout {
                spacing: 16
                Icon { name: modelData.id === "" ? "add" : "queue_music"; color: Theme.text }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: modelData.name + (modelData.count >= 0 ? "  ·  " + modelData.count : "")
                    elide: Text.ElideRight
                    color: Theme.text
                }
            }
            onClicked: {
                if (modelData.id === "") {
                    newPrompt.open()
                } else {
                    Core.call("playlist.add", { id: modelData.id, items: dlg.ids })
                    dlg.close()
                }
            }
        }
    }
    TextPrompt {
        id: newPrompt
        title: Theme.tr("New playlist")
        placeholder: Theme.tr("Playlist name")
        acceptText: Theme.tr("Create")
        onSubmitted: (t) => {
            Core.call("playlist.create", { name: t, kind: "track", items: dlg.ids, server: Core.state.server === "connected" })
            dlg.close()
        }
    }
}
