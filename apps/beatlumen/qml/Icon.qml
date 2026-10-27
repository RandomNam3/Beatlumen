import QtQuick
import QtQuick.Controls.impl

// Tinted Material Symbols icon.
Item {
    id: root
    property string name
    property int size: 24
    property color color: Theme.text
    implicitWidth: size
    implicitHeight: size
    width: size
    height: size

    IconImage {
        anchors.fill: parent
        sourceSize: Qt.size(root.size, root.size)
        source: root.name ? Theme.icon(root.name) : ""
        color: root.color
    }
}
