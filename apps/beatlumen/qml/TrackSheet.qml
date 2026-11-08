import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// What can be done with one or more songs, as a sheet for fingers: the songs
// on top, then one big row per action. A computer gets TrackMenu instead.
BottomSheet {
    id: sheet
    property var ids: []
    property var extra: []      // [{text, action}] rows shown first
    property var tracks: []
    property bool canEditLabels: false
    signal addToPlaylist(var ids)
    signal editLabels(string id)

    // Offline copies: server songs that are / are not kept on this device.
    property var toSave: []
    property var toRemove: []
    property var devices: []
    property bool showDevices: false

    title: Theme.tr("Song")
    wantedHeight: column.implicitHeight + 8

    readonly property var track: ids.length === 1 && tracks.length === 1 ? tracks[0] : null

    function openFor(trackIds, extraItems) {
        ids = trackIds
        extra = extraItems || []
        const avail = Core.state.offline && Core.state.offline.available === true
        tracks = avail || trackIds.length === 1 ? (Core.call("library.tracks", { ids: trackIds }) || []) : []
        toSave = avail ? tracks.filter(t => !t.local && !t.offline && String(t.id).indexOf(":") < 0).map(t => t.id) : []
        toRemove = avail ? tracks.filter(t => t.offline).map(t => t.id) : []
        devices = Core.state.server === "connected" && trackIds.some(id => String(id).indexOf(":") < 0)
            ? (Core.call("devices.list") || []).filter(d => !d.self && d.online && d.remote !== false) : []
        showDevices = false
        open()
    }

    readonly property var rows: {
        const marks = ids.filter(id => Favorites.canMark(id))
        const all = marks.length > 0 && marks.every(id => Favorites.has(id))
        const r = extra.map(e => ({ icon: e.icon || "", text: e.text, run: e.action }))
        r.push({ icon: "play_arrow", text: Theme.tr("Play"), run: () => Core.call("play", { ids: ids, start: 0 }) })
        r.push({ icon: "playlist_play", text: Theme.tr("Play next"), run: () => Core.call("enqueue", { ids: ids, next: true }) })
        r.push({ icon: "queue_music", text: Theme.tr("Add to queue"), run: () => Core.call("enqueue", { ids: ids }) })
        if (marks.length > 0)
            r.push({ icon: all ? "favorite" : "favorite_border", text: all ? Theme.tr("Remove from favorites") : Theme.tr("Add to favorites"), run: () => Favorites.set(marks, !all) })
        // Songs of an extension are not in the library yet: into it, without playing.
        const waiting = ids.filter(id => String(id).indexOf("extitem:") === 0)
        if (waiting.length > 0)
            r.push({ icon: "add", text: Theme.tr("Add to library"), run: () => Core.call("extensions.add", { ids: waiting }) })
        else
            r.push({ icon: "playlist_add", text: Theme.tr("Add to playlist…"), run: () => sheet.addToPlaylist(ids) })
        // Such a song only has a radio if the extension offers one.
        if (waiting.length === 0 || ((Core.state.extensions || {}).list || []).some(e => e.radio === true))
            r.push({ icon: "radio", text: Theme.tr("Start radio"), run: () => Core.call("songs.radio", { seed: { tracks: ids.slice(0, 50) }, name: "" }) })
        const win = sheet.ApplicationWindow.window
        if (track && track.albumId && win)
            r.push({ icon: "album", text: Theme.tr("Go to album"), run: () => win.showAlbum({ id: track.albumId, title: track.album, artist: track.artist, year: track.year }) })
        if (track && track.artist && win)
            r.push({ icon: "person", text: Theme.tr("Go to artist"), run: () => win.showArtist(track.artist.split(", ")[0]) })
        if (canEditLabels && ids.length === 1)
            r.push({ icon: "label", text: Theme.tr("Labels…"), run: () => sheet.editLabels(ids[0]) })
        const songs = Core.state.server === "connected" ? ids.filter(id => String(id).indexOf(":") < 0) : []
        if (songs.length > 0)
            r.push({ icon: "delete", text: Theme.tr("Remove from library"), run: () => Core.call("library.remove", { ids: songs }) })
        if (toSave.length > 0)
            r.push({ icon: "download", text: Theme.tr("Keep offline"), run: () => Core.call("offline.save", { ids: toSave }) })
        if (toRemove.length > 0)
            r.push({ icon: "delete", text: Theme.tr("Remove offline copy"), run: () => Core.call("offline.remove", { ids: toRemove }) })
        return r
    }

    component Row: ItemDelegate {
        property string iconName
        Layout.fillWidth: true
        implicitHeight: Theme.settingRow
        leftPadding: 20
        icon.source: iconName ? Theme.icon(iconName) : ""
        icon.color: Theme.textDim
        icon.width: 24
        icon.height: 24
        font.pixelSize: 15
        Accessible.name: text
    }

    Flickable {
        anchors.fill: parent
        contentHeight: column.implicitHeight
        boundsBehavior: Flickable.StopAtBounds
        ScrollBar.vertical: ScrollBar { }
        ColumnLayout {
            id: column
            width: parent.width
            spacing: 0
            // The songs.
            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 20
                Layout.rightMargin: 12
                Layout.bottomMargin: 8
                spacing: 12
                CoverImage {
                    visible: sheet.track !== null
                    Layout.preferredWidth: 48
                    Layout.preferredHeight: 48
                    radius: 8
                    source: sheet.track ? Covers.url(sheet.track.id, sheet.track.cover) : ""
                    seed: sheet.track ? sheet.track.album || sheet.track.title || "" : ""
                }
                Icon {
                    visible: sheet.track === null
                    name: "queue_music"
                    size: 32
                    color: Theme.textDim
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: sheet.track ? (sheet.track.title || "") : sheet.ids.length + " " + Theme.tr("songs")
                        color: Theme.text
                        font.pixelSize: 16
                        font.weight: Font.DemiBold
                        elide: Text.ElideRight
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: sheet.track !== null
                        text: sheet.track ? [sheet.track.artist || Theme.tr("Unknown artist"), sheet.track.album].filter(x => x).join(" · ") : ""
                        color: Theme.textDim
                        font.pixelSize: 13
                        elide: Text.ElideRight
                    }
                }
            }
            Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 1
                color: Theme.outline
            }
            Repeater {
                model: sheet.rows
                Row {
                    required property var modelData
                    iconName: modelData.icon
                    text: modelData.text
                    onClicked: {
                        const run = modelData.run
                        sheet.close()
                        run()
                    }
                }
            }
            // Other devices of the server login (server songs only).
            Row {
                visible: sheet.devices.length > 0
                iconName: "devices"
                text: Theme.tr("Play on")
                onClicked: sheet.showDevices = !sheet.showDevices
            }
            Repeater {
                model: sheet.showDevices ? sheet.devices : []
                Row {
                    required property var modelData
                    leftPadding: 44
                    text: modelData.name
                    onClicked: {
                        sheet.close()
                        Core.call("devices.playOn", { id: modelData.id, ids: sheet.ids, start: 0 })
                    }
                }
            }
        }
    }
}
