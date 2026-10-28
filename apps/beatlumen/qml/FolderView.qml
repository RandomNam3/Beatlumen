import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Browses the library by folder.
ColumnLayout {
    id: fv
    property var path: []           // stack of folder paths ("" = roots)
    property var view: ({ folders: [], tracks: [] })
    signal trackMenu(var track, Item anchor)
    readonly property string current: path.length > 0 ? path[path.length - 1] : ""
    readonly property int libraryRev: Core.state.libraryRev || 0

    function load() { view = Core.call("library.folder", { path: current }) || { folders: [], tracks: [] } }
    onCurrentChanged: load()
    onLibraryRevChanged: load()
    Component.onCompleted: load()
    spacing: 0

    RowLayout {
        visible: fv.path.length > 0
        Layout.fillWidth: true
        IconButton {
            iconName: "arrow_back"
            tip: Theme.tr("Back")
            onClicked: fv.path = fv.path.slice(0, -1)
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: fv.current.replace(/^server:/, Theme.tr("Server") + " / ")
            elide: Text.ElideLeft
            color: Theme.textDim
        }
        IconButton {
            iconName: "play_arrow"
            tip: Theme.tr("Play all")
            visible: fv.view.tracks.length > 0
            onClicked: Core.call("play", { ids: fv.view.tracks.map(t => t.id), start: 0 })
        }
    }
    ListView {
        Layout.fillWidth: true
        Layout.fillHeight: true
        clip: true
        ScrollBar.vertical: ScrollBar { }
        model: fv.view.folders.concat(fv.view.tracks)
        delegate: Loader {
            required property var modelData
            required property int index
            width: ListView.view.width
            sourceComponent: modelData.path !== undefined && modelData.count !== undefined ? folderDelegate : trackDelegate
            Component {
                id: folderDelegate
                ItemDelegate {
                    height: Theme.row
                    onClicked: fv.path = fv.path.concat([modelData.path])
                    contentItem: RowLayout {
                        spacing: 12
                        Icon { name: modelData.path.startsWith("server:") && fv.path.length === 0 ? "cloud" : "folder"; color: Theme.accent }
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.name; elide: Text.ElideRight; color: Theme.text; font.pixelSize: 15 }
                        Label { textFormat: Text.PlainText; text: modelData.count; color: Theme.textFaint }
                        Icon { name: "chevron_right"; color: Theme.textFaint }
                    }
                }
            }
            Component {
                id: trackDelegate
                TrackDelegate {
                    track: modelData
                    current: Core.state.current && Core.state.current.id === modelData.id
                    onPlay: Core.call("play", { ids: fv.view.tracks.map(t => t.id), start: index - fv.view.folders.length })
                    onMenu: (t, a) => fv.trackMenu(t, a)
                }
            }
        }
    }
}
