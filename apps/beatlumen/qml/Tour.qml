import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A short tour: dims everything but one control and says what it does.
Popup {
    id: tour
    // [{ item: <Item>, title, text }]
    property var steps: []
    property int step: 0
    readonly property var current: steps.length > 0 ? steps[Math.min(step, steps.length - 1)] : null

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    modal: true
    // This dims by itself, around the control.
    Overlay.modal: Item { }
    padding: 0
    closePolicy: Popup.CloseOnEscape
    background: null

    function start(list) {
        steps = list
        step = 0
        open()
        nextButton.forceActiveFocus()
    }
    function next() {
        if (step + 1 < steps.length)
            step++
        else
            close()
    }

    // Where the control is now.
    function hole() {
        if (!current || !current.item || !parent)
            return Qt.rect(0, 0, 0, 0)
        const p = current.item.mapToItem(parent, 0, 0)
        return Qt.rect(p.x - 4, p.y - 4, current.item.width + 8, current.item.height + 8)
    }
    property rect target: Qt.rect(0, 0, 0, 0)
    onCurrentChanged: target = hole()
    onOpened: target = hole()
    onWidthChanged: target = hole()
    onHeightChanged: target = hole()

    contentItem: Item {
        Accessible.role: Accessible.Dialog
        Accessible.name: Theme.tr("Tour")
        // The dim area around the control.
        readonly property color dim: Qt.rgba(0, 0, 0, 0.72)
        Rectangle { x: 0; y: 0; width: parent.width; height: tour.target.y; color: parent.dim }
        Rectangle { x: 0; y: tour.target.y + tour.target.height; width: parent.width; height: parent.height - y; color: parent.dim }
        Rectangle { x: 0; y: tour.target.y; width: tour.target.x; height: tour.target.height; color: parent.dim }
        Rectangle { x: tour.target.x + tour.target.width; y: tour.target.y; width: parent.width - x; height: tour.target.height; color: parent.dim }
        Rectangle {
            x: tour.target.x
            y: tour.target.y
            width: tour.target.width
            height: tour.target.height
            radius: Theme.smallRadius
            color: "transparent"
            border.width: 2
            border.color: "white"
        }

        // The text, next to the control where there is room.
        Rectangle {
            id: bubble
            readonly property real room: parent.width - tour.target.x - tour.target.width
            readonly property bool beside: room >= 340 && tour.target.height < parent.height * 0.6
            width: Math.min(360, parent.width - 32)
            height: bubbleColumn.implicitHeight + 32
            radius: Theme.radius
            color: Theme.surfaceStrong
            border.width: 1
            border.color: Theme.outline
            x: beside ? tour.target.x + tour.target.width + 16 : (parent.width - width) / 2
            y: {
                if (beside)
                    return Math.max(16, Math.min(parent.height - height - 16, tour.target.y))
                const above = tour.target.y - height - 16
                return above >= 16 ? above : Math.min(parent.height - height - 16, tour.target.y + tour.target.height + 16)
            }
            ColumnLayout {
                id: bubbleColumn
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.margins: 16
                spacing: 8
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: tour.current ? tour.current.title : ""
                    color: Theme.text
                    font.pixelSize: 18
                    font.weight: Font.DemiBold
                    wrapMode: Text.WordWrap
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: tour.current ? tour.current.text : ""
                    color: Theme.textDim
                    wrapMode: Text.WordWrap
                }
                RowLayout {
                    Layout.fillWidth: true
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: (tour.step + 1) + " / " + tour.steps.length
                        color: Theme.textDim
                        font.pixelSize: 13
                    }
                    Button {
                        flat: true
                        text: Theme.tr("Skip")
                        onClicked: tour.close()
                    }
                    Button {
                        flat: true
                        visible: tour.step > 0
                        text: Theme.tr("Back")
                        onClicked: tour.step--
                    }
                    AccentButton {
                        id: nextButton
                        text: tour.step + 1 < tour.steps.length ? Theme.tr("Next") : Theme.tr("Done")
                        highlighted: true
                        onClicked: tour.next()
                    }
                }
            }
        }
    }
}
