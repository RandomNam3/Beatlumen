import QtQuick
import Beatlumen.Core

// File and folder pickers on Android and in the browser: the system's
// document picker (files are copied or handed over as bytes) and the
// in-app folder browser. Avoids Qt Quick Dialogs, which would pull every
// Qt Quick Controls style into the package.
Item {
    function openAudio() { Core.pickFiles("audio/*", true) }
    function openImages() { Core.pickFiles("image/*", true) }
    // Files for the encrypted storage: the core encrypts what is picked next.
    function openUpload() { Core.call("vault.expectUpload"); Core.pickFiles("*/*", true) }
    // The next picked file is the current song's text.
    function openLyrics() { Core.call("lyrics.expectImport"); Core.pickFiles("*/*", false) }
    function addMusicFolder() { folderPicker.showFor(false) }
    function addImageFolder() { folderPicker.showFor(true) }
    function exportPlaylist(id) { folderPicker.exportId = id; folderPicker.images = false; folderPicker.show() }

    FolderPicker {
        id: folderPicker
        property bool forImages: false
        // A playlist to export into the chosen folder.
        property string exportId: ""
        function showFor(img) { exportId = ""; forImages = img; images = img; show() }
        onChosen: (p) => {
            if (exportId) { Core.call("playlist.export", { id: exportId, folder: p }); exportId = "" }
            else if (forImages) Core.call("backgrounds.addFolder", { path: p })
            else Core.call("library.addFolder", { path: p })
        }
    }
}
