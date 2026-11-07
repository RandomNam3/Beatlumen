import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A result of a server extension: a song (plays, waits in the queue until its
// turn, or goes straight into the library) or a collection (opens).
TrackDelegate {
    id: row
    required property var modelData
    signal openCollection(var entry)

    readonly property bool isCollection: modelData.kind === "collection"
    property bool adding: false
    onModelDataChanged: adding = false
    function details() {
        const parts = (modelData.detail || "").split(" · ").filter(x => x).map(x => Theme.tr(x))
        if (modelData.inLibrary)
            parts.push(Theme.tr("In library"))
        return [modelData.artist].concat(parts).filter(x => x).join(" · ")
    }
    track: ({ id: isCollection ? "" : modelData.songId, title: modelData.title,
              artist: details(), duration: isCollection ? 0 : modelData.duration, cover: modelData.image })
    showMenu: !isCollection
    actionIcon: isCollection || modelData.inLibrary === true || adding ? "" : "add"
    actionTip: Theme.tr("Add to library")
    onRowAction: {
        adding = true
        Core.call("extensions.add", { ids: [modelData.songId] })
    }
    onPlay: {
        if (isCollection)
            openCollection(modelData)
        else
            Core.call("extensions.play", { ext: modelData.ext, id: modelData.id })
    }
}
