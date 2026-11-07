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
    // Which extension's results (empty: all of them).
    property string only: ""
    signal openCollection(var entry)
    signal trackMenu(var track, Item anchor)
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

    ListView {
        anchors.fill: parent
        anchors.topMargin: 4
        clip: true
        model: es.only === "" ? es.results : es.results.filter(r => r.ext === es.only)
        ScrollBar.vertical: ScrollBar {}
        delegate: ExtensionRow {
            onOpenCollection: (entry) => es.openCollection(entry)
            onMenu: (t, a) => es.trackMenu(t, a)
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
}
