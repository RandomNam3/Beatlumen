import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts

// A setting with title, explanation and a switch; the whole row is tappable.
ItemDelegate {
    id: r
    property string title
    property string detail: ""
    property bool on: false
    signal switched(bool on)
    implicitHeight: Math.max(Theme.row, col.implicitHeight + 20)
    Accessible.role: Accessible.CheckBox
    Accessible.name: title
    Accessible.description: detail
    Accessible.checkable: true
    Accessible.checked: on
    onClicked: r.switched(!r.on)
    Keys.onReturnPressed: r.switched(!r.on)
    Keys.onEnterPressed: r.switched(!r.on)
    contentItem: RowLayout {
        spacing: 12
        ColumnLayout {
            id: col
            Layout.fillWidth: true
            spacing: 2
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: r.title; font.pixelSize: 15; color: Theme.text; wrapMode: Text.WordWrap }
            Label { textFormat: Text.PlainText; Layout.fillWidth: true; visible: r.detail !== ""; text: r.detail; font.pixelSize: 13; color: Theme.textDim; wrapMode: Text.WordWrap }
        }
        Switch {
            checked: r.on
            // The row is the control; this only shows its state.
            focusPolicy: Qt.NoFocus
            onToggled: r.switched(checked)
            Accessible.name: r.title
        }
    }
}
