import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A titled row of cards that scrolls sideways: by dragging, the wheel, the
// arrow buttons or, with keyboard focus, the arrow keys.
ColumnLayout {
    id: shelf
    property string title
    property var items: []
    property string moreText: ""
    property alias delegate: list.delegate
    signal moreClicked()

    visible: items.length > 0
    spacing: 8

    function scroll(direction) {
        const target = list.contentX + direction * Math.max(list.width * 0.8, 160)
        animation.to = Math.max(0, Math.min(list.contentWidth - list.width, target))
        animation.restart()
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.leftMargin: 12
        Layout.rightMargin: 4
        Layout.minimumHeight: Theme.touch
        spacing: 0
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: shelf.title
            color: Theme.text
            font.pixelSize: 20
            font.weight: Font.DemiBold
            elide: Text.ElideRight
            Accessible.role: Accessible.Heading
            Accessible.name: text
        }
        Button {
            visible: shelf.moreText !== ""
            flat: true
            text: shelf.moreText
            onClicked: shelf.moreClicked()
        }
        IconButton {
            visible: !Core.mobile && list.contentWidth > list.width + 1
            enabled: list.contentX > 1
            iconName: "chevron_left"
            tip: Theme.tr("Scroll back")
            onClicked: shelf.scroll(-1)
        }
        IconButton {
            visible: !Core.mobile && list.contentWidth > list.width + 1
            enabled: list.contentX + list.width < list.contentWidth - 1
            iconName: "chevron_right"
            tip: Theme.tr("Scroll on")
            onClicked: shelf.scroll(1)
        }
    }

    ListView {
        id: list
        // Only the first time: later updates of the row stay calm.
        property bool appeared: false
        onCountChanged: if (count > 0) Qt.callLater(() => list.appeared = true)
        populate: Transition {
            enabled: Theme.animate && !list.appeared
            NumberAnimation { property: "opacity"; from: 0; to: 1; duration: Theme.motion(Theme.slow); easing.type: Theme.easeOut }
        }
        Layout.fillWidth: true
        Layout.preferredHeight: contentItem.childrenRect.height > 0 ? contentItem.childrenRect.height + 4 : 206
        leftMargin: 12
        rightMargin: 12
        topMargin: 2
        orientation: ListView.Horizontal
        spacing: 14
        clip: true
        model: shelf.items
        boundsBehavior: Flickable.StopAtBounds
        activeFocusOnTab: true
        Accessible.role: Accessible.List
        Accessible.name: shelf.title
        // The window's arrow keys and Space seek and pause otherwise.
        Keys.onShortcutOverride: (event) => {
            if (event.key === Qt.Key_Left || event.key === Qt.Key_Right || event.key === Qt.Key_Space)
                event.accepted = true
        }
        NumberAnimation {
            id: animation
            target: list
            property: "contentX"
            duration: Theme.ms(220)
            easing.type: Easing.OutCubic
        }
    }
}
