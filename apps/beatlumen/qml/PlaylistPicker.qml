import QtQuick
import QtQuick.Controls
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
        playlists = (Core.call("playlists.list") || []).filter(p => p.kind === "Track")
        open()
    }

    ListView {
        anchors.fill: parent
        clip: true
        model: [{ id: "", name: Theme.tr("New playlist"), count: -1 }].concat(dlg.playlists)
        delegate: ItemDelegate {
            width: ListView.view.width
            height: Theme.row - 8
            text: modelData.name + (modelData.count >= 0 ? "  ·  " + modelData.count : "")
            icon.source: Theme.icon(modelData.id === "" ? "add" : "queue_music")
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
            Core.call("playlist.create", { name: t, kind: "Track", items: dlg.ids, server: Core.state.server === "connected" })
            dlg.close()
        }
    }
}
