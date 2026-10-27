import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Page title row with optional back button and trailing actions.
RowLayout {
    id: h
    property string title
    property string subtitle
    property bool back: false
    default property alias actions: actionRow.data
    signal backClicked()

    spacing: 4
    IconButton {
        visible: h.back
        iconName: "arrow_back"
        tip: Theme.tr("Back")
        onClicked: h.backClicked()
    }
    ColumnLayout {
        Layout.fillWidth: true
        Layout.leftMargin: h.back ? 0 : 8
        spacing: 0
        Label {
            Layout.fillWidth: true
            text: h.title
            font.pixelSize: 26
            font.weight: Font.Bold
            color: Theme.text
            elide: Text.ElideRight
        }
        Label {
            Layout.fillWidth: true
            visible: h.subtitle !== ""
            text: h.subtitle
            font.pixelSize: 13
            color: Theme.textDim
            elide: Text.ElideRight
        }
    }
    RowLayout {
        id: actionRow
        spacing: 0
    }
}
