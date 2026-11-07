import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

// A panel that slides up from the bottom edge, over what is there. It closes
// by tapping outside, pulling the handle down or going back.
Popup {
    id: sheet
    default property alias content: body.data
    property string title: ""
    // How much room the content wants, and the share of the window it gets.
    property real wantedHeight: room
    property real maxShare: 0.85
    // 0: below the window, 1: all the way up.
    property real reveal: 0
    property real pull: 0
    readonly property real room: parent ? parent.height : 0

    parent: Overlay.overlay
    modal: true
    focus: true
    padding: 0
    closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
    width: parent ? Math.min(parent.width, 640) : 0
    height: Math.min(room * maxShare, wantedHeight + handle.height)
    x: parent ? (parent.width - width) / 2 : 0
    y: room - height * reveal + pull

    onClosed: pull = 0

    Overlay.modal: Rectangle {
        color: Qt.rgba(0, 0, 0, Theme.dark ? 0.5 : 0.3)
        Behavior on opacity { NumberAnimation { duration: Theme.ms(150) } }
    }
    enter: Transition {
        NumberAnimation { property: "reveal"; from: 0; to: 1; duration: Theme.ms(220); easing.type: Easing.OutCubic }
    }
    exit: Transition {
        NumberAnimation { property: "reveal"; from: 1; to: 0; duration: Theme.ms(160); easing.type: Easing.InCubic }
    }
    background: Rectangle {
        color: Theme.surfaceStrong
        radius: 16
        // Only the upper corners are round.
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: 16
            color: parent.color
        }
    }
    contentItem: ColumnLayout {
        spacing: 0
        Accessible.role: Accessible.Dialog
        Accessible.name: sheet.title
        Item {
            id: handle
            Layout.fillWidth: true
            Layout.preferredHeight: 24
            Rectangle {
                anchors.centerIn: parent
                width: 36
                height: 4
                radius: 2
                color: Theme.outline
            }
            // Takes the pointer at once: what scrolls below never sees this.
            MouseArea {
                id: pulling
                anchors.fill: parent
                preventStealing: true
                property real startY: 0
                function sceneY(mouse) { return mapToItem(null, mouse.x, mouse.y).y }
                onPressed: (mouse) => startY = sceneY(mouse)
                onPositionChanged: (mouse) => sheet.pull = Math.max(0, sceneY(mouse) - startY)
                onReleased: {
                    if (sheet.pull > Math.min(96, sheet.height * 0.25))
                        sheet.close()
                    else
                        sheet.pull = 0
                }
            }
        }
        Item {
            id: body
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
        }
    }
    Behavior on pull {
        enabled: !pulling.pressed
        NumberAnimation { duration: Theme.ms(150) }
    }
}
