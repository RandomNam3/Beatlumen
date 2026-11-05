import QtQuick

// Keyboard focus outline; put inside the control and set `shown`.
Rectangle {
    property bool shown: false
    property real inset: -2
    anchors.fill: parent
    anchors.margins: inset
    radius: Theme.smallRadius
    color: "transparent"
    border.width: 2
    border.color: Theme.dark ? "white" : "#1b1a20"
    visible: shown
    z: 100
}
