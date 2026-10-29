import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// Context menu for one or more songs.
Menu {
    id: menu
    width: 260
    property var ids: []
    property var extra: []      // [{text, action}] items shown first
    signal addToPlaylist(var ids)

    // Offline copies: server songs that are / are not kept on this device.
    property var toSave: []
    property var toRemove: []

    function openFor(trackIds, anchor, extraItems) {
        ids = trackIds
        const avail = Core.state.offline && Core.state.offline.available === true
        const tracks = avail ? (Core.call("library.tracks", { ids: trackIds }) || []) : []
        toSave = tracks.filter(t => !t.local && !t.offline && String(t.id).indexOf(":") < 0).map(t => t.id)
        toRemove = tracks.filter(t => t.offline).map(t => t.id)
        extra = extraItems || []
        if (anchor) {
            parent = anchor
            x = anchor.width - width
            y = anchor.height
        }
        open()
    }

    Repeater {
        model: menu.extra
        MenuItem {
            text: modelData.text
            onTriggered: modelData.action()
        }
    }
    MenuItem {
        text: Theme.tr("Play")
        icon.source: Theme.icon("play_arrow")
        onTriggered: Core.call("play", { ids: menu.ids, start: 0 })
    }
    MenuItem {
        text: Theme.tr("Play next")
        icon.source: Theme.icon("playlist_play")
        onTriggered: Core.call("enqueue", { ids: menu.ids, next: true })
    }
    MenuItem {
        text: Theme.tr("Add to queue")
        icon.source: Theme.icon("queue_music")
        onTriggered: Core.call("enqueue", { ids: menu.ids })
    }
    MenuItem {
        text: Theme.tr("Add to playlist…")
        icon.source: Theme.icon("playlist_add")
        onTriggered: menu.addToPlaylist(menu.ids)
    }
    MenuItem {
        visible: menu.toSave.length > 0
        height: visible ? implicitHeight : 0
        text: Theme.tr("Keep offline")
        icon.source: Theme.icon("download")
        onTriggered: Core.call("offline.save", { ids: menu.toSave })
    }
    MenuItem {
        visible: menu.toRemove.length > 0
        height: visible ? implicitHeight : 0
        text: Theme.tr("Remove offline copy")
        icon.source: Theme.icon("delete")
        onTriggered: Core.call("offline.remove", { ids: menu.toRemove })
    }
}
