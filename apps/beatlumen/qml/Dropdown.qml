import QtQuick
import QtQuick.Controls.Material

// A dropdown as large as the controls around it, as wide as its longest entry.
ComboBox {
    id: control
    implicitWidth: Math.max(72, implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Theme.fingers ? 48 : 32
    implicitContentWidthPolicy: ComboBox.WidestText
    padding: 4
    spacing: 0
    font.pixelSize: Theme.fingers ? 14 : 13
    // Material's own padding is made for its taller field.
    Component.onCompleted: {
        contentItem.leftPadding = 10
        contentItem.topPadding = 0
        contentItem.bottomPadding = 0
    }
}
