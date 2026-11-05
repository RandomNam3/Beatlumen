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

    readonly property int trackCount: Core.state.libraryTracks || 0
    readonly property var current: Core.state.current || null
    readonly property bool hasContent: trackCount > 0 || ((Core.state.sources || {}).stations || []).length > 0
    property var overview: ({})
    readonly property var recent: overview.recent || []
    readonly property var fresh: overview.new || []
    readonly property var favorites: overview.favorites || []

    function reload() {
        overview = Core.call("home.overview") || {}
    }
    Component.onCompleted: reload()
    // Several of these change at once (a song ends, the library arrives).
    readonly property string revisions: [Core.state.historyRev, Core.state.favoritesRev, Core.state.libraryRev].join()
    onRevisionsChanged: if (visible) reloadTimer.restart()
    onVisibleChanged: if (visible) reloadTimer.restart()
    Timer { id: reloadTimer; interval: 150; onTriggered: page.reload() }

    function ids(list) {
        return list.map(t => t.id)
    }
    function artistOf(t) {
        return t.artist || Theme.tr("Unknown artist")
    }

    TrackMenu { id: songMenu; onAddToPlaylist: (ids) => picker.show(ids) }
    PlaylistPicker { id: picker }

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
            }

            EmptyState {
                Layout.fillWidth: true
                Layout.leftMargin: 24
                Layout.rightMargin: 24
                Layout.topMargin: 32
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
                            color: Theme.accent
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

            // Room to scroll past the last row.
            Item { Layout.preferredHeight: 8 }
        }
    }
}
