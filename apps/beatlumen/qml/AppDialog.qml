import QtQuick
import QtQuick.Controls.Material

// Modal dialog with a dark scrim: Material's own dim colour is a grey that
// washes bright backgrounds out instead of darkening them.
Dialog {
    modal: true
    // A popup is only centred anew when its parent changes size: on the overlay,
    // it moves up when the keyboard makes the overlay smaller.
    parent: Overlay.overlay
    Overlay.modal: Rectangle {
        color: Qt.rgba(0, 0, 0, Theme.dark ? 0.55 : 0.35)
        Behavior on opacity { NumberAnimation { duration: Theme.ms(150) } }
    }
}
