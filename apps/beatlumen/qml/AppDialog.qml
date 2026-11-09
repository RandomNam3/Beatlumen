import QtQuick
import QtQuick.Controls.Material

// Modal dialog with a dark scrim: Material's own dim colour is a grey that
// washes bright backgrounds out instead of darkening them.
Dialog {
    id: dialog
    modal: true
    onAboutToShow: Theme.smoothFrames(Theme.normal)
    enter: Transition {
        NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Theme.motion(Theme.normal); easing.type: Theme.easeOut }
        NumberAnimation { property: "scale"; from: Theme.slide ? 0.94 : 1; to: 1; duration: Theme.motion(Theme.normal); easing.type: Theme.easeOut }
    }
    exit: Transition {
        NumberAnimation { property: "opacity"; from: 1; to: 0; duration: Theme.motion(Theme.fast); easing.type: Theme.easeIn }
        NumberAnimation { property: "scale"; from: 1; to: Theme.slide ? 0.97 : 1; duration: Theme.motion(Theme.fast); easing.type: Theme.easeIn }
    }
    // A popup is only centred anew when its parent changes size: on the overlay,
    // it moves up when the keyboard makes the overlay smaller.
    parent: Overlay.overlay
    Overlay.modal: Rectangle {
        color: Qt.rgba(0, 0, 0, Theme.dark ? 0.55 : 0.35)
        Behavior on opacity { NumberAnimation { duration: Theme.motion(Theme.fast) } }
    }
}
