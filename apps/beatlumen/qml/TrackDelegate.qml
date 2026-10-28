import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// One song in a list: cover, title, artist, duration and a menu.
ItemDelegate {
    id: d
    property var track: ({})
    property int number: -1
    property bool current: false
    property bool showCover: true
    signal play()
    signal menu(var track, Item anchor)

    width: ListView.view ? ListView.view.width : implicitWidth
    height: Theme.row
    padding: 0
    highlighted: current
    onClicked: play()
    onPressAndHold: menu(track, menuButton)

    Component.onCompleted: if (track.local && !track.cover) Covers.request(track.id)

    contentItem: RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 4
        spacing: 12
        Label {
            textFormat: Text.PlainText
            visible: d.number >= 0 && !d.showCover
            Layout.preferredWidth: 28
            horizontalAlignment: Text.AlignRight
            text: d.number >= 0 ? d.number : ""
            color: d.current ? Theme.accent : Theme.textFaint
            font.features: { "tnum": 1 }
        }
        CoverImage {
            visible: d.showCover
            Layout.preferredWidth: 44
            Layout.preferredHeight: 44
            source: Covers.url(d.track.id, d.track.cover)
            radius: 8
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: d.track.title || ""
                color: d.current ? Theme.accent : Theme.text
                font.pixelSize: 15
                font.weight: d.current ? Font.DemiBold : Font.Medium
                elide: Text.ElideRight
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: [d.track.artist || Theme.tr("Unknown artist"), d.track.album].filter(x => x).join(" · ")
                color: Theme.textDim
                font.pixelSize: 13
                elide: Text.ElideRight
            }
        }
        Label {
            textFormat: Text.PlainText
            text: d.track.duration > 0 ? Theme.fmtTime(d.track.duration) : ""
            color: Theme.textFaint
            font.pixelSize: 13
            font.features: { "tnum": 1 }
        }
        IconButton {
            id: menuButton
            iconName: "more_vert"
            iconColor: Theme.textDim
            tip: Theme.tr("More")
            onClicked: d.menu(d.track, menuButton)
        }
    }
}
