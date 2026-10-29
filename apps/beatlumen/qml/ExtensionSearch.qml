import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Search results of a server extension (e.g. YouTube). A click plays the
// song right away while the server stores it: in the library on the
// server or in the user's encrypted files (asked once, then remembered).
Item {
    id: es
    property string query
    readonly property var ext: Core.state.extensions || ({})
    readonly property int rev: ext.rev || 0
    property var results: []
    property var pending: null

    onQueryChanged: timer.restart()
    onRevChanged: results = Core.call("extensions.results") || []
    Component.onCompleted: {
        results = Core.call("extensions.results") || []
        timer.restart()
    }
    Timer {
        id: timer
        interval: 450
        onTriggered: Core.call("extensions.search", { query: es.query.trim() })
    }

    function detail(r) {
        const parts = (r.detail || "").split(" · ").filter(x => x).map(x => Theme.tr(x))
        if (r.inLibrary)
            parts.push(Theme.tr("In library"))
        return [r.artist].concat(parts).filter(x => x).join(" · ")
    }
    function play(r) {
        const res = Core.call("extensions.play", { ext: r.ext, id: r.id })
        if (res && res.ask) {
            es.pending = r
            target.open()
        }
    }

    ListView {
        anchors.fill: parent
        anchors.topMargin: 4
        clip: true
        model: es.results
        ScrollBar.vertical: ScrollBar {}
        delegate: TrackDelegate {
            required property var modelData
            track: ({ id: "ext-result:" + modelData.ext + ":" + modelData.id, title: modelData.title,
                      artist: es.detail(modelData), duration: modelData.duration, cover: modelData.image })
            showMenu: false
            onPlay: es.play(modelData)
        }
    }
    BusyIndicator {
        anchors.centerIn: parent
        running: es.ext.searching === true && es.results.length === 0
    }
    EmptyState {
        anchors.centerIn: parent
        width: Math.min(parent.width - 48, 420)
        visible: es.ext.searching !== true && !timer.running && es.results.length === 0 && es.query.trim() !== ""
        iconName: es.ext.error ? "warning" : "search"
        title: es.ext.error ? Theme.tr("Search failed") : Theme.tr("No results")
        text: es.ext.error || Theme.tr("Try a different title, artist or album.")
    }

    AppDialog {
        id: target
        readonly property bool vaultReady: (Core.state.vault || {}).state === "ready"
        function choose(where) {
            const r = es.pending
            close()
            if (r)
                Core.call("extensions.play", { ext: r.ext, id: r.id, target: where, remember: true })
        }
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(460, (es.Window.window ? es.Window.window.width : 460) - 32)
        title: Theme.tr("Where to save songs?")
        standardButtons: Dialog.Cancel
        ColumnLayout {
            anchors.fill: parent
            spacing: 6
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: Theme.tr("The song plays right away while the server stores it. Your choice is remembered; you can change it in the settings.")
            }
            component Choice: ItemDelegate {
                id: ch
                property string heading
                property string detail
                property string iconName
                Layout.fillWidth: true
                Accessible.name: heading
                contentItem: RowLayout {
                    spacing: 14
                    Icon { name: ch.iconName; size: 26; color: ch.enabled ? Theme.accent : Theme.textFaint }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: ch.heading; font.pixelSize: 15; font.weight: Font.Medium; color: ch.enabled ? Theme.text : Theme.textFaint; wrapMode: Text.WordWrap }
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: ch.detail; font.pixelSize: 13; color: Theme.textDim; wrapMode: Text.WordWrap }
                    }
                }
            }
            Choice {
                heading: Theme.tr("Library on the server")
                detail: Theme.tr("Everyone who uses this server can play it.")
                iconName: "library_music"
                onClicked: target.choose("library")
            }
            Choice {
                heading: Theme.tr("My encrypted files")
                detail: target.vaultReady ? Theme.tr("Only you (and whoever you share it with) can play it.")
                                          : Theme.tr("Unlock your account first (Settings → Account).")
                iconName: "lock"
                enabled: target.vaultReady
                onClicked: target.choose("vault")
            }
        }
    }
}
