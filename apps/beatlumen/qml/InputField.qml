import QtQuick
import QtQuick.Controls.Material

// A text field as large as the controls around it.
TextField {
    implicitHeight: Theme.fingers ? Material.textFieldHeight : 36
    font.pixelSize: Theme.fingers ? 14 : 13
}
