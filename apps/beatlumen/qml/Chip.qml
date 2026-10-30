import QtQuick
import QtQuick.Controls.Material

// Filter chip (section bars, setting categories).
AbstractButton {
    id: chip
    implicitHeight: 36
    implicitWidth: implicitContentWidth + leftPadding + rightPadding
    leftPadding: 12
    rightPadding: 12
    font.pixelSize: 14
    font.weight: Font.Medium
    Accessible.name: text
    contentItem: Label {
        textFormat: Text.PlainText
        text: chip.text
        font: chip.font
        verticalAlignment: Text.AlignVCenter
        color: chip.checked ? Theme.onAccent : Theme.text
    }
    background: Rectangle {
        radius: height / 2
        color: chip.checked ? Theme.accent : chip.down ? Theme.outline : Theme.surfaceVariant
        border.width: chip.checked ? 0 : 1
        border.color: Theme.outline
        Behavior on color { ColorAnimation { duration: 120 } }
    }
}
