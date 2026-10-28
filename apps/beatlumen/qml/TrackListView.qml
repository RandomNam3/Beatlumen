import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// Scrollable song list; tapping a song plays the whole list from there.
ListView {
    id: list
    property var tracks: []
    property bool numbered: false
    property bool showCovers: true
    signal trackMenu(var track, Item anchor)

    clip: true
    model: tracks
    reuseItems: true
    boundsBehavior: Flickable.StopAtBounds
    ScrollBar.vertical: ScrollBar { }
    readonly property string currentId: Core.state.current ? Core.state.current.id : ""

    delegate: TrackDelegate {
        required property var modelData
        required property int index
        track: modelData
        number: list.numbered ? (modelData.track || index + 1) : -1
        showCover: list.showCovers
        current: modelData.id === list.currentId
        onPlay: Core.call("play", { ids: list.tracks.map(t => t.id), start: index })
        onMenu: (t, a) => list.trackMenu(t, a)
    }
}
