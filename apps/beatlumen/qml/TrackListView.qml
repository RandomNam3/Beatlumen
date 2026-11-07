import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Scrollable song list; tapping a song plays the whole list from there. With
// `selectable`, several songs can be selected: Ctrl and Shift with a mouse, a
// long press with a finger. A bar then offers what to do with them.
ListView {
    id: list
    property var tracks: []
    property bool numbered: false
    property bool showCovers: true
    property bool selectable: false
    signal trackMenu(var track, Item anchor)
    signal selectionMenu(var ids, Item anchor)

    // Ids of the selected songs, and where a Shift range starts.
    property var selected: ({})
    property int selectedCount: 0
    property int rangeStart: -1
    readonly property bool selecting: selectedCount > 0

    function setSelected(next) {
        selected = next
        selectedCount = Object.keys(next).length
        const win = ApplicationWindow.window
        if (win && "selectionOwner" in win) {
            if (selectedCount > 0)
                win.selectionOwner = list
            else if (win.selectionOwner === list)
                win.selectionOwner = null
        }
    }
    function toggleAt(index) {
        const id = tracks[index].id
        const next = Object.assign({}, selected)
        if (next[id])
            delete next[id]
        else
            next[id] = true
        rangeStart = index
        setSelected(next)
    }
    function rangeTo(index) {
        const from = rangeStart < 0 ? index : rangeStart
        const next = Object.assign({}, selected)
        for (let i = Math.min(from, index); i <= Math.max(from, index); i++)
            next[tracks[i].id] = true
        setSelected(next)
    }
    function selectAll() {
        const next = {}
        for (const t of tracks)
            next[t.id] = true
        setSelected(next)
    }
    function clearSelection() {
        rangeStart = -1
        setSelected({})
    }
    // In the order of the list.
    function selectedIds() {
        return tracks.filter(t => selected[t.id]).map(t => t.id)
    }
    onTracksChanged: if (selecting) clearSelection()
    Component.onDestruction: if (selecting) clearSelection()

    clip: true
    model: tracks
    reuseItems: true
    boundsBehavior: Flickable.StopAtBounds
    ScrollBar.vertical: ScrollBar { }
    readonly property string currentId: Core.state.current ? Core.state.current.id : ""
    bottomMargin: selecting ? selectionBar.height + 12 : 0

    delegate: TrackDelegate {
        required property var modelData
        required property int index
        track: modelData
        number: list.numbered ? (modelData.track || index + 1) : -1
        showCover: list.showCovers
        current: modelData.id === list.currentId
        selectable: list.selectable
        selecting: list.selecting
        selected: list.selected[modelData.id] === true
        onSelectToggled: list.toggleAt(index)
        onSelectRange: list.rangeTo(index)
        onPlay: Core.call("play", { ids: list.tracks.map(t => t.id), start: index })
        onMenu: (t, a) => list.trackMenu(t, a)
    }

    // What to do with the selected songs.
    Rectangle {
        id: selectionBar
        visible: list.selecting
        parent: list
        z: 10
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.margins: 8
        height: Theme.touch + 12
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
                tip: Theme.tr("Clear selection")
                onClicked: list.clearSelection()
            }
            Label {
                id: selectionCount
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: Theme.tr("%1 selected").arg(list.selectedCount)
                color: Theme.text
                elide: Text.ElideRight
            }
            IconButton {
                iconName: "check"
                tip: Theme.tr("Select all")
                onClicked: list.selectAll()
            }
            IconButton {
                iconName: "play_arrow"
                tip: Theme.tr("Play")
                onClicked: {
                    Core.call("play", { ids: list.selectedIds(), start: 0 })
                    list.clearSelection()
                }
            }
            IconButton {
                iconName: "queue_music"
                tip: Theme.tr("Add to queue")
                onClicked: {
                    Core.call("enqueue", { ids: list.selectedIds() })
                    list.clearSelection()
                }
            }
            IconButton {
                id: selectionMore
                iconName: "more_vert"
                tip: Theme.tr("More")
                onClicked: list.selectionMenu(list.selectedIds(), selectionMore)
            }
        }
    }
}
