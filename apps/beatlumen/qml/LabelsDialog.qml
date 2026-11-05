import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The labels of a song: those worked out to look at, your own to change.
AppDialog {
    id: dlg
    property string trackId
    property var auto: []
    property var given: []
    title: Theme.tr("Labels")
    modal: true
    anchors.centerIn: Overlay.overlay
    width: Math.min(460, (parent ? parent.width : 460) - 32)
    standardButtons: Dialog.Close

    function reload() {
        const l = Core.call("labels.of", { id: trackId }) || {}
        auto = l.auto || []
        given = l.given || []
    }
    function show(id) {
        trackId = id
        reload()
        field.clear()
        open()
    }
    function add() {
        const name = field.text.trim()
        if (name === "")
            return
        Core.call("labels.change", { id: trackId, add: [name] })
        field.clear()
        reload()
    }

    ColumnLayout {
        anchors.left: parent.left
        anchors.right: parent.right
        spacing: 10

        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: Theme.tr("Your labels")
            color: Theme.text
            font.weight: Font.DemiBold
        }
        Flow {
            Layout.fillWidth: true
            spacing: 6
            visible: dlg.given.length > 0
            Repeater {
                model: dlg.given
                Chip {
                    required property string modelData
                    text: modelData + "  ×"
                    Accessible.name: Theme.tr("Remove") + ": " + modelData
                    onClicked: {
                        Core.call("labels.change", { id: dlg.trackId, remove: [modelData] })
                        dlg.reload()
                    }
                }
            }
        }
        RowLayout {
            Layout.fillWidth: true
            TextField {
                id: field
                Layout.fillWidth: true
                placeholderText: Theme.tr("New label")
                maximumLength: 40
                Accessible.name: Theme.tr("New label")
                onAccepted: dlg.add()
            }
            AccentButton {
                text: Theme.tr("Add")
                highlighted: true
                enabled: field.text.trim() !== ""
                onClicked: dlg.add()
            }
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.topMargin: 6
            visible: dlg.auto.length > 0
            wrapMode: Text.WordWrap
            text: Theme.tr("Worked out from tags and sound (mood is an estimate)")
            color: Theme.textDim
            font.pixelSize: 13
        }
        Flow {
            Layout.fillWidth: true
            spacing: 6
            visible: dlg.auto.length > 0
            Repeater {
                model: dlg.auto
                Rectangle {
                    required property string modelData
                    width: autoText.implicitWidth + 20
                    height: 30
                    radius: 15
                    color: Theme.surfaceVariant
                    Label {
                        id: autoText
                        textFormat: Text.PlainText
                        anchors.centerIn: parent
                        text: Theme.labelName(parent.modelData)
                        color: Theme.textDim
                        font.pixelSize: 13
                    }
                }
            }
        }
    }
}
