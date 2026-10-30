import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Adds, renames or removes an internet radio station.
AppDialog {
    id: dlg
    property var station: null

    title: Theme.tr(station ? "Edit station" : "Add station")
    anchors.centerIn: Overlay.overlay
    width: Math.min(440, (parent ? parent.width : 440) - 32)
    standardButtons: Dialog.NoButton

    function openFor(s) {
        station = s || null
        nameField.text = s ? s.name : ""
        urlField.text = s ? s.url : ""
        error.text = ""
        open()
    }

    onOpened: (station ? nameField : urlField).forceActiveFocus()

    ColumnLayout {
        anchors.fill: parent
        spacing: 8
        TextField {
            id: urlField
            Layout.fillWidth: true
            enabled: !dlg.station
            placeholderText: Theme.tr("Stream address or station file")
            inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
        }
        TextField {
            id: nameField
            Layout.fillWidth: true
            placeholderText: Theme.tr("Name")
        }
        Label {
            id: error
            visible: text.length > 0
            textFormat: Text.PlainText
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            color: Theme.danger
        }
        RowLayout {
            Layout.fillWidth: true
            Button {
                visible: !!dlg.station
                text: Theme.tr("Remove")
                flat: true
                onClicked: { Core.call("radio.remove", { url: dlg.station.url }); dlg.close() }
            }
            Item { Layout.fillWidth: true }
            Button { text: Theme.tr("Cancel"); flat: true; onClicked: dlg.close() }
            AccentButton {
                text: Theme.tr("Save")
                highlighted: true
                enabled: urlField.text.trim().length > 0
                onClicked: {
                    const r = Core.call("radio.add", { url: urlField.text.trim(), name: nameField.text.trim() })
                    if (r && r.error)
                        error.text = r.error
                    else
                        dlg.close()
                }
            }
        }
    }
}
