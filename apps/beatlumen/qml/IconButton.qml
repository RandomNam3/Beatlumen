import QtQuick
import QtQuick.Controls
import Beatlumen.Core

// Round, touch sized icon button with tooltip.
ToolButton {
    id: control
    property string iconName
    property string tip
    property color iconColor: Theme.text
    property int iconSize: 24
    property bool active: false

    implicitWidth: Theme.touch
    implicitHeight: Theme.touch
    padding: 0
    icon.source: iconName ? Theme.icon(iconName) : ""
    icon.color: active ? Theme.accent : iconColor
    icon.width: iconSize
    icon.height: iconSize
    display: AbstractButton.IconOnly
    ToolTip.text: tip
    ToolTip.visible: tip !== "" && hovered && !Core.mobile
    ToolTip.delay: 600
    Accessible.name: tip
}
