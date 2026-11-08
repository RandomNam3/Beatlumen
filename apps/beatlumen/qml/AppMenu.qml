import QtQuick
import QtQuick.Controls.Material

// A menu that grows out of where it opens and fades, with the app's motion
// (only fading without sliding, at once without motion).
Menu {
    id: menu
    readonly property real startScale: Theme.slide ? 0.92 : 1
    onAboutToShow: Theme.smoothFrames(Theme.fast)
    enter: Transition {
        NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Theme.motion(Theme.fast); easing.type: Theme.easeOut }
        NumberAnimation { property: "scale"; from: menu.startScale; to: 1; duration: Theme.motion(Theme.fast); easing.type: Theme.easeOut }
    }
    exit: Transition {
        NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Theme.motion(Theme.fast); easing.type: Theme.easeIn }
    }
}
