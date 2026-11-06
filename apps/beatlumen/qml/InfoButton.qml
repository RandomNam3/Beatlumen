import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// A small "i": the longer explanation, for whoever wants to read it.
IconButton {
    id: b
    property string info
    iconName: "info"
    iconSize: 16
    iconColor: Theme.textDim
    implicitWidth: Theme.fingers ? 40 : 24
    implicitHeight: Theme.fingers ? 40 : 24
    tip: Theme.tr("More about this")
    Accessible.description: info
    onClicked: {
        const p = b.mapToItem(Overlay.overlay, b.width / 2, b.height)
        popup.x = p.x - popup.width / 2
        popup.y = p.y + 4
        popup.open()
    }
    Popup {
        id: popup
        parent: Overlay.overlay
        margins: 8
        padding: 12
        focus: true
        width: Math.min(320, parent ? parent.width - 16 : 320)
        background: Rectangle {
            color: Theme.surfaceStrong
            border.color: Theme.outline
            radius: Theme.smallRadius
        }
        contentItem: Label {
            textFormat: Text.PlainText
            text: b.info
            color: Theme.text
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
    }
}
