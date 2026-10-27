import QtQuick
import QtQuick.Effects

// Rounded cover art with a placeholder icon.
Item {
    id: root
    property string source
    property string placeholder: "music_note"
    property real radius: Theme.smallRadius

    Rectangle {
        anchors.fill: parent
        radius: root.radius
        color: Theme.surfaceVariant
        Icon {
            anchors.centerIn: parent
            name: root.placeholder
            size: Math.max(16, Math.min(root.width, root.height) * 0.42)
            color: Theme.textFaint
            visible: img.status !== Image.Ready
        }
    }
    Image {
        id: img
        anchors.fill: parent
        source: root.source
        sourceSize: Qt.size(Math.max(64, root.width * 2), Math.max(64, root.height * 2))
        fillMode: Image.PreserveAspectCrop
        asynchronous: true
        cache: true
        visible: false
    }
    Rectangle {
        id: mask
        anchors.fill: parent
        radius: root.radius
        visible: false
        layer.enabled: true
    }
    MultiEffect {
        anchors.fill: parent
        source: img
        maskEnabled: true
        maskSource: mask
        visible: img.status === Image.Ready
    }
}
