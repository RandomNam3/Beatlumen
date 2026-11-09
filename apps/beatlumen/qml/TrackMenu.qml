import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// Context menu for one or more songs.
AppMenu {
    id: menu
    width: 260
    property var ids: []
    property var extra: []      // [{text, action}] items shown first
    signal addToPlaylist(var ids)
    signal editLabels(string id)
    property bool canEditLabels: false

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
    // Other devices of the server login (server songs only).
    AppMenu {
        id: playOn
        title: Theme.tr("Play on")
        readonly property var targets: (menu.opened && Core.state.server === "connected")
            ? (Core.call("devices.list") || []).filter(d => !d.self && d.online && d.remote !== false) : []
        enabled: targets.length > 0 && menu.ids.some(id => String(id).indexOf(":") < 0)
        Repeater {
            model: playOn.targets
            MenuItem {
                required property var modelData
                text: modelData.name
                icon.source: Theme.icon("devices")
                onTriggered: Core.call("devices.playOn", { id: modelData.id, ids: menu.ids, start: 0 })
            }
        }
    }
    MenuItem {
        readonly property var marks: menu.ids.filter(id => Favorites.canMark(id))
        readonly property bool all: marks.length > 0 && marks.every(id => Favorites.has(id))
        visible: marks.length > 0
        height: visible ? implicitHeight : 0
        text: all ? Theme.tr("Remove from favorites") : Theme.tr("Add to favorites")
        icon.source: Theme.icon(all ? "favorite" : "favorite_border")
        onTriggered: Favorites.set(marks, !all)
    }
    // Songs of an extension: into the library without playing them.
    MenuItem {
        readonly property var waiting: menu.ids.filter(id => String(id).indexOf("extitem:") === 0)
        visible: waiting.length > 0
        height: visible ? implicitHeight : 0
        text: Theme.tr("Add to library")
        icon.source: Theme.icon("add")
        onTriggered: Core.call("extensions.add", { ids: waiting })
    }
    MenuItem {
        // A song of an extension only has a radio if the extension offers one.
        visible: !menu.ids.some(id => String(id).indexOf("extitem:") === 0)
            || ((Core.state.extensions || {}).list || []).some(e => e.radio === true)
        height: visible ? implicitHeight : 0
        text: Theme.tr("Start radio")
        icon.source: Theme.icon("radio")
        onTriggered: Core.call("songs.radio", { seed: { tracks: menu.ids.slice(0, 50) }, name: "" })
    }
    MenuItem {
        visible: menu.canEditLabels && menu.ids.length === 1
        height: visible ? implicitHeight : 0
        text: Theme.tr("Labels…")
        icon.source: Theme.icon("label")
        onTriggered: menu.editLabels(menu.ids[0])
    }
    MenuItem {
        // A song of an extension is not in the library yet.
        visible: menu.ids.every(id => String(id).indexOf("extitem:") !== 0)
        height: visible ? implicitHeight : 0
        text: Theme.tr("Add to playlist…")
        icon.source: Theme.icon("playlist_add")
        onTriggered: menu.addToPlaylist(menu.ids)
    }
    // Out of this account's library; the file stays for the others.
    MenuItem {
        readonly property var songs: Core.state.server === "connected" ? menu.ids.filter(id => String(id).indexOf(":") < 0) : []
        visible: songs.length > 0
        height: visible ? implicitHeight : 0
        text: Theme.tr("Remove from library")
        icon.source: Theme.icon("delete")
        onTriggered: Core.call("library.remove", { ids: songs })
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
