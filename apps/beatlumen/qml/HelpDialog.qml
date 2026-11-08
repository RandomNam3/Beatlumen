import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Keyboard shortcuts at a glance.
AppDialog {
    id: dlg
    signal startTour()
    signal startSetup()
    title: Theme.tr("Help")
    modal: true
    anchors.centerIn: Overlay.overlay
    width: Math.min(520, (parent ? parent.width : 520) - 32)
    height: Math.min(implicitHeight, (parent ? parent.height : 600) - 48)
    standardButtons: Dialog.Close
    onAboutToShow: Theme.nameButtons(this)

    readonly property var shortcuts: [
        ["Space", Theme.tr("Play or pause")],
        ["Ctrl + →  /  Ctrl + ←", Theme.tr("Next or previous song")],
        ["→  /  ←", Theme.tr("Forward or back 5 seconds")],
        ["Ctrl + ↑  /  Ctrl + ↓", Theme.tr("Louder or quieter")],
        ["Ctrl + L", Theme.tr("Mark the song as a favorite")],
        ["Ctrl + F", Theme.tr("Search")],
        ["Ctrl + Alt + →  /  ←", Theme.tr("Next or previous background")],
        ["F11", Theme.tr("Fullscreen")],
        ["Esc", Theme.tr("Back, or show the visual")],
        ["Tab", Theme.tr("Next control (Shift + Tab: previous)")],
    ]

    ScrollView {
        id: scroll
        anchors.fill: parent
        contentWidth: availableWidth
        clip: true
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        ColumnLayout {
            width: scroll.availableWidth
            spacing: 6
            Flow {
                Layout.fillWidth: true
                Layout.bottomMargin: 8
                spacing: 8
                AccentButton {
                    text: Theme.tr("Show me around")
                    highlighted: true
                    onClicked: { dlg.close(); dlg.startTour() }
                }
                Button {
                    flat: true
                    text: Theme.tr("Repeat the setup")
                    onClicked: { dlg.close(); dlg.startSetup() }
                }
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                Layout.bottomMargin: 4
                text: Theme.tr("Keyboard shortcuts")
                color: Theme.text
                font.weight: Font.DemiBold
            }
            Repeater {
                model: dlg.shortcuts
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: 16
                    Label {
                        textFormat: Text.PlainText
                        Layout.preferredWidth: Math.min(180, scroll.availableWidth * 0.42)
                        text: parent.modelData[0]
                        color: Theme.text
                        font.pixelSize: 13
                        font.weight: Font.DemiBold
                        wrapMode: Text.WordWrap
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: parent.modelData[1]
                        color: Theme.textDim
                        font.pixelSize: 13
                        wrapMode: Text.WordWrap
                    }
                }
            }
        }
    }
}
