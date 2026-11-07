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
    property bool showMenu: true
    // Can be selected (see TrackListView); while any song is, a tap selects.
    property bool selectable: false
    property bool selecting: false
    property bool selected: false
    signal selectToggled()
    signal selectRange()
    // One more action on the row, if any.
    property string actionIcon: ""
    property string actionTip: ""
    signal rowAction()
    signal play()
    signal menu(var track, Item anchor)

    width: ListView.view ? ListView.view.width : implicitWidth
    height: Theme.row
    padding: 0
    highlighted: current || selected
    Accessible.role: Accessible.ListItem
    Accessible.name: [track.title, track.artist, track.album, current ? Theme.tr("Now playing") : "", selected ? Theme.tr("Selected") : ""].filter(x => x).join(", ")
    Keys.onReturnPressed: play()
    Keys.onEnterPressed: play()
    Keys.onMenuPressed: if (showMenu) menu(track, menuButton)
    onClicked: selecting ? selectToggled() : play()
    // A finger selects, as a long press does elsewhere; a mouse has the keys.
    onPressAndHold: {
        if (selectable && Theme.fingers)
            selectToggled()
        else if (showMenu)
            menu(track, menuButton)
    }
    // Ctrl and Shift select; without them the press goes on to the row.
    MouseArea {
        anchors.fill: parent
        z: 5
        enabled: d.selectable
        onPressed: (mouse) => mouse.accepted = (mouse.modifiers & (Qt.ControlModifier | Qt.ShiftModifier)) !== 0
        onClicked: (mouse) => {
            if (mouse.modifiers & Qt.ShiftModifier)
                d.selectRange()
            else
                d.selectToggled()
        }
    }

    Component.onCompleted: if ((track.local || track.source) && !track.cover) Covers.request(track.id)

    contentItem: RowLayout {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 4
        spacing: 12
        Item {
            visible: d.number >= 0 && !d.showCover
            Layout.preferredWidth: 28
            Layout.preferredHeight: 28
            Label {
                textFormat: Text.PlainText
                anchors.fill: parent
                visible: !d.current
                horizontalAlignment: Text.AlignRight
                verticalAlignment: Text.AlignVCenter
                text: d.number >= 0 ? d.number : ""
                color: Theme.textFaint
                font.features: { "tnum": 1 }
            }
            Icon { anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter; visible: d.current; name: "graphic_eq"; size: 20; color: Theme.accent }
        }
        CoverImage {
            visible: d.showCover
            Layout.preferredWidth: Theme.cover
            Layout.preferredHeight: Theme.cover
            source: Covers.url(d.track.id, d.track.cover)
            seed: d.track.album || d.track.title || ""
            radius: 8
            // Playing: the cover is dimmed behind an equalizer icon.
            Rectangle {
                anchors.fill: parent
                radius: 8
                visible: d.current && !d.selected
                color: Qt.rgba(0, 0, 0, 0.45)
                Icon { anchors.centerIn: parent; name: "graphic_eq"; size: 22; color: "white" }
            }
            Rectangle {
                anchors.fill: parent
                radius: 8
                visible: d.selected
                color: Theme.accent
                Icon { anchors.centerIn: parent; name: "check"; size: 22; color: Theme.onAccent }
            }
        }
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: d.track.title || ""
                color: d.current ? Theme.accentText : Theme.text
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
        // Kept on this device for offline use.
        Icon {
            visible: d.track.offline === true
            name: "download_done"
            size: 18
            color: Theme.textFaint
        }
        Label {
            textFormat: Text.PlainText
            text: d.track.duration > 0 ? Theme.fmtTime(d.track.duration) : ""
            color: Theme.textFaint
            font.pixelSize: 13
            font.features: { "tnum": 1 }
        }
        IconButton {
            visible: d.actionIcon !== ""
            iconName: d.actionIcon
            iconColor: Theme.textDim
            tip: d.actionTip
            onClicked: d.rowAction()
        }
        // On narrow screens the row needs the room; the menu has it too.
        FavoriteButton {
            trackId: d.track.id || ""
            visible: d.showMenu && Favorites.canMark(trackId) && (d.width >= 420 || on)
            iconColor: Theme.textDim
        }
        IconButton {
            id: menuButton
            visible: d.showMenu
            iconName: "more_vert"
            iconColor: Theme.textDim
            tip: Theme.tr("More")
            onClicked: d.menu(d.track, menuButton)
        }
    }
}
