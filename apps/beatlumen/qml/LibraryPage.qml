import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Songs, albums, artists, folders and playlists of the local library and
// the connected server.
Item {
    id: page
    signal addFolder()
    signal openFiles()
    signal connectServer()

    readonly property int libraryRev: Core.state.libraryRev || 0
    readonly property int playlistsRev: Core.state.playlistsRev || 0
    readonly property int trackCount: Core.state.libraryTracks || 0
    property var tracks: []
    property var albums: []
    property var artists: []
    property var playlists: []
    property var results: []
    property bool loaded: false

    function reload() {
        tracks = Core.call("library.tracks") || []
        albums = Core.call("library.albums") || []
        artists = Core.call("library.artists") || []
        loaded = true
    }
    function reloadPlaylists() {
        playlists = (Core.call("playlists.list") || []).filter(p => p.kind === "Track")
    }
    function back() {
        if (stack.depth > 1) {
            stack.pop()
            return true
        }
        return false
    }

    onLibraryRevChanged: reloadTimer.restart()
    onPlaylistsRevChanged: reloadPlaylists()
    Component.onCompleted: { reload(); reloadPlaylists() }
    Timer { id: reloadTimer; interval: 400; onTriggered: page.reload() }

    TrackMenu { id: trackMenu; onAddToPlaylist: (ids) => picker.show(ids) }
    PlaylistPicker { id: picker }

    StackView {
        id: stack
        anchors.fill: parent
        initialItem: mainView
    }

    Component {
        id: mainView
        ColumnLayout {
            spacing: 8
            PageHeader {
                Layout.fillWidth: true
                title: Theme.tr("Library")
                subtitle: Core.state.scanning ? Theme.tr("Scanning library…") + " " + (Core.state.scanProcessed || 0) + " / " + (Core.state.scanFound || 0)
                                              : page.trackCount + " " + Theme.tr("songs") + " · " + page.albums.length + " " + Theme.tr("albums")
                IconButton {
                    iconName: "add"
                    tip: Theme.tr("Add music folder")
                    onClicked: addMenu.popup()
                    Menu {
                        id: addMenu
                        MenuItem { text: Theme.tr("Add music folder"); icon.source: Theme.icon("create_new_folder"); onTriggered: page.addFolder() }
                        MenuItem { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onTriggered: page.openFiles() }
                        MenuItem { text: Theme.tr("Connect server"); icon.source: Theme.icon("cloud"); onTriggered: page.connectServer() }
                        MenuItem { text: Theme.tr("Rescan"); icon.source: Theme.icon("refresh"); onTriggered: Core.call("library.rescan") }
                    }
                }
            }
            TextField {
                id: search
                Layout.fillWidth: true
                Layout.leftMargin: 8
                Layout.rightMargin: 8
                placeholderText: Theme.tr("Search music")
                leftPadding: 44
                inputMethodHints: Qt.ImhNoPredictiveText
                visible: page.trackCount > 0
                Icon {
                    anchors.verticalCenter: parent.verticalCenter
                    x: 12
                    name: "search"
                    color: Theme.textDim
                }
                onTextChanged: searchTimer.restart()
                Timer {
                    id: searchTimer
                    interval: 180
                    onTriggered: page.results = search.text.trim().length > 0 ? (Core.call("library.search", { query: search.text }) || []) : []
                }
            }
            TabBar {
                id: tabs
                Layout.fillWidth: true
                visible: search.text.length === 0 && page.trackCount > 0
                background: null
                Repeater {
                    model: [Theme.tr("Songs"), Theme.tr("Albums"), Theme.tr("Artists"), Theme.tr("Folders"), Theme.tr("Playlists")]
                    TabButton { text: modelData }
                }
            }
            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                EmptyState {
                    anchors.centerIn: parent
                    width: Math.min(parent.width - 48, 420)
                    visible: page.loaded && page.trackCount === 0
                    iconName: "library_music"
                    title: Core.state.scanning ? Theme.tr("Scanning library…") : Theme.tr("Your library is empty")
                    text: Theme.tr("Add a music folder or connect to a Beatlumen server.")
                    Button { text: Theme.tr("Add music folder"); highlighted: true; icon.source: Theme.icon("create_new_folder"); onClicked: page.addFolder() }
                    Button { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onClicked: page.openFiles() }
                    Button { text: Theme.tr("Connect server"); flat: true; icon.source: Theme.icon("cloud"); onClicked: page.connectServer() }
                }

                TrackListView {
                    anchors.fill: parent
                    visible: search.text.length > 0
                    tracks: page.results
                    onTrackMenu: (t, a) => trackMenu.openFor([t.id], a)
                }

                StackLayout {
                    anchors.fill: parent
                    visible: search.text.length === 0 && page.trackCount > 0
                    currentIndex: tabs.currentIndex

                    // Songs
                    ColumnLayout {
                        spacing: 0
                        RowLayout {
                            Layout.leftMargin: 8
                            spacing: 8
                            Button { text: Theme.tr("Play all"); icon.source: Theme.icon("play_arrow"); highlighted: true
                                     onClicked: { Core.call("shuffle", { value: false }); Core.call("play", { ids: page.tracks.map(t => t.id), start: 0 }) } }
                            Button { text: Theme.tr("Shuffle all"); icon.source: Theme.icon("shuffle"); flat: true
                                     onClicked: { Core.call("shuffle", { value: true }); Core.call("play", { ids: page.tracks.map(t => t.id), start: Math.floor(Math.random() * page.tracks.length) }) } }
                        }
                        TrackListView {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            tracks: page.tracks
                            onTrackMenu: (t, a) => trackMenu.openFor([t.id], a)
                        }
                    }

                    // Albums
                    GridView {
                        id: albumGrid
                        clip: true
                        readonly property int cols: Math.max(2, Math.floor(width / 180))
                        cellWidth: width / cols
                        cellHeight: cellWidth + 56
                        model: page.albums
                        ScrollBar.vertical: ScrollBar { }
                        delegate: ItemDelegate {
                            width: albumGrid.cellWidth
                            height: albumGrid.cellHeight
                            padding: 8
                            Component.onCompleted: if (modelData.coverTrack && !modelData.cover) Covers.request(modelData.coverTrack)
                            onClicked: stack.push(albumView, { album: modelData })
                            contentItem: ColumnLayout {
                                spacing: 6
                                CoverImage {
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: width
                                    placeholder: "album"
                                    source: modelData.coverTrack ? Covers.url(modelData.coverTrack, modelData.cover) : modelData.cover
                                }
                                Label { Layout.fillWidth: true; text: modelData.title || Theme.tr("Unknown album"); font.weight: Font.DemiBold; elide: Text.ElideRight; color: Theme.text }
                                Label { Layout.fillWidth: true; text: modelData.artist || Theme.tr("Unknown artist"); elide: Text.ElideRight; color: Theme.textDim; font.pixelSize: 13 }
                            }
                        }
                    }

                    // Artists
                    ListView {
                        clip: true
                        model: page.artists
                        ScrollBar.vertical: ScrollBar { }
                        delegate: ItemDelegate {
                            width: ListView.view.width
                            height: Theme.row
                            onClicked: stack.push(artistView, { artist: modelData.name })
                            contentItem: RowLayout {
                                spacing: 12
                                Rectangle {
                                    Layout.preferredWidth: 44; Layout.preferredHeight: 44; radius: 22
                                    color: Qt.alpha(Theme.accent, 0.18)
                                    Label { anchors.centerIn: parent; text: (modelData.name || "?").charAt(0).toUpperCase(); color: Theme.accent; font.weight: Font.Bold; font.pixelSize: 18 }
                                }
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    Label { Layout.fillWidth: true; text: modelData.name || Theme.tr("Unknown artist"); font.pixelSize: 15; font.weight: Font.Medium; elide: Text.ElideRight; color: Theme.text }
                                    Label { text: modelData.tracks + " " + Theme.tr("songs") + " · " + modelData.albums + " " + Theme.tr("albums"); color: Theme.textDim; font.pixelSize: 13 }
                                }
                                Icon { name: "chevron_right"; color: Theme.textFaint }
                            }
                        }
                    }

                    // Folders
                    FolderView {
                        onTrackMenu: (t, a) => trackMenu.openFor([t.id], a)
                    }

                    // Playlists
                    Item {
                        EmptyState {
                            anchors.centerIn: parent
                            width: Math.min(parent.width - 48, 420)
                            visible: page.playlists.length === 0
                            iconName: "queue_music"
                            title: Theme.tr("No playlists yet")
                            Button { text: Theme.tr("New playlist"); icon.source: Theme.icon("add"); onClicked: newPlaylist.open() }
                        }
                        ListView {
                            anchors.fill: parent
                            clip: true
                            model: page.playlists
                            header: ItemDelegate {
                                width: ListView.view.width
                                visible: page.playlists.length > 0
                                height: visible ? Theme.row : 0
                                text: Theme.tr("New playlist")
                                icon.source: Theme.icon("add")
                                onClicked: newPlaylist.open()
                            }
                            delegate: ItemDelegate {
                                width: ListView.view.width
                                height: Theme.row
                                onClicked: stack.push(playlistView, { playlist: modelData })
                                contentItem: RowLayout {
                                    spacing: 12
                                    CoverImage { Layout.preferredWidth: 44; Layout.preferredHeight: 44; radius: 8; placeholder: "queue_music"; source: modelData.cover }
                                    ColumnLayout {
                                        Layout.fillWidth: true
                                        spacing: 2
                                        Label { Layout.fillWidth: true; text: modelData.name; font.pixelSize: 15; font.weight: Font.Medium; elide: Text.ElideRight; color: Theme.text }
                                        Label { text: modelData.count + " " + Theme.tr("songs") + (modelData.server ? " · " + Theme.tr("Server") : ""); color: Theme.textDim; font.pixelSize: 13 }
                                    }
                                    IconButton { iconName: "play_arrow"; tip: Theme.tr("Play"); onClicked: Core.call("playlist.play", { id: modelData.id }) }
                                }
                            }
                        }
                        TextPrompt {
                            id: newPlaylist
                            title: Theme.tr("New playlist")
                            placeholder: Theme.tr("Playlist name")
                            acceptText: Theme.tr("Create")
                            onSubmitted: (t) => Core.call("playlist.create", { name: t, kind: "Track", server: Core.state.server === "connected" })
                        }
                    }
                }
            }
        }
    }

    // ---- drill-down pages -------------------------------------------------
    component DetailHeader: ColumnLayout {
        id: dh
        property string title
        property string subtitle
        property string cover
        property string placeholder: "album"
        property var ids: []
        default property alias extra: extraRow.data
        spacing: 12
        PageHeader {
            Layout.fillWidth: true
            back: true
            title: ""
            onBackClicked: stack.pop()
        }
        RowLayout {
            Layout.leftMargin: 12
            Layout.rightMargin: 12
            spacing: 16
            CoverImage {
                Layout.preferredWidth: 112
                Layout.preferredHeight: 112
                radius: 14
                placeholder: dh.placeholder
                source: dh.cover
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 4
                Label { Layout.fillWidth: true; text: dh.title; font.pixelSize: 22; font.weight: Font.Bold; wrapMode: Text.WordWrap; maximumLineCount: 2; elide: Text.ElideRight; color: Theme.text }
                Label { Layout.fillWidth: true; text: dh.subtitle; color: Theme.textDim; elide: Text.ElideRight }
                RowLayout {
                    spacing: 4
                    Button {
                        text: Theme.tr("Play"); highlighted: true; icon.source: Theme.icon("play_arrow")
                        onClicked: { Core.call("shuffle", { value: false }); Core.call("play", { ids: dh.ids, start: 0 }) }
                    }
                    IconButton {
                        iconName: "shuffle"; tip: Theme.tr("Shuffle")
                        onClicked: { Core.call("shuffle", { value: true }); Core.call("play", { ids: dh.ids, start: 0 }) }
                    }
                    IconButton {
                        id: moreButton
                        iconName: "more_vert"; tip: Theme.tr("More")
                        onClicked: trackMenu.openFor(dh.ids, moreButton)
                    }
                    RowLayout { id: extraRow; spacing: 0 }
                }
            }
        }
    }

    Component {
        id: albumView
        ColumnLayout {
            property var album: ({})
            property var tracks: Core.call("library.album", { id: album.id }) || []
            DetailHeader {
                Layout.fillWidth: true
                title: album.title || Theme.tr("Unknown album")
                subtitle: [album.artist, album.year, tracks.length + " " + Theme.tr("songs")].filter(x => x).join(" · ")
                cover: album.coverTrack ? Covers.url(album.coverTrack, album.cover) : (album.cover || "")
                ids: tracks.map(t => t.id)
            }
            TrackListView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                tracks: parent.tracks
                numbered: true
                showCovers: false
                onTrackMenu: (t, a) => trackMenu.openFor([t.id], a)
            }
        }
    }

    Component {
        id: artistView
        ColumnLayout {
            property string artist
            property var tracks: Core.call("library.artist", { name: artist }) || []
            DetailHeader {
                Layout.fillWidth: true
                title: artist || Theme.tr("Unknown artist")
                subtitle: tracks.length + " " + Theme.tr("songs")
                placeholder: "person"
                cover: tracks.length > 0 ? Covers.url(tracks[0].id, tracks[0].cover) : ""
                ids: tracks.map(t => t.id)
            }
            TrackListView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                tracks: parent.tracks
                onTrackMenu: (t, a) => trackMenu.openFor([t.id], a)
            }
        }
    }

    Component {
        id: playlistView
        ColumnLayout {
            id: pv
            property var playlist: ({})
            property var tracks: []
            function reload() { tracks = Core.call("playlist.items", { id: playlist.id }) || [] }
            Component.onCompleted: reload()
            Connections { target: page; function onPlaylistsRevChanged() { pv.reload() } }
            DetailHeader {
                Layout.fillWidth: true
                title: pv.playlist.name || ""
                subtitle: pv.tracks.length + " " + Theme.tr("songs")
                placeholder: "queue_music"
                cover: pv.playlist.cover || ""
                ids: pv.tracks.map(t => t.id)
                IconButton {
                    iconName: "edit"; tip: Theme.tr("Rename")
                    onClicked: renamePrompt.open()
                }
                IconButton {
                    iconName: "delete"; tip: Theme.tr("Delete")
                    onClicked: { Core.call("playlist.delete", { id: pv.playlist.id }); stack.pop() }
                }
            }
            TrackListView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                tracks: pv.tracks
                onTrackMenu: (t, a) => {
                    const i = pv.tracks.indexOf(t)
                    trackMenu.openFor([t.id], a, [{ text: Theme.tr("Remove"), action: () => Core.call("playlist.removeItem", { id: pv.playlist.id, index: i }) }])
                }
            }
            TextPrompt {
                id: renamePrompt
                title: Theme.tr("Rename")
                text: pv.playlist.name || ""
                onSubmitted: (t) => Core.call("playlist.rename", { id: pv.playlist.id, name: t })
            }
        }
    }
}
