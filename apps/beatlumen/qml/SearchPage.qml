import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Search: the library and, next to it, server extensions; or one extension
// alone. Before typing, labels to browse.
Item {
    id: page
    signal openAlbum(var album)
    signal openArtist(string name)
    signal openLabel(string name)
    signal openCollection(string ext, string id)

    readonly property string query: search.text.trim()
    readonly property bool searching: query.length > 0
    // Server extensions that search (e.g. YouTube) and the search's source.
    readonly property var extList: (Core.state.extensions || {}).list || []
    property string source: "library"
    onExtListChanged: if (source !== "library" && !extList.some(e => e.id === source)) source = "library"

    property var songs: []
    property var albums: []
    property var artists: []
    property var labels: []

    function focusField() {
        search.forceActiveFocus()
        search.selectAll()
    }
    function run() {
        if (!searching) {
            songs = []
            albums = []
            artists = []
            return
        }
        const q = query.toLowerCase()
        songs = Core.call("library.search", { query: query }) || []
        albums = (Core.call("library.albums") || []).filter(a => (a.title || "").toLowerCase().indexOf(q) >= 0
            || (a.artist || "").toLowerCase().indexOf(q) >= 0).slice(0, 24)
        artists = (Core.call("library.artists") || []).filter(a => (a.name || "").toLowerCase().indexOf(q) >= 0).slice(0, 24)
    }
    function reloadLabels() {
        labels = (Core.call("labels.list") || []).filter(l => {
            const kind = Theme.labelKind(l.name)
            return kind === "" || kind === "genre" || kind === "mood" || kind === "decade"
        })
    }
    Component.onCompleted: reloadLabels()
    onVisibleChanged: if (visible) { reloadLabels(); focusField() }
    readonly property int labelsRev: Core.state.labelsRev || 0
    onLabelsRevChanged: reloadLabels()

    TrackMenu { id: songMenu; onAddToPlaylist: (ids) => picker.show(ids); canEditLabels: true; onEditLabels: (id) => labelsDialog.show(id) }
    PlaylistPicker { id: picker }
    LabelsDialog { id: labelsDialog }

    ColumnLayout {
        anchors.fill: parent
        spacing: 8

        PageHeader {
            Layout.fillWidth: true
            title: Theme.tr("Search")
        }

        // A filled pill instead of an outlined form field.
        InputField {
            id: search
            Layout.fillWidth: true
            Layout.leftMargin: 12
            Layout.rightMargin: 12
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
                color: Theme.textDim
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
                onClicked: { search.clear(); search.forceActiveFocus() }
            }
            onTextChanged: searchTimer.restart()
            Timer {
                id: searchTimer
                interval: 180
                onTriggered: page.run()
            }
        }

        // Where to search.
        Row {
            Layout.leftMargin: 12
            spacing: 6
            visible: page.extList.length > 0
            Chip {
                text: Theme.tr("All")
                checked: page.source === "library"
                onClicked: page.source = "library"
            }
            Repeater {
                model: page.extList
                Chip {
                    required property var modelData
                    text: modelData.name
                    checked: page.source === modelData.id
                    onClicked: page.source = modelData.id
                }
            }
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            // ---- before typing: labels to browse --------------------------------
            ScrollView {
                id: browse
                anchors.fill: parent
                visible: !page.searching && page.source === "library"
                clip: true
                contentWidth: availableWidth
                ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                GridLayout {
                    width: browse.availableWidth - 24
                    x: 12
                    columns: Math.max(2, Math.floor(width / 170))
                    columnSpacing: 10
                    rowSpacing: 10
                    Repeater {
                        model: page.labels
                        AbstractButton {
                            id: tile
                            required property var modelData
                            Layout.fillWidth: true
                            implicitHeight: Theme.fingers ? 64 : 48
                            focusPolicy: Qt.TabFocus
                            text: Theme.labelName(modelData.name)
                            Accessible.role: Accessible.Button
                            Accessible.name: text
                            onClicked: page.openLabel(modelData.name)
                            Keys.onReturnPressed: clicked()
                            Keys.onEnterPressed: clicked()
                            background: Rectangle {
                                radius: Theme.smallRadius
                                color: Theme.seedColor(tile.modelData.name, tile.down ? 0.20 : tile.hovered ? 0.36 : 0.28)
                            }
                            contentItem: Label {
                                textFormat: Text.PlainText
                                leftPadding: 14
                                rightPadding: 14
                                verticalAlignment: Text.AlignVCenter
                                text: tile.text
                                color: Theme.text
                                font.pixelSize: 15
                                font.weight: Font.DemiBold
                                elide: Text.ElideRight
                            }
                            FocusRing { shown: tile.visualFocus; radius: Theme.smallRadius + 2 }
                        }
                    }
                }
            }

            // ---- library results ----------------------------------------------
            ScrollView {
                id: results
                anchors.fill: parent
                visible: page.searching && page.source === "library"
                clip: true
                contentWidth: availableWidth
                ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                ColumnLayout {
                    width: results.availableWidth
                    spacing: 16

                    EmptyState {
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 24
                        Layout.topMargin: 16
                        visible: page.songs.length === 0 && page.albums.length === 0 && page.artists.length === 0 && !searchTimer.running && mixed.total === 0 && !mixed.searching
                        iconName: "search"
                        title: Theme.tr("No results")
                        text: Theme.tr("Try a different title, artist or album.")
                    }

                    Shelf {
                        Layout.fillWidth: true
                        title: Theme.tr("Artists")
                        items: page.artists
                        delegate: ShelfCard {
                            required property var modelData
                            focus: ListView.isCurrentItem
                            side: 120
                            title: modelData.name || Theme.tr("Unknown artist")
                            placeholder: "person"
                            onClicked: page.openArtist(modelData.name)
                        }
                    }
                    Shelf {
                        Layout.fillWidth: true
                        title: Theme.tr("Albums")
                        items: page.albums
                        delegate: ShelfCard {
                            required property var modelData
                            readonly property string coverId: modelData.coverTrack || ""
                            focus: ListView.isCurrentItem
                            side: 120
                            title: modelData.title || ""
                            subtitle: modelData.artist || ""
                            placeholder: "album"
                            cover: Covers.url(coverId, modelData.cover)
                            Component.onCompleted: if (coverId && !modelData.cover) Covers.request(coverId)
                            onClicked: page.openAlbum(modelData)
                        }
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        visible: page.songs.length > 0
                        spacing: 4
                        Label {
                            textFormat: Text.PlainText
                            Layout.leftMargin: 12
                            text: Theme.tr("Songs")
                            color: Theme.text
                            font.pixelSize: 20
                            font.weight: Font.DemiBold
                            Accessible.role: Accessible.Heading
                            Accessible.name: text
                        }
                        TrackListView {
                            Layout.fillWidth: true
                            Layout.preferredHeight: contentHeight
                            interactive: false
                            tracks: page.songs.slice(0, 100)
                            onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                        }
                    }
                    // What extensions found, next to the library's songs.
                    ColumnLayout {
                        id: mixed
                        Layout.fillWidth: true
                        spacing: 16
                        readonly property var ext: Core.state.extensions || ({})
                        readonly property bool searching: ext.searching === true
                        property var found: []
                        readonly property int total: found.length
                        readonly property int rev: ext.rev || 0
                        onRevChanged: found = Core.call("extensions.results") || []
                        // Every search is a request to the outside world: wait
                        // for a pause in the typing, and for more than a letter.
                        Timer {
                            id: mixedTimer
                            interval: 600
                            onTriggered: Core.call("extensions.search", { query: page.query.length >= 3 ? page.query : "" })
                        }
                        Connections {
                            target: page
                            function onQueryChanged() {
                                if (page.extList.length > 0)
                                    mixedTimer.restart()
                            }
                        }
                        Repeater {
                            model: page.extList
                            ColumnLayout {
                                id: section
                                required property var modelData
                                readonly property var mine: mixed.found.filter(r => r.ext === modelData.id)
                                Layout.fillWidth: true
                                visible: mine.length > 0
                                spacing: 4
                                RowLayout {
                                    Layout.fillWidth: true
                                    Layout.leftMargin: 12
                                    Layout.rightMargin: 8
                                    Label {
                                        textFormat: Text.PlainText
                                        Layout.fillWidth: true
                                        text: section.modelData.name
                                        color: Theme.text
                                        font.pixelSize: 20
                                        font.weight: Font.DemiBold
                                        Accessible.role: Accessible.Heading
                                        Accessible.name: text
                                    }
                                    Button {
                                        flat: true
                                        visible: section.mine.length > 6
                                        text: Theme.tr("Show all")
                                        onClicked: page.source = section.modelData.id
                                    }
                                }
                                // A column, not the layout: a row's width follows its
                                // list, and from its own content that never settles.
                                Column {
                                    id: sectionRows
                                    Layout.fillWidth: true
                                    Repeater {
                                        model: section.mine.slice(0, 6)
                                        ExtensionRow {
                                            width: sectionRows.width
                                            onOpenCollection: (entry) => page.openCollection(entry.ext, entry.id)
                                            onMenu: (t, a) => songMenu.openFor([t.id], a)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---- a server extension's results --------------------------------
            Loader {
                anchors.fill: parent
                active: page.searching && page.source !== "library"
                sourceComponent: ExtensionSearch {
                    query: page.query
                    only: page.source
                    onOpenCollection: (entry) => page.openCollection(entry.ext, entry.id)
                    onTrackMenu: (t, a) => songMenu.openFor([t.id], a)
                }
            }
        }
    }
}
