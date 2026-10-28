import QtQuick
import QtQuick.Controls.Material

// Button whose text and icon stay readable on the accent colour when it is
// highlighted (dark text on light accents, white on dark ones; the Material
// style would decide by its own, coarser rule).
Button {
    id: b
    readonly property bool onAccent: highlighted && !flat && enabled
    // Contrast against the colour actually drawn (the style may shade it).
    readonly property color onAccentColor: background && background.color !== undefined ? Theme.onColor(background.color) : Theme.onAccent
    icon.color: onAccent ? onAccentColor : "transparent"
    Component.onCompleted: {
        if (contentItem && contentItem.color !== undefined) {
            contentItem.color = Qt.binding(() => b.onAccent ? b.onAccentColor
                : !b.enabled ? b.Material.hintTextColor
                : b.checked || b.flat && b.highlighted ? b.Material.accentColor : Theme.text)
        }
    }
}
