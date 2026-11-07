import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The start page: where you left off, what you listened to, what is new,
// your favorites.
Item {
    id: page
    signal addFolder()
    signal connectServer()
    signal openFiles()
    signal openAlbum(var album)
    signal openFavorites()
    signal openLabel(string name)
    signal openCollection(string ext, string id)
    signal startTour()

    readonly property int trackCount: Core.state.libraryTracks || 0
    readonly property var current: Core.state.current || null
    readonly property bool hasContent: trackCount > 0 || ((Core.state.sources || {}).stations || []).length > 0
    property var overview: ({})
    readonly property var recent: overview.recent || []
    readonly property var fresh: overview.new || []
    readonly property var favorites: overview.favorites || []
    readonly property var suggestions: overview.shelves || []
    readonly property var mixes: overview.mixes || []

    function reload() {
        overview = Core.call("home.overview") || {}
        // Chips: moods first, then genres; only what has songs.
        const labels = Core.call("labels.list") || []
        const of = (kind) => labels.filter(l => Theme.labelKind(l.name) === kind)
        chips = of("mood").concat(of("genre")).slice(0, 10)
    }
    property var chips: []
    // A dense grid of what is likely wanted again, no two the same.
    readonly property int quickColumns: width >= 1000 ? 4 : width >= 620 ? 3 : 2
    readonly property var quick: {
        const seen = {}
        return recent.concat(favorites).filter(t => {
            if (seen[t.id])
                return false
            seen[t.id] = true
            return true
        }).slice(0, quickColumns * 2)
    }
    Component.onCompleted: reload()
    // Several of these change at once (a song ends, the library arrives).
    readonly property string revisions: [Core.state.historyRev, Core.state.favoritesRev, Core.state.libraryRev, Core.state.recommendRev, Core.state.labelsRev].join()
    onRevisionsChanged: if (visible) reloadTimer.restart()
    onVisibleChanged: {
        if (visible)
            reloadTimer.restart()
        reloadExtensions()
    }
    Timer { id: reloadTimer; interval: 150; onTriggered: page.reload() }

    // Rows of suggestions from server extensions.
    readonly property bool extOn: ((Core.state.settings || {}).ui || {}).extShelves !== false
    readonly property int extRev: (Core.state.extensions || {}).rev || 0
    property var extShelves: []
    function reloadExtensions() {
        extShelves = visible && extOn ? (Core.call("extensions.shelves") || []) : []
    }
    onExtRevChanged: reloadExtensions()
    onExtOnChanged: reloadExtensions()

    function ids(list) {
        return list.map(t => t.id)
    }
    function artistOf(t) {
        return t.artist || Theme.tr("Unknown artist")
    }
    function shelfTitle(s) {
        switch (s.key) {
        case "for-you": return Theme.tr("For you")
        case "because": return Theme.tr("Because you listened to %1").arg(s.argTitle || "")
        case "new": return Theme.tr("New for you")
        case "forgotten": return Theme.tr("Not heard for a while")
        default: return Theme.tr("To discover")
        }
    }
    function reasonText(t) {
        switch (t.reason) {
        case "artist": return Theme.tr("You listen to %1").arg(Theme.capitalized(t.reasonArg))
        case "genre": return Theme.tr("You like %1").arg(Theme.capitalized(t.reasonArg))
        case "played-with": return Theme.tr("Often heard with %1").arg(t.reasonArg)
        default: return ""
        }
    }
    function mixName(m) {
        switch (m.key) {
        case "genre":
        case "artist": return Theme.tr("%1 mix").arg(Theme.capitalized(m.arg))
        case "discover": return Theme.tr("Discovery mix")
        default: return Theme.tr("Forgotten favorites")
        }
    }

    TrackActions { id: songMenu; onAddToPlaylist: (ids) => picker.show(ids); canEditLabels: true; onEditLabels: (id) => labelsDialog.show(id) }
    LabelsDialog { id: labelsDialog }
    PlaylistPicker { id: picker }

    // One small entry of the grid.
    component QuickCard: ItemDelegate {
        id: qc
        required property var modelData
        readonly property string artistLine: page.artistOf(modelData)
        Layout.fillWidth: true
        Layout.preferredWidth: 1
        implicitHeight: 56
        padding: 0
        Accessible.name: [modelData.title, artistLine].filter(x => x).join(", ")
        background: Rectangle {
            radius: Theme.smallRadius
            color: qc.down ? Theme.outline : qc.hovered ? Theme.outline : Theme.surfaceVariant
        }
        onClicked: Core.call("play", { ids: page.ids(page.quick), start: page.quick.indexOf(modelData) })
        onPressAndHold: songMenu.openFor([modelData.id], qc)
        TapHandler {
            acceptedButtons: Qt.RightButton
            onTapped: songMenu.openFor([qc.modelData.id], qc)
        }
        contentItem: RowLayout {
            spacing: 10
            CoverImage {
                Layout.preferredWidth: 56
                Layout.preferredHeight: 56
                radius: Theme.smallRadius
                source: Covers.url(qc.modelData.id, qc.modelData.cover)
                seed: qc.modelData.album || qc.modelData.title || ""
                Component.onCompleted: if ((qc.modelData.local || qc.modelData.source) && !qc.modelData.cover) Covers.request(qc.modelData.id)
            }
            ColumnLayout {
                Layout.fillWidth: true
                Layout.rightMargin: 8
                spacing: 0
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: qc.modelData.title || ""; color: Theme.text; font.pixelSize: 13; font.weight: Font.Medium; elide: Text.ElideRight }
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: qc.artistLine; color: Theme.textDim; font.pixelSize: 12; elide: Text.ElideRight }
            }
        }
    }

    component SongCard: ShelfCard {
        required property var modelData
        required property int index
        property var row: []
        focus: ListView.isCurrentItem
        title: modelData.title || ""
        subtitle: page.artistOf(modelData)
        seed: modelData.album || modelData.title || ""
        cover: Covers.url(modelData.id, modelData.cover)
        Component.onCompleted: if ((modelData.local || modelData.source) && !modelData.cover) Covers.request(modelData.id)
        onClicked: Core.call("play", { ids: page.ids(row), start: index })
        onMenuRequested: (anchor) => songMenu.openFor([modelData.id], anchor)
    }

    ScrollView {
        id: scroll
        anchors.fill: parent
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        contentWidth: availableWidth
        clip: true

        ColumnLayout {
            width: scroll.availableWidth
            spacing: 24

            PageHeader {
                Layout.fillWidth: true
                title: Theme.tr("Home")
                AccentButton {
                    visible: page.hasContent
                    Layout.rightMargin: 12
                    text: Theme.tr("My radio")
                    highlighted: true
                    icon.source: Theme.icon("radio")
                    onClicked: Core.call("songs.radio", { seed: { taste: true }, name: Theme.tr("My radio") })
                }
            }

            // Moods and genres to go to directly.
            Flickable {
                Layout.fillWidth: true
                Layout.preferredHeight: 32
                visible: page.chips.length > 0
                contentWidth: chipRow.width + 24
                flickableDirection: Flickable.HorizontalFlick
                boundsBehavior: Flickable.StopAtBounds
                clip: true
                Row {
                    id: chipRow
                    x: 12
                    spacing: 6
                    Repeater {
                        model: page.chips
                        Chip {
                            required property var modelData
                            text: Theme.labelName(modelData.name)
                            onClicked: page.openLabel(modelData.name)
                        }
                    }
                }
            }

            GridLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                visible: page.quick.length > 0
                columns: page.quickColumns
                columnSpacing: 8
                rowSpacing: 8
                Repeater {
                    model: page.quick
                    QuickCard { }
                }
            }

            EmptyState {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 24
                Layout.topMargin: 16
                visible: !page.hasContent
                iconName: "library_music"
                title: Core.state.scanning ? Theme.tr("Scanning library…") : Theme.tr("Your library is empty")
                text: Theme.tr("Add a music folder or connect to a Beatlumen server.")
                AccentButton {
                    visible: Core.platform !== "web"
                    text: Theme.tr("Add music folder")
                    highlighted: true
                    icon.source: Theme.icon("create_new_folder")
                    onClicked: page.addFolder()
                }
                Button { text: Theme.tr("Open files"); icon.source: Theme.icon("folder_open"); onClicked: page.openFiles() }
                Button { text: Theme.tr("Connect server"); flat: true; icon.source: Theme.icon("cloud"); onClicked: page.connectServer() }
            }

            // Who used the app before the setup came gets a hint, once.
            Rectangle {
                readonly property var ui: (Core.state.settings || {}).ui || ({})
                visible: page.hasContent && ui.setupDone !== true
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                implicitHeight: hintRow.implicitHeight + 24
                radius: Theme.radius
                color: Theme.surfaceVariant
                ColumnLayout {
                    id: hintRow
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.margins: 12
                    spacing: 4
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.text
                        text: Theme.tr("New: start page, recommendations, statistics and more visuals.")
                    }
                    Flow {
                        Layout.fillWidth: true
                        spacing: 8
                        AccentButton {
                            text: Theme.tr("Show me around")
                            highlighted: true
                            onClicked: {
                                Core.call("settings.set", { key: "ui.setupDone", value: true })
                                page.startTour()
                            }
                        }
                        Button {
                            flat: true
                            text: Theme.tr("Got it")
                            onClicked: Core.call("settings.set", { key: "ui.setupDone", value: true })
                        }
                    }
                }
            }

            // Where you left off.
            ItemDelegate {
                id: resume
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                visible: page.current !== null && Core.state.play !== "playing" && Core.state.play !== "loading"
                implicitHeight: 88
                padding: 12
                background: Rectangle {
                    radius: Theme.radius
                    color: resume.down ? Theme.outline : Theme.surfaceVariant
                }
                Accessible.name: Theme.tr("Continue listening") + ": " + (page.current ? page.current.title : "")
                onClicked: Core.call("toggle")
                contentItem: RowLayout {
                    spacing: 14
                    CoverImage {
                        Layout.preferredWidth: 64
                        Layout.preferredHeight: 64
                        radius: 10
                        source: page.current ? Covers.url(page.current.id, page.current.cover) : ""
                        seed: page.current ? (page.current.album || page.current.title || "") : ""
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: Theme.tr("Continue listening")
                            color: Theme.accentText
                            font.pixelSize: 13
                            font.weight: Font.DemiBold
                            elide: Text.ElideRight
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: page.current ? page.current.title : ""
                            color: Theme.text
                            font.pixelSize: 16
                            font.weight: Font.Medium
                            elide: Text.ElideRight
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: page.current ? page.artistOf(page.current) : ""
                            color: Theme.textDim
                            font.pixelSize: 13
                            elide: Text.ElideRight
                        }
                    }
                    Icon {
                        name: "play_circle"
                        size: 40
                        color: Theme.accent
                    }
                }
            }

            // The server's suggestions (or those worked out here).
            Repeater {
                model: page.suggestions.slice(0, 1)
                delegate: Shelf {
                    required property var modelData
                    Layout.fillWidth: true
                    title: page.shelfTitle(modelData)
                    items: modelData.items
                    delegate: SongCard { row: modelData.items; note: page.reasonText(modelData) }
                }
            }

            Shelf {
                Layout.fillWidth: true
                title: Theme.tr("Your mixes")
                items: page.mixes
                delegate: ShelfCard {
                    required property var modelData
                    required property int index
                    focus: ListView.isCurrentItem
                    title: page.mixName(modelData)
                    subtitle: Theme.tr("%1 songs").arg(modelData.count)
                    seed: title
                    placeholder: "queue_music"
                    cover: Covers.url(modelData.coverTrack, modelData.cover)
                    Component.onCompleted: if (!modelData.cover) Covers.request(modelData.coverTrack)
                    onClicked: Core.call("mix.play", { id: modelData.id })
                }
            }

            Shelf {
                Layout.fillWidth: true
                title: Theme.tr("Recently played")
                items: page.recent
                delegate: SongCard { row: page.recent }
            }

            Shelf {
                Layout.fillWidth: true
                title: Theme.tr("New in your library")
                items: page.fresh
                delegate: ShelfCard {
                    id: newCard
                    required property var modelData
                    required property int index
                    readonly property bool album: modelData.type === "album"
                    readonly property string coverId: album ? (modelData.coverTrack || "") : modelData.id
                    focus: ListView.isCurrentItem
                    title: modelData.title || ""
                    subtitle: page.artistOf(modelData)
                    seed: album ? modelData.title : (modelData.album || modelData.title || "")
                    placeholder: album ? "album" : "music_note"
                    hasPlay: album
                    cover: Covers.url(coverId, modelData.cover)
                    Component.onCompleted: if (coverId && !modelData.cover) Covers.request(coverId)
                    onClicked: album ? page.openAlbum(modelData) : Core.call("play", { ids: [modelData.id], start: 0 })
                    onPlayClicked: Core.call("play", { ids: page.ids(Core.call("library.album", { id: modelData.id }) || []), start: 0 })
                    onMenuRequested: (anchor) => songMenu.openFor(album ? page.ids(Core.call("library.album", { id: modelData.id }) || []) : [modelData.id], anchor)
                }
            }

            Shelf {
                Layout.fillWidth: true
                title: Theme.tr("Favorites")
                items: page.favorites
                moreText: (page.overview.favoritesCount || 0) > page.favorites.length ? Theme.tr("Show all") : ""
                onMoreClicked: page.openFavorites()
                delegate: SongCard { row: page.favorites }
            }

            Repeater {
                model: page.suggestions.slice(1)
                delegate: Shelf {
                    required property var modelData
                    Layout.fillWidth: true
                    title: page.shelfTitle(modelData)
                    items: modelData.items
                    delegate: SongCard { row: modelData.items; note: page.reasonText(modelData) }
                }
            }

            Repeater {
                model: page.extShelves
                delegate: Shelf {
                    required property var modelData
                    Layout.fillWidth: true
                    title: [modelData.title, modelData.extName].filter(x => x).join(" · ")
                    items: modelData.items
                    delegate: ShelfCard {
                        required property var modelData
                        readonly property bool collection: modelData.kind === "collection"
                        focus: ListView.isCurrentItem
                        title: modelData.title || ""
                        subtitle: modelData.artist || ""
                        placeholder: collection ? "album" : "music_note"
                        cover: modelData.image || ""
                        onClicked: {
                            if (collection)
                                page.openCollection(modelData.ext, modelData.id)
                            else
                                Core.call("extensions.play", { ext: modelData.ext, id: modelData.id })
                        }
                        onMenuRequested: (anchor) => { if (!collection) songMenu.openFor([modelData.songId], anchor) }
                    }
                }
            }

            // Room to scroll past the last row.
            Item { Layout.preferredHeight: 8 }
        }
    }
}
