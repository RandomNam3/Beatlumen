import QtQuick
import QtQuick.Effects

// Rounded cover art with a placeholder icon. With a `seed` (e.g. the album
// name) the placeholder gets its own colour, so covers without art can
// still be told apart.
Item {
    id: root
    property string source
    property string placeholder: "music_note"
    property real radius: Theme.smallRadius
    property string seed
    readonly property bool loaded: img.status === Image.Ready

    Rectangle {
        anchors.fill: parent
        radius: root.radius
        color: root.seed ? Theme.seedColor(root.seed, 0.30) : Theme.surfaceVariant
        gradient: root.seed ? tint : null
        Gradient {
            id: tint
            GradientStop { position: 0; color: Theme.seedColor(root.seed, 0.36) }
            GradientStop { position: 1; color: Theme.seedColor(root.seed, 0.20) }
        }
        Icon {
            anchors.centerIn: parent
            name: root.placeholder
            size: Math.max(16, Math.min(root.width, root.height) * 0.42)
            color: root.seed ? Theme.seedColor(root.seed, 0.75) : Theme.textFaint
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
