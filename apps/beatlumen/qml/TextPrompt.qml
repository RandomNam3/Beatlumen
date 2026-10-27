import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

// Small dialog asking for one line of text.
Dialog {
    id: dlg
    property alias text: field.text
    property alias placeholder: field.placeholderText
    property bool password: false
    property string acceptText: Theme.tr("OK")
    signal submitted(string text)

    modal: true
    anchors.centerIn: Overlay.overlay
    width: Math.min(420, (parent ? parent.width : 420) - 32)
    standardButtons: Dialog.NoButton
    onOpened: { field.forceActiveFocus(); field.selectAll() }

    ColumnLayout {
        anchors.fill: parent
        spacing: 12
        TextField {
            id: field
            Layout.fillWidth: true
            echoMode: dlg.password ? TextInput.Password : TextInput.Normal
            onAccepted: if (text.length > 0) { dlg.submitted(text); dlg.close() }
        }
        RowLayout {
            Layout.alignment: Qt.AlignRight
            Button { text: Theme.tr("Cancel"); flat: true; onClicked: dlg.close() }
            Button {
                text: dlg.acceptText
                highlighted: true
                enabled: field.text.length > 0
                onClicked: { dlg.submitted(field.text); dlg.close() }
            }
        }
    }
}
