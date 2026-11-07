import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A result of a server extension: a song (plays, and waits in the queue until
// its turn) or a collection (opens).
TrackDelegate {
    id: row
    required property var modelData
    signal openCollection(var entry)

    readonly property bool isCollection: modelData.kind === "collection"
    function details() {
        const parts = (modelData.detail || "").split(" · ").filter(x => x).map(x => Theme.tr(x))
        if (modelData.inLibrary)
            parts.push(Theme.tr("In library"))
        return [modelData.artist].concat(parts).filter(x => x).join(" · ")
    }
    track: ({ id: isCollection ? "" : modelData.songId, title: modelData.title,
              artist: details(), duration: isCollection ? 0 : modelData.duration, cover: modelData.image })
    showMenu: !isCollection
    onPlay: {
        if (isCollection)
            openCollection(modelData)
        else
            Core.call("extensions.play", { ext: modelData.ext, id: modelData.id })
    }
}
