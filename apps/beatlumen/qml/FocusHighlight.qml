import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// One outline for all controls, around whatever has the keyboard focus (not
// after a click). Follows it while lists scroll and panels move.
Rectangle {
    id: ring
    property Item target: null
    readonly property bool show: !!target && target.visualFocus === true

    parent: Overlay.overlay
    z: 10000
    visible: show
    color: "transparent"
    radius: Theme.smallRadius
    border.width: 2
    border.color: Theme.dark ? "white" : "#1b1a20"
    // A second line for contrast on any background.
    Rectangle {
        anchors.fill: parent
        anchors.margins: 2
        radius: Math.max(0, parent.radius - 2)
        color: "transparent"
        border.width: 1
        border.color: Theme.dark ? "#1b1a20" : "white"
    }

    function follow() {
        if (!show || !parent)
            return
        const p = target.mapToItem(parent, 0, 0)
        x = p.x - 3
        y = p.y - 3
        width = target.width + 6
        height = target.height + 6
    }
    onTargetChanged: follow()
    onShowChanged: follow()
    Timer {
        interval: 60
        repeat: true
        running: ring.show
        onTriggered: ring.follow()
    }
}
