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
        playlists = (Core.call("playlists.list") || []).filter(p => p.kind === "track")
    }
    function openVault() {
        if (!(stack.currentItem instanceof VaultPage))
            stack.push(vaultView)
    }
    function back() {
        if (stack.depth > 1) {
            stack.pop()
            return true
        }
        return false
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

    TrackMenu { id: songMenu; onAddToPlaylist: (ids) => picker.show(ids) }
    PlaylistPicker { id: picker }
    TextPrompt {
        id: newPlaylist
        title: Theme.tr("New playlist")
        placeholder: Theme.tr("Playlist name")
        acceptText: Theme.tr("Create")
        onSubmitted: (t) => Core.call("playlist.create", { name: t, kind: "track", server: Core.state.server === "connected" })
    }

    StackView {
        id: stack
        anchors.fill: parent
        initialItem: mainView
    }

    Component {
        id: vaultView
        VaultPage {
            back: true
            onBackClicked: stack.pop()
            onUpload: page.uploadVault()
        }
    }

    // ---- building blocks --------------------------------------------------

    // Filter chip of the section bar.
    component Chip: AbstractButton {
        id: chip
        implicitHeight: 36
        implicitWidth: implicitContentWidth + leftPadding + rightPadding
        leftPadding: 12
        rightPadding: 12
        font.pixelSize: 14
        font.weight: Font.Medium
        Accessible.name: text
        contentItem: Label {
            textFormat: Text.PlainText
            text: chip.text
            font: chip.font
            verticalAlignment: Text.AlignVCenter
            color: chip.checked ? Theme.onAccent : Theme.text
        }
        background: Rectangle {
            radius: height / 2
            color: chip.checked ? Theme.accent : chip.down ? Theme.outline : Theme.surfaceVariant
            border.width: chip.checked ? 0 : 1
            border.color: Theme.outline
            Behavior on color { ColorAnimation { duration: 120 } }
        }
    }

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
        height: 72
        leftPadding: 16
        rightPadding: 8
        Accessible.name: title
        contentItem: RowLayout {
            spacing: 14
            Item { id: leadBox; Layout.preferredWidth: 52; Layout.preferredHeight: 52 }
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
            readonly property bool searching: search.text.trim().length > 0

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
                    Menu {
                        id: addMenu
                        MenuItem { text: Theme.tr("Add music folder"); icon.source: Theme.icon("create_new_folder"); onTriggered: page.addFolder() }
                        MenuItem { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onTriggered: page.openFiles() }
                        MenuItem { text: Theme.tr("Connect server"); icon.source: Theme.icon("cloud"); onTriggered: page.connectServer() }
                        MenuItem { text: Theme.tr("Rescan"); icon.source: Theme.icon("refresh"); onTriggered: Core.call("library.rescan") }
                    }
                }
            }

            // Search: a filled pill instead of an outlined form field.
            TextField {
                id: search
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                Layout.topMargin: 4
                visible: page.trackCount > 0
                implicitHeight: 46
                leftPadding: 46
                rightPadding: clearButton.visible ? 46 : 16
                topPadding: 0
                bottomPadding: 0
                verticalAlignment: TextInput.AlignVCenter
                font.pixelSize: 15
                color: Theme.text
                inputMethodHints: Qt.ImhNoPredictiveText
                Accessible.name: Theme.tr("Search music")
                background: Rectangle {
                    radius: height / 2
                    color: Theme.surfaceVariant
                    border.width: search.activeFocus ? 2 : 1
                    border.color: search.activeFocus ? Theme.accent : Theme.outline
                }
                Icon {
                    anchors.verticalCenter: parent.verticalCenter
                    x: 14
                    name: "search"
                    size: 22
                    color: search.activeFocus ? Theme.accent : Theme.textDim
                }
                Label {
                    textFormat: Text.PlainText
                    anchors.verticalCenter: parent.verticalCenter
                    x: search.leftPadding
                    width: search.width - search.leftPadding - 16
                    visible: search.text.length === 0
                    text: Theme.tr("Search music")
                    color: Theme.textFaint
                    font.pixelSize: 15
                    elide: Text.ElideRight
                }
                IconButton {
                    id: clearButton
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    visible: search.text.length > 0
                    iconName: "close"
                    iconSize: 20
                    tip: Theme.tr("Clear")
                    onClicked: { search.clear(); search.focus = false }
                }
                onTextChanged: searchTimer.restart()
                Timer {
                    id: searchTimer
                    interval: 180
                    onTriggered: page.results = main.searching ? (Core.call("library.search", { query: search.text }) || []) : []
                }
            }

            // Sections as filter chips (scroll sideways on narrow screens).
            Flickable {
                id: chips
                Layout.fillWidth: true
                Layout.preferredHeight: 36
                Layout.topMargin: 10
                Layout.bottomMargin: 6
                visible: !main.searching && page.trackCount > 0
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
                        model: [Theme.tr("Songs"), Theme.tr("Albums"), Theme.tr("Artists"), Theme.tr("Folders"), Theme.tr("Playlists")]
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
                    AccentButton { text: Theme.tr("Add music folder"); highlighted: true; icon.source: Theme.icon("create_new_folder"); onClicked: page.addFolder() }
                    Button { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onClicked: page.openFiles() }
                    Button { text: Theme.tr("Connect server"); flat: true; icon.source: Theme.icon("cloud"); onClicked: page.connectServer() }
                    Button {
                        visible: (Core.state.capture || {}).available === true
                        text: Theme.tr("Visualize other apps")
                        flat: true
                        icon.source: Theme.icon("graphic_eq")
                        onClicked: Core.call("capture.start")
                    }
                }

                // Search results
                Item {
                    anchors.fill: parent
                    visible: main.searching
                    TrackListView {
                        anchors.fill: parent
                        anchors.topMargin: 8
                        tracks: page.results
                        onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                    }
                    EmptyState {
                        anchors.centerIn: parent
                        width: Math.min(parent.width - 48, 420)
                        visible: page.results.length === 0 && !searchTimer.running
                        iconName: "search"
                        title: Theme.tr("No results")
                        text: Theme.tr("Try a different title, artist or album.")
                    }
                }

                StackLayout {
                    anchors.fill: parent
                    visible: !main.searching && page.trackCount > 0
                    currentIndex: main.section

                    // Songs: play/shuffle scroll away with the list.
                    TrackListView {
                        tracks: page.tracks
                        onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                        header: Item {
                            width: ListView.view.width
                            height: 60
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                spacing: 8
                                AccentButton {
                                    text: Theme.tr("Play all"); icon.source: Theme.icon("play_arrow"); highlighted: true
                                    onClicked: page.play(page.tracks.map(t => t.id), false)
                                }
                                Button {
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
                        readonly property int cols: Math.max(2, Math.floor(avail / 172))
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
                            header: ListRow {
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
                            delegate: ListRow {
                                id: playlistRow
                                required property var modelData
                                title: modelData.name
                                subtitle: page.count(modelData.count, "song", "songs") + (modelData.server ? " · " + Theme.tr("Server") : "")
                                onClicked: stack.push(playlistView, { playlist: modelData })
                                lead: CoverImage {
                                    anchors.fill: parent
                                    radius: 12
                                    placeholder: "queue_music"
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
        signal trackMenu(var track, Item anchor)
        readonly property var ids: tracks.map(t => t.id)
        readonly property bool wide: width >= 640

        TrackListView {
            id: list
            anchors.fill: parent
            tracks: dp.tracks
            numbered: dp.numbered
            showCovers: !dp.numbered
            topMargin: 0
            bottomMargin: 8
            onTrackMenu: (t, a) => dp.trackMenu(t, a)
            header: Item {
                id: hero
                width: ListView.view.width
                height: content.implicitHeight + 64

                // Ambient backdrop: the cover, blurred and fading out.
                Image {
                    id: backdropSource
                    anchors.fill: parent
                    source: dp.cover
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
                    anchors.topMargin: 56
                    anchors.leftMargin: 20
                    anchors.rightMargin: 20
                    columns: dp.wide ? 2 : 1
                    columnSpacing: 24
                    rowSpacing: 16
                    CoverImage {
                        readonly property real side: dp.wide ? 200 : Math.min(hero.width * 0.58, 240)
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
                            font.pixelSize: dp.wide ? 32 : 24
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
                                onClicked: songMenu.openFor(dp.ids, moreButton, dp.menuItems)
                            }
                        }
                    }
                }
            }
        }

        // Top bar: back button, title once the header scrolled away.
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            height: 52
            readonly property bool solid: list.contentY - list.originY > 200
            color: solid ? Theme.surfaceStrong : "transparent"
            Behavior on color { ColorAnimation { duration: 150 } }
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
                    Behavior on opacity { NumberAnimation { duration: 150 } }
                    font.pixelSize: 17
                    font.weight: Font.DemiBold
                    elide: Text.ElideRight
                    color: Theme.text
                }
            }
        }
    }

    Component {
        id: albumView
        DetailPage {
            property var album: ({})
            tracks: Core.call("library.album", { id: album.id }) || []
            title: page.albumTitle(album)
            subtitle: [page.artistName(album.artist), album.year, page.count(tracks.length, "song", "songs")].filter(x => x).join(" · ")
            cover: album.coverTrack ? Covers.url(album.coverTrack, album.cover) : (album.cover || "")
            numbered: true
            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
        }
    }

    Component {
        id: artistView
        DetailPage {
            property string artist
            tracks: Core.call("library.artist", { name: artist }) || []
            title: page.artistName(artist)
            subtitle: page.count(tracks.length, "song", "songs")
            placeholder: "person"
            round: true
            cover: tracks.length > 0 ? Covers.url(tracks[0].id, tracks[0].cover) : ""
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
            placeholder: "queue_music"
            cover: playlist.cover || ""
            menuItems: [
                { text: Theme.tr("Rename"), action: () => renamePrompt.open() },
                { text: Theme.tr("Delete"), action: () => { Core.call("playlist.delete", { id: pv.playlist.id }); stack.pop() } }
            ]
            onTrackMenu: (t, a) => {
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
