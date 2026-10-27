import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Centered placeholder with icon, text and optional actions.
ColumnLayout {
    id: e
    property string iconName: "music_note"
    property string title
    property string text
    default property alias actions: buttons.data
    spacing: 12

    Icon {
        Layout.alignment: Qt.AlignHCenter
        name: e.iconName
        size: 56
        color: Theme.textFaint
    }
    Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        text: e.title
        font.pixelSize: 18
        font.weight: Font.DemiBold
        wrapMode: Text.WordWrap
        color: Theme.text
    }
    Label {
        Layout.fillWidth: true
        visible: e.text !== ""
        horizontalAlignment: Text.AlignHCenter
        text: e.text
        wrapMode: Text.WordWrap
        color: Theme.textDim
    }
    Flow {
        id: buttons
        Layout.alignment: Qt.AlignHCenter
        Layout.maximumWidth: e.width
        spacing: 8
    }
}
