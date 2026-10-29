import QtQuick
import QtQuick.Controls.Material

// Modal dialog with a dark scrim: Material's own dim colour is a grey that
// washes bright backgrounds out instead of darkening them.
Dialog {
    modal: true
    Overlay.modal: Rectangle {
        color: Qt.rgba(0, 0, 0, Theme.dark ? 0.55 : 0.35)
        Behavior on opacity { NumberAnimation { duration: 150 } }
    }
}
