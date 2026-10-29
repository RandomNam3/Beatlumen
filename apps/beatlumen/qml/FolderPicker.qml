import QtQuick
import Beatlumen.Core

// In-app folder browser (Android needs real paths for library scanning).
FilePicker {
    signal chosen(string path)
    mode: "folder"
    onPicked: (paths) => chosen(paths[0])
}
