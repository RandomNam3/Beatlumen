import QtQuick
import QtQuick.Controls.Material
import QtQuick.Effects
import QtQuick.Layouts
import Beatlumen.Core

// Songs, albums, artists, folders and playlists of the local library and
// the connected server.
Item {
    id: page
    signal addFolder()
    signal openFiles()
    signal connectServer()
    signal uploadVault()
    signal exportPlaylist(string id)
    // Back from a detail page to the list.
    signal detailClosed()

    readonly property int libraryRev: Core.state.libraryRev || 0
    readonly property int playlistsRev: Core.state.playlistsRev || 0
    readonly property int trackCount: Core.state.libraryTracks || 0
    readonly property bool hasContent: trackCount > 0 || ((Core.state.sources || {}).stations || []).length > 0 || (Core.state.trashCount || 0) > 0
    property var tracks: []
    property var albums: []
    property var artists: []
    property var playlists: []
    property bool loaded: false

    function reload() {
        tracks = Core.call("library.tracks") || []
        albums = Core.call("library.albums") || []
        artists = Core.call("library.artists") || []
        loaded = true
    }
    function reloadPlaylists() {
        playlists = (Core.call("playlists.list") || []).filter(p => p.kind === "track")
    }
    function openVault() {
        if (!(stack.currentItem instanceof VaultPage))
            stack.push(vaultView)
    }
    property var folders: null
    function back() {
        if (stack.depth > 1) {
            stack.pop()
            return true
        }
        return !!folders && folders.visible && folders.up()
    }
    // Wiping back leaves a detail page first.
    function swipe(dir) {
        return dir < 0 && back()
    }
    function openAlbum(album) {
        stack.pop(null)
        stack.push(albumView, { album: album })
    }
    function openArtist(name) {
        stack.pop(null)
        stack.push(artistView, { artist: name })
    }
    function openLabel(name) {
        stack.pop(null)
        stack.push(labelView, { label: name })
    }
    // Wide enough for the sections at the side.
    readonly property bool wide: width >= 860
    // What is pinned to the side (of this device).
    readonly property var pinned: ((Core.state.settings || {}).ui || {}).pinned || []
    function isPinned(kind, id) {
        return pinned.some(p => p.kind === kind && p.id === id)
    }
    function setPinned(kind, id, name, on) {
        const rest = pinned.filter(p => !(p.kind === kind && p.id === id))
        Core.call("settings.set", { key: "ui.pinned", value: on ? rest.concat([{ kind: kind, id: id, name: name }]) : rest })
    }
    // The entry for a detail page's menu.
    function pinItem(kind, id, name) {
        const on = isPinned(kind, id)
        return { text: on ? Theme.tr("Unpin") : Theme.tr("Pin to the side"), action: () => setPinned(kind, id, name, !on) }
    }
    function openPinned(p) {
        switch (p.kind) {
        case "album": {
            const a = albums.find(x => x.id === p.id)
            if (a)
                openAlbum(a)
            break
        }
        case "artist": openArtist(p.id); break
        case "label": openLabel(p.id); break
        case "playlist": {
            const l = playlists.find(x => x.id === p.id)
            if (l) {
                stack.pop(null)
                stack.push(playlistView, { playlist: l })
            }
            break
        }
        }
    }
    function filtered(list, filter) {
        switch (filter) {
        case "favorites": return list.filter(t => t.favorite === true)
        case "local": return list.filter(t => t.local === true)
        case "server": return list.filter(t => !t.local && !t.source)
        case "offline": return list.filter(t => t.offline === true)
        default: return list
        }
    }
    function openCollection(ext, id) {
        stack.pop(null)
        stack.push(collectionView, { ext: ext, itemId: id })
        Core.call("extensions.collection", { ext: ext, id: id })
    }
    function openMix(id, name, cover) {
        stack.pop(null)
        stack.push(mixView, { mixId: id, mixName: name, cover: cover })
    }
    function openFavorites() {
        const fav = (Core.call("playlists.list") || []).find(p => p.fixed === true)
        stack.pop(null)
        if (fav)
            stack.push(playlistView, { playlist: fav })
    }
    function play(ids, shuffle) {
        if (ids.length === 0)
            return
        Core.call("shuffle", { value: shuffle })
        Core.call("play", { ids: ids, start: shuffle ? Math.floor(Math.random() * ids.length) : 0 })
    }
    function albumTitle(a) { return (!a.title || a.title === "Unknown Album") ? Theme.tr("Unknown album") : a.title }
    function artistName(n) { return (!n || n === "Unknown Artist") ? Theme.tr("Unknown artist") : n }
    function count(n, one, many) { return n + " " + Theme.tr(n === 1 ? one : many) }

    onLibraryRevChanged: reloadTimer.restart()
    onPlaylistsRevChanged: reloadPlaylists()
    Component.onCompleted: { reload(); reloadPlaylists() }
    Timer { id: reloadTimer; interval: 400; onTriggered: page.reload() }

    TrackActions { id: songMenu; onAddToPlaylist: (ids) => picker.show(ids); canEditLabels: true; onEditLabels: (id) => labelsDialog.show(id) }
    LabelsDialog { id: labelsDialog }
    PlaylistPicker { id: picker }
    StationDialog { id: stationDialog }
    TextPrompt {
        id: newPlaylist
        title: Theme.tr("New playlist")
        placeholder: Theme.tr("Playlist name")
        acceptText: Theme.tr("Create")
        onSubmitted: (t) => Core.call("playlist.create", { name: t, kind: "track", server: Core.state.server === "connected" })
    }

    AppStackView {
        id: stack
        onDepthChanged: if (depth === 1) page.detailClosed()
        anchors.fill: parent
        initialItem: mainView
    }

    Component {
        id: vaultView
        VaultPage {
            back: true
            onBackClicked: stack.pop()
            onUpload: page.uploadVault()
            onOpenTrash: {
                stack.pop(null)
                stack.currentItem.showTrash()
            }
        }
    }

    // ---- building blocks --------------------------------------------------

    // Round play button on top of covers.
    component PlayBadge: RoundButton {
        id: badge
        implicitWidth: 40
        implicitHeight: 40
        padding: 0
        Material.background: Theme.accent
        Material.elevation: 2
        icon.source: Theme.icon("play_arrow")
        icon.color: Theme.onAccent
        icon.width: 26
        icon.height: 26
        display: AbstractButton.IconOnly
        Accessible.name: Theme.tr("Play")
        ToolTip.text: Theme.tr("Play")
        ToolTip.visible: hovered && !Core.mobile
        ToolTip.delay: 600
    }

    // Row with an avatar/cover, two lines of text and a trailing item.
    component ListRow: ItemDelegate {
        id: lr
        property string title
        property string subtitle
        default property alias trailing: trail.data
        property alias lead: leadBox.data
        width: ListView.view ? ListView.view.width : implicitWidth
        height: Theme.listRow
        leftPadding: 12
        rightPadding: 8
        Accessible.name: title
        contentItem: RowLayout {
            spacing: 12
            Item { id: leadBox; Layout.preferredWidth: Theme.lead; Layout.preferredHeight: Theme.lead }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: lr.title; font.pixelSize: 16; font.weight: Font.Medium; elide: Text.ElideRight; color: Theme.text }
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: lr.subtitle; visible: text !== ""; color: Theme.textDim; font.pixelSize: 13; elide: Text.ElideRight }
            }
            Row { id: trail; Layout.alignment: Qt.AlignVCenter }
        }
    }

    // ---- main view --------------------------------------------------------
    Component {
        id: mainView
        ColumnLayout {
            id: main
            spacing: 0
            property int section: 0
            property string filter: "all"
            readonly property var sectionList: [
                { label: Theme.tr("Songs"), icon: "music_note" },
                { label: Theme.tr("Albums"), icon: "album" },
                { label: Theme.tr("Artists"), icon: "person" },
                { label: Theme.tr("Folders"), icon: "folder" },
                { label: Theme.tr("Playlists"), icon: "queue_music" },
                { label: Theme.tr("Labels"), icon: "label" }
            ].concat((Core.state.sources || {}).available === true ? [{ label: Theme.tr("Radio"), icon: "radio" }] : [])
             .concat(Core.state.server === "connected" || (Core.state.trashCount || 0) > 0 ? [{ label: Theme.tr("Trash"), icon: "delete", page: 7 }] : [])
            onSectionListChanged: if (section >= sectionList.length) section = 0
            function showTrash() {
                section = sectionList.findIndex(s => s.page === 7)
            }

            PageHeader {
                Layout.fillWidth: true
                title: Theme.tr("Library")
                subtitle: Core.state.scanning ? Theme.tr("Scanning library…") + " " + (Core.state.scanProcessed || 0) + " / " + (Core.state.scanFound || 0)
                                              : page.count(page.trackCount, "song", "songs") + " · " + page.count(page.albums.length, "album", "albums")
                IconButton {
                    readonly property var cap: Core.state.capture || ({})
                    readonly property bool on: cap.running === true || cap.requested === true
                    visible: cap.available === true
                    iconName: "graphic_eq"
                    active: on
                    tip: on ? Theme.tr("Stop visualizing other apps") : Theme.tr("Visualize other apps")
                    onClicked: Core.call(on ? "capture.stop" : "capture.start")
                }
                IconButton {
                    visible: Core.state.server === "connected" || Core.state.server === "login"
                    iconName: "lock"
                    tip: Theme.tr("Encrypted files")
                    onClicked: page.openVault()
                }
                IconButton {
                    iconName: "add"
                    tip: Theme.tr("Add music folder")
                    onClicked: addMenu.popup()
                    AppMenu {
                        id: addMenu
                        MenuItem { text: Theme.tr("Add music folder"); icon.source: Theme.icon("create_new_folder"); onTriggered: page.addFolder() }
                        MenuItem { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onTriggered: page.openFiles() }
                        MenuItem { text: Theme.tr("Connect server"); icon.source: Theme.icon("cloud"); onTriggered: page.connectServer() }
                        MenuItem { text: Theme.tr("Rescan"); icon.source: Theme.icon("refresh"); onTriggered: Core.call("library.rescan") }
                    }
                }
            }

            // Sections as filter chips (scroll sideways on narrow screens).
            Flickable {
                id: chips
                Layout.fillWidth: true
                Layout.preferredHeight: 36
                Layout.topMargin: 10
                Layout.bottomMargin: 6
                visible: page.hasContent && !page.wide
                contentWidth: chipRow.width + 24
                flickableDirection: Flickable.HorizontalFlick
                boundsBehavior: Flickable.StopAtBounds
                clip: true
                // Keeps a chip (and a bit of its neighbours) in view.
                function reveal(item) {
                    const pad = 40
                    const x = chipRow.x + item.x
                    if (x - pad < contentX)
                        contentX = Math.max(0, x - pad)
                    else if (x + item.width + pad > contentX + width)
                        contentX = Math.min(contentWidth - width, x + item.width + pad - width)
                }
                Row {
                    id: chipRow
                    x: 12
                    spacing: 6
                    Repeater {
                        model: main.sectionList.map(s => s.label)
                        Chip {
                            required property string modelData
                            required property int index
                            text: modelData
                            checked: main.section === index
                            onClicked: {
                                main.section = index
                                chips.reveal(this)
                            }
                        }
                    }
                }
                // Hint that more chips follow.
                Rectangle {
                    x: chips.contentX + chips.width - width
                    width: 28
                    height: parent.height
                    visible: chips.contentX + chips.width < chips.contentWidth - 1
                    gradient: Gradient {
                        orientation: Gradient.Horizontal
                        GradientStop { position: 0; color: "transparent" }
                        GradientStop { position: 1; color: Theme.surfaceStrong }
                    }
                }
            }

            LibrarySidebar {
                id: sidebar
                parent: libraryBody
                visible: page.wide && page.hasContent
                anchors.left: parent.left
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                anchors.topMargin: 10
                width: 200
                sections: main.sectionList
                section: main.section
                filter: main.filter
                pinned: page.pinned
                onSectionChosen: (i) => main.section = i
                onFilterChosen: (f) => main.filter = f
                onPinnedOpened: (p) => page.openPinned(p)
                onUnpin: (p) => page.setPinned(p.kind, p.id, p.name, false)
            }
            Item {
                id: libraryBody
                Layout.fillWidth: true
                Layout.fillHeight: true

                EmptyState {
                    anchors.centerIn: parent
                    width: Math.min(parent.width - 48, 420)
                    visible: page.loaded && !page.hasContent
                    iconName: "library_music"
                    title: Core.state.scanning ? Theme.tr("Scanning library…") : Theme.tr("Your library is empty")
                    text: Theme.tr("Add a music folder or connect to a Beatlumen server.")
                    AccentButton { text: Theme.tr("Add music folder"); highlighted: true; icon.source: Theme.icon("create_new_folder"); onClicked: page.addFolder() }
                    Button { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onClicked: page.openFiles() }
                    Button { text: Theme.tr("Connect server"); flat: true; icon.source: Theme.icon("cloud"); onClicked: page.connectServer() }
                    Button {
                        visible: (Core.state.sources || {}).available === true
                        text: Theme.tr("Add radio station")
                        flat: true
                        icon.source: Theme.icon("graphic_eq")
                        onClicked: stationDialog.openFor(null)
                    }
                    Button {
                        visible: (Core.state.capture || {}).available === true
                        text: Theme.tr("Visualize other apps")
                        flat: true
                        icon.source: Theme.icon("graphic_eq")
                        onClicked: Core.call("capture.start")
                    }
                }

                StackLayout {
                    id: sectionPages
                    anchors.fill: parent
                    anchors.leftMargin: sidebar.visible ? sidebar.width + 8 : 0
                    visible: page.hasContent
                    SwitchMotion { stack: sectionPages }
                    currentIndex: (main.sectionList[main.section] || {}).page || main.section

                    // Songs: play/shuffle scroll away with the list.
                    TrackListView {
                        id: songsList
                        readonly property var shown: page.filtered(page.tracks, main.filter)
                        tracks: shown
                        selectable: true
                        onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                        onSelectionMenu: (ids, a) => songMenu.openFor(ids, a)
                        header: Item {
                            width: ListView.view.width
                            height: 60
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                spacing: 8
                                // Narrow windows: the shuffle button keeps its icon only.
                                readonly property bool narrow: width < 340
                                AccentButton {
                                    text: Theme.tr("Play all"); icon.source: Theme.icon("play_arrow"); highlighted: true
                                    onClicked: page.play(songsList.shown.map(t => t.id), false)
                                }
                                Button {
                                    display: parent.narrow ? AbstractButton.IconOnly : AbstractButton.TextBesideIcon
                                    Accessible.name: Theme.tr("Shuffle all")
                                    text: Theme.tr("Shuffle all"); icon.source: Theme.icon("shuffle"); flat: true
                                    onClicked: page.play(page.tracks.map(t => t.id), true)
                                }
                                Item { Layout.fillWidth: true }
                            }
                        }
                    }

                    // Albums
                    GridView {
                        id: albumGrid
                        clip: true
                        leftMargin: 8
                        rightMargin: 8
                        topMargin: 4
                        bottomMargin: 8
                        boundsBehavior: Flickable.StopAtBounds
                        readonly property real avail: width - leftMargin - rightMargin
                        readonly property int cols: Math.max(2, Math.floor(avail / 150))
                        cellWidth: Math.floor(avail / cols)
                        cellHeight: cellWidth + 52
                        model: page.albums
                        ScrollBar.vertical: ScrollBar { }
                        delegate: ItemDelegate {
                            id: albumCell
                            required property var modelData
                            width: albumGrid.cellWidth
                            height: albumGrid.cellHeight
                            padding: 6
                            topPadding: 6
                            bottomPadding: 4
                            Accessible.name: page.albumTitle(modelData)
                            background: Rectangle {
                                radius: 14
                                color: albumCell.down ? Theme.surfaceVariant : "transparent"
                            }
                            Component.onCompleted: if (modelData.coverTrack && !modelData.cover) Covers.request(modelData.coverTrack)
                            onClicked: stack.push(albumView, { album: modelData })
                            contentItem: ColumnLayout {
                                spacing: 2
                                CoverImage {
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: width
                                    Layout.bottomMargin: 6
                                    radius: 12
                                    placeholder: "album"
                                    seed: albumCell.modelData.title || ""
                                    source: albumCell.modelData.coverTrack ? Covers.url(albumCell.modelData.coverTrack, albumCell.modelData.cover) : (albumCell.modelData.cover || "")
                                    PlayBadge {
                                        anchors.right: parent.right
                                        anchors.bottom: parent.bottom
                                        anchors.margins: 6
                                        onClicked: page.play((Core.call("library.album", { id: albumCell.modelData.id }) || []).map(t => t.id), false)
                                    }
                                }
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: page.albumTitle(albumCell.modelData); font.pixelSize: 14; font.weight: Font.DemiBold; elide: Text.ElideRight; color: Theme.text }
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: page.artistName(albumCell.modelData.artist); elide: Text.ElideRight; color: Theme.textDim; font.pixelSize: 13 }
                            }
                        }
                    }

                    // Artists
                    ListView {
                        clip: true
                        boundsBehavior: Flickable.StopAtBounds
                        model: page.artists
                        ScrollBar.vertical: ScrollBar { }
                        delegate: ListRow {
                            id: artistRow
                            required property var modelData
                            title: page.artistName(modelData.name)
                            subtitle: page.count(modelData.tracks, "song", "songs") + " · " + page.count(modelData.albums, "album", "albums")
                            onClicked: stack.push(artistView, { artist: modelData.name })
                            lead: Rectangle {
                                anchors.fill: parent
                                radius: width / 2
                                color: Theme.seedColor(artistRow.title, 0.30)
                                Label {
                                    textFormat: Text.PlainText
                                    anchors.centerIn: parent
                                    text: artistRow.title.charAt(0).toUpperCase()
                                    color: Theme.seedColor(artistRow.title, 0.85)
                                    font.weight: Font.Bold
                                    font.pixelSize: 21
                                }
                            }
                            Icon { name: "chevron_right"; color: Theme.textFaint }
                        }
                    }

                    // Folders
                    FolderView {
                        Component.onCompleted: page.folders = this
                        onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                    }

                    // Playlists
                    Item {
                        EmptyState {
                            anchors.centerIn: parent
                            width: Math.min(parent.width - 48, 420)
                            visible: page.playlists.length === 0
                            iconName: "queue_music"
                            title: Theme.tr("No playlists yet")
                            text: Theme.tr("Collect songs with “Add to playlist…” in a song's menu.")
                            AccentButton { text: Theme.tr("New playlist"); highlighted: true; icon.source: Theme.icon("add"); onClicked: newPlaylist.open() }
                        }
                        ListView {
                            anchors.fill: parent
                            clip: true
                            boundsBehavior: Flickable.StopAtBounds
                            visible: page.playlists.length > 0
                            model: page.playlists
                            ScrollBar.vertical: ScrollBar { }
                            header: Column {
                                width: ListView.view ? ListView.view.width : 0
                                ListRow {
                                    width: parent.width
                                    title: Theme.tr("New playlist")
                                    onClicked: newPlaylist.open()
                                    lead: Rectangle {
                                        anchors.fill: parent
                                        radius: 12
                                        color: "transparent"
                                        border.width: 1.5
                                        border.color: Theme.outline
                                        Icon { anchors.centerIn: parent; name: "add"; color: Theme.accent }
                                    }
                                }
                                ListRow {
                                    width: parent.width
                                    title: Theme.tr("New smart playlist")
                                    subtitle: Theme.tr("Fills itself by rules.")
                                    onClicked: smartDialog.edit(null)
                                    lead: Rectangle {
                                        anchors.fill: parent
                                        radius: 12
                                        color: "transparent"
                                        border.width: 1.5
                                        border.color: Theme.outline
                                        Icon { anchors.centerIn: parent; name: "smart_playlist"; color: Theme.accent }
                                    }
                                }
                            }
                            delegate: ListRow {
                                id: playlistRow
                                required property var modelData
                                title: modelData.name
                                subtitle: page.count(modelData.count, "song", "songs") + (modelData.server ? " · " + Theme.tr("Server") : "") + (modelData.smart ? " · " + Theme.tr("Smart") : "")
                                onClicked: stack.push(playlistView, { playlist: modelData })
                                lead: CoverImage {
                                    anchors.fill: parent
                                    radius: 12
                                    placeholder: playlistRow.modelData.fixed ? "favorite" : playlistRow.modelData.smart ? "smart_playlist" : "queue_music"
                                    seed: playlistRow.modelData.name || ""
                                    source: playlistRow.modelData.cover || ""
                                }
                                IconButton {
                                    iconName: "play_circle"
                                    iconSize: 30
                                    iconColor: Theme.accent
                                    tip: Theme.tr("Play")
                                    onClicked: Core.call("playlist.play", { id: playlistRow.modelData.id })
                                }
                            }
                        }
                    }

                    // Labels
                    Item {
                        id: labelsPane
                        readonly property int rev: Core.state.labelsRev || 0
                        property var all: []
                        function reload() { all = Core.call("labels.list") || [] }
                        onRevChanged: if (visible) reload()
                        onVisibleChanged: if (visible) reload()
                        function ofKind(kind) { return all.filter(l => Theme.labelKind(l.name) === kind) }
                        EmptyState {
                            anchors.centerIn: parent
                            width: Math.min(parent.width - 48, 420)
                            visible: labelsPane.all.length === 0
                            iconName: "label"
                            title: Theme.tr("No labels yet")
                            text: Theme.tr("Labels come from tags and the sound. Add your own in the song menu.")
                        }
                        ScrollView {
                            id: labelsScroll
                            anchors.fill: parent
                            clip: true
                            contentWidth: availableWidth
                            ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                            ColumnLayout {
                                width: labelsScroll.availableWidth
                                spacing: 8
                                Repeater {
                                    model: [
                                        { kind: "", title: Theme.tr("Your labels") },
                                        { kind: "mood", title: Theme.tr("Mood") },
                                        { kind: "genre", title: Theme.tr("Genres") },
                                        { kind: "decade", title: Theme.tr("Decades") },
                                        { kind: "lang", title: Theme.tr("Languages") },
                                        { kind: "publisher", title: Theme.tr("Record labels") },
                                        { kind: "tempo", title: Theme.tr("Tempo") },
                                        { kind: "energy", title: Theme.tr("Energy") },
                                        { kind: "key", title: Theme.tr("Keys") }
                                    ]
                                    ColumnLayout {
                                        required property var modelData
                                        readonly property var labels: labelsPane.ofKind(modelData.kind)
                                        Layout.fillWidth: true
                                        visible: labels.length > 0
                                        spacing: 8
                                        SectionLabel { text: modelData.title }
                                        Flow {
                                            Layout.fillWidth: true
                                            Layout.leftMargin: 16
                                            Layout.rightMargin: 16
                                            spacing: 6
                                            Repeater {
                                                model: labels
                                                Chip {
                                                    required property var modelData
                                                    text: Theme.labelName(modelData.name) + "  " + modelData.count
                                                    onClicked: stack.push(labelView, { label: modelData.name })
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Internet radio
                    Item {
                        readonly property var stations: (Core.state.sources || {}).stations || []
                        EmptyState {
                            anchors.centerIn: parent
                            width: Math.min(parent.width - 48, 420)
                            visible: parent.stations.length === 0
                            iconName: "graphic_eq"
                            title: Theme.tr("No stations yet")
                            text: Theme.tr("Add a station by its stream address or a .pls or .m3u file.")
                            AccentButton { text: Theme.tr("Add station"); highlighted: true; icon.source: Theme.icon("add"); onClicked: stationDialog.openFor(null) }
                        }
                        ListView {
                            anchors.fill: parent
                            clip: true
                            boundsBehavior: Flickable.StopAtBounds
                            visible: parent.stations.length > 0
                            model: parent.stations
                            ScrollBar.vertical: ScrollBar { }
                            header: ListRow {
                                title: Theme.tr("Add station")
                                onClicked: stationDialog.openFor(null)
                                lead: Rectangle {
                                    anchors.fill: parent
                                    radius: 12
                                    color: "transparent"
                                    border.width: 1.5
                                    border.color: Theme.outline
                                    Icon { anchors.centerIn: parent; name: "add"; color: Theme.accent }
                                }
                            }
                            delegate: ListRow {
                                id: stationRow
                                required property var modelData
                                readonly property bool playing: Core.state.current && Core.state.current.id === modelData.id
                                title: modelData.name
                                subtitle: playing && Core.state.current.title !== modelData.name
                                          ? (Core.state.current.artist ? Core.state.current.artist + " – " : "") + Core.state.current.title
                                          : modelData.url
                                onClicked: Core.call("play", { ids: [modelData.id] })
                                onPressAndHold: stationDialog.openFor(modelData)
                                lead: CoverImage {
                                    anchors.fill: parent
                                    radius: 12
                                    placeholder: "graphic_eq"
                                    seed: stationRow.modelData.name || ""
                                }
                                IconButton {
                                    iconName: "more_vert"
                                    tip: Theme.tr("Edit")
                                    onClicked: stationDialog.openFor(stationRow.modelData)
                                }
                            }
                        }
                    }

                    // Trash
                    TrashView {}
                }
            }
        }
    }

    // ---- drill-down pages -------------------------------------------------

    // Album, artist or playlist: a large header that scrolls away with the
    // songs, and a slim bar with the title once it is gone.
    component DetailPage: Item {
        id: dp
        property string title
        property string subtitle
        property string cover
        property string placeholder: "album"
        property var tracks: []
        property bool numbered: false
        property bool round: false
        // Additional entries for the ⋮ menu: [{text, action}].
        property var menuItems: []
        // Shown between the header and the songs.
        property Component extra: null
        signal trackMenu(var track, Item anchor)
        readonly property var ids: tracks.map(t => t.id)
        readonly property bool wide: width >= 640

        TrackListView {
            id: list
            anchors.fill: parent
            tracks: dp.tracks
            numbered: dp.numbered
            showCovers: !dp.numbered
            selectable: true
            topMargin: 0
            onTrackMenu: (t, a) => dp.trackMenu(t, a)
            onSelectionMenu: (ids, a) => songMenu.openFor(ids, a)
            header: Item {
                id: hero
                width: ListView.view.width
                height: content.implicitHeight + 52 + (extraHolder.item ? extraHolder.item.implicitHeight + 16 : 0)

                // Ambient backdrop: the cover, blurred and fading out.
                Image {
                    id: backdropSource
                    anchors.fill: parent
                    source: Covers.safe(dp.cover)
                    sourceSize: Qt.size(48, 48)
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    visible: false
                }
                Rectangle {
                    id: fade
                    anchors.fill: parent
                    visible: false
                    layer.enabled: true
                    gradient: Gradient {
                        GradientStop { position: 0; color: Qt.rgba(1, 1, 1, 0.9) }
                        GradientStop { position: 0.55; color: Qt.rgba(1, 1, 1, 0.5) }
                        GradientStop { position: 1; color: "transparent" }
                    }
                }
                MultiEffect {
                    anchors.fill: parent
                    source: backdropSource
                    visible: backdropSource.status === Image.Ready
                    blurEnabled: true
                    blur: 1.0
                    blurMax: 48
                    autoPaddingEnabled: false
                    maskEnabled: true
                    maskSource: fade
                    // Soft mask (opacity follows the gradient's alpha) instead
                    // of the default hard cut.
                    maskThresholdMin: 0.5
                    maskSpreadAtMin: 1
                    opacity: Theme.dark ? 0.55 : 0.45
                }
                Rectangle {
                    anchors.fill: parent
                    visible: backdropSource.status !== Image.Ready
                    gradient: Gradient {
                        GradientStop { position: 0; color: Qt.alpha(Theme.seedColor(dp.title, 0.4), 0.55) }
                        GradientStop { position: 1; color: "transparent" }
                    }
                }

                GridLayout {
                    id: content
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.topMargin: 44
                    anchors.leftMargin: 20
                    anchors.rightMargin: 20
                    columns: dp.wide ? 2 : 1
                    columnSpacing: 20
                    rowSpacing: 12
                    CoverImage {
                        readonly property real side: dp.wide ? 150 : Math.min(hero.width * 0.5, 200)
                        Layout.preferredWidth: side
                        Layout.preferredHeight: side
                        Layout.alignment: dp.wide ? Qt.AlignTop : Qt.AlignHCenter
                        radius: dp.round ? side / 2 : 16
                        placeholder: dp.placeholder
                        seed: dp.title
                        source: dp.cover
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.alignment: Qt.AlignBottom
                        spacing: 4
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: dp.title
                            horizontalAlignment: dp.wide ? Text.AlignLeft : Text.AlignHCenter
                            font.pixelSize: dp.wide ? 26 : 22
                            font.weight: Font.Bold
                            wrapMode: Text.WordWrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                            color: Theme.text
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: dp.subtitle
                            horizontalAlignment: dp.wide ? Text.AlignLeft : Text.AlignHCenter
                            color: Theme.textDim
                            elide: Text.ElideRight
                        }
                        RowLayout {
                            Layout.alignment: dp.wide ? Qt.AlignLeft : Qt.AlignHCenter
                            Layout.topMargin: 10
                            spacing: 8
                            IconButton {
                                iconName: "shuffle"; tip: Theme.tr("Shuffle")
                                enabled: dp.ids.length > 0
                                onClicked: page.play(dp.ids, true)
                            }
                            AccentButton {
                                text: Theme.tr("Play"); highlighted: true; icon.source: Theme.icon("play_arrow")
                                enabled: dp.ids.length > 0
                                implicitWidth: Math.max(implicitContentWidth + leftPadding + rightPadding, 140)
                                onClicked: page.play(dp.ids, false)
                            }
                            IconButton {
                                id: moreButton
                                iconName: "more_vert"; tip: Theme.tr("More")
                                onClicked: songMenu.openFor(dp.ids, moreButton, dp.menuItems, { title: dp.title, subtitle: dp.subtitle })
                            }
                        }
                    }
                }
                Loader {
                    id: extraHolder
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: content.bottom
                    anchors.topMargin: 16
                    anchors.leftMargin: 20
                    anchors.rightMargin: 20
                    sourceComponent: dp.extra
                }
            }
        }

        // Top bar: back button, title once the header scrolled away.
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            height: 44
            readonly property bool solid: list.contentY - list.originY > 200
            color: solid ? Theme.surfaceStrong : "transparent"
            Behavior on color { ColorAnimation { duration: Theme.ms(150) } }
            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 2
                anchors.rightMargin: 8
                IconButton {
                    iconName: "arrow_back"
                    tip: Theme.tr("Back")
                    onClicked: stack.pop()
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: dp.title
                    opacity: parent.parent.solid ? 1 : 0
                    Behavior on opacity { NumberAnimation { duration: Theme.ms(150) } }
                    font.pixelSize: 17
                    font.weight: Font.DemiBold
                    elide: Text.ElideRight
                    color: Theme.text
                }
            }
        }
    }

    Component {
        id: labelView
        DetailPage {
            id: lv
            property string label
            tracks: Core.call("labels.tracks", { name: label }) || []
            title: Theme.labelName(label)
            subtitle: page.count(tracks.length, "song", "songs")
            placeholder: "label"
            menuItems: [
                { text: Theme.tr("Start radio"), action: () => Core.call("songs.radio", { seed: { label: lv.label }, name: Theme.labelName(lv.label) }) },
                page.pinItem("label", lv.label, Theme.labelName(lv.label))
            ]
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
        }
    }

    Component {
        id: mixView
        DetailPage {
            property string mixId
            property string mixName
            tracks: Core.call("mix.tracks", { id: mixId }) || []
            title: mixName
            subtitle: page.count(tracks.length, "song", "songs")
            placeholder: "queue_music"
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
        }
    }

    SmartListDialog { id: smartDialog }

    Component {
        id: albumView
        DetailPage {
            property var album: ({})
            tracks: Core.call("library.album", { id: album.id }) || []
            title: page.albumTitle(album)
            subtitle: [page.artistName(album.artist), album.year, page.count(tracks.length, "song", "songs")].filter(x => x).join(" · ")
            cover: album.coverTrack ? Covers.url(album.coverTrack, album.cover) : (album.cover || "")
            numbered: true
            menuItems: [
                { text: Theme.tr("Start radio"), action: () => Core.call("songs.radio", { seed: { album: album.id }, name: page.albumTitle(album) }) },
                page.pinItem("album", album.id, page.albumTitle(album))
            ]
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
        }
    }

    // An album or playlist of a server extension: its songs are imported one
    // by one as they start.
    Component {
        id: collectionView
        DetailPage {
            id: collectionPage
            property string ext
            property string itemId
            readonly property int rev: (Core.state.extensions || {}).rev || 0
            property var info: ({})
            function reload() {
                const d = Core.call("extensions.collectionData") || {}
                if (d.ext === ext && d.id === itemId)
                    info = d
            }
            onRevChanged: reload()
            tracks: (info.items || []).map(r => ({ id: r.songId, title: r.title, artist: r.artist, album: r.album,
                                                   duration: r.duration, cover: r.image }))
            title: info.title || (info.loading ? Theme.tr("Loading…") : info.error ? Theme.tr("Could not load this") : "")
            subtitle: [info.artist, info.detail, tracks.length > 0 ? page.count(tracks.length, "song", "songs") : ""].filter(x => x).join(" · ")
            cover: info.image || ""
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
            readonly property var waiting: ids.filter(id => String(id).indexOf("extitem:") === 0)
            extra: Component {
                Flow {
                    AccentButton {
                        visible: collectionPage.waiting.length > 0
                        text: Theme.tr("Add all to library")
                        highlighted: true
                        icon.source: Theme.icon("add")
                        onClicked: {
                            enabled = false
                            Core.call("extensions.add", { ids: collectionPage.waiting })
                        }
                    }
                }
            }
        }
    }

    Component {
        id: artistView
        DetailPage {
            property string artist
            tracks: Core.call("library.artist", { name: artist }) || []
            title: page.artistName(artist)
            subtitle: page.count(tracks.length, "song", "songs")
            id: artistPage
            placeholder: "person"
            round: true
            // Its picture from the internet, where allowed and found.
            property string picture: ""
            cover: picture !== "" ? picture : tracks.length > 0 ? Covers.url(tracks[0].id, tracks[0].cover) : ""
            extra: Component {
                ArtistInfoPanel {
                    name: artistPage.artist
                    known: page.artists
                    onImageChanged: artistPage.picture = image
                    onOpenArtist: (n) => stack.push(artistView, { artist: n })
                }
            }
            menuItems: [
                { text: Theme.tr("Start radio"), action: () => Core.call("songs.radio", { seed: { artist: artist }, name: page.artistName(artist) }) },
                page.pinItem("artist", artist, page.artistName(artist))
            ]
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
        }
    }

    Component {
        id: playlistView
        DetailPage {
            id: pv
            property var playlist: ({})
            function reload() {
                const r = Core.call("playlist.items", { id: playlist.id })
                tracks = Array.isArray(r) ? r : []
                const fresh = (Core.call("playlists.list") || []).find(p => p.id === playlist.id)
                if (fresh)
                    playlist = fresh
            }
            Component.onCompleted: reload()
            Connections { target: page; function onPlaylistsRevChanged() { pv.reload() } }
            title: playlist.name || ""
            subtitle: page.count(tracks.length, "song", "songs")
            placeholder: playlist.fixed ? "favorite" : playlist.smart ? "smart_playlist" : "queue_music"
            cover: playlist.cover || ""
            menuItems: playlist.smart ? [
                { text: Theme.tr("Edit rules"), action: () => smartDialog.edit(pv.playlist.rules) },
                { text: Theme.tr("Delete"), action: () => { Core.call("smart.delete", { id: pv.playlist.id }); stack.pop() } }
            ] : playlist.fixed ? [
                { text: Theme.tr("Export as file…"), action: () => page.exportPlaylist(pv.playlist.id) }
            ] : [
                { text: Theme.tr("Rename"), action: () => renamePrompt.open() },
                { text: Theme.tr("Export as file…"), action: () => page.exportPlaylist(pv.playlist.id) },
                { text: Theme.tr("Delete"), action: () => { Core.call("playlist.delete", { id: pv.playlist.id }); stack.pop() } }
            ]
            onTrackMenu: (t, a) => {
                if (pv.playlist.smart)
                    return songMenu.openFor([t.id], a)
                const i = pv.tracks.indexOf(t)
                songMenu.openFor([t.id], a, [{ text: Theme.tr("Remove"), action: () => Core.call("playlist.removeItem", { id: pv.playlist.id, index: i }) }])
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
