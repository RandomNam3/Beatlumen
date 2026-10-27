import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// A style (preset) with a colourful preview generated from its name.
AbstractButton {
    id: card
    property string name
    property string origin: "builtin"
    property bool active: false
    signal deleteRequested()

    implicitHeight: 112
    Accessible.name: name
    onPressAndHold: if (origin !== "builtin") menu.popup()

    function hash(s) {
        let h = 2166136261
        for (let i = 0; i < s.length; i++)
            h = Math.imul(h ^ s.charCodeAt(i), 16777619)
        return (h >>> 0)
    }
    readonly property real hue: (hash(name) % 360) / 360

    background: Rectangle {
        radius: Theme.radius
        border.width: card.active ? 3 : 0
        border.color: Theme.accent
        gradient: Gradient {
            orientation: Gradient.Horizontal
            GradientStop { position: 0; color: Qt.hsla(card.hue, 0.75, 0.42, 1) }
            GradientStop { position: 1; color: Qt.hsla((card.hue + 0.18) % 1, 0.8, 0.30, 1) }
        }
        Rectangle {
            anchors.fill: parent
            radius: parent.radius
            color: "black"
            opacity: card.pressed ? 0.25 : card.hovered ? 0.05 : 0.12
        }
    }
    contentItem: Item {
        Label {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.margins: 12
            text: card.name
            color: "white"
            font.pixelSize: 16
            font.weight: Font.Bold
            wrapMode: Text.WordWrap
            maximumLineCount: 2
            elide: Text.ElideRight
        }
        Label {
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.margins: 12
            text: card.origin === "builtin" ? Theme.tr("Built-in") : card.origin === "server" ? Theme.tr("Server") : Theme.tr("Mine")
            color: Qt.rgba(1, 1, 1, 0.8)
            font.pixelSize: 11
            font.weight: Font.DemiBold
            font.capitalization: Font.AllUppercase
        }
        Icon {
            visible: card.active
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: 10
            name: "check"
            color: "white"
        }
    }
    Menu {
        id: menu
        MenuItem { text: Theme.tr("Delete"); icon.source: Theme.icon("delete"); onTriggered: card.deleteRequested() }
    }
}
