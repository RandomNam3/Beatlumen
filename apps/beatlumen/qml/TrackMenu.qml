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

    function openFor(trackIds, anchor, extraItems) {
        ids = trackIds
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
}
