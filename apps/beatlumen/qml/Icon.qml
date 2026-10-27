import QtQuick
import QtQuick.Controls.impl

// Tinted Material Symbols icon.
IconImage {
    property string name
    property int size: 24
    width: size
    height: size
    sourceSize: Qt.size(size, size)
    source: name ? Theme.icon(name) : ""
    color: Theme.text
}
