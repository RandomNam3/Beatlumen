import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Bottom navigation for phones; `rail` turns it into a vertical rail.
Rectangle {
    id: nav
    property int current: -1
    property bool rail: false
    signal selected(int index)

    // `page`: what the item opens (-1 = now playing). Devices only with a
    // server connection.
    readonly property var all: [
        { icon: "graphic_eq", label: Theme.tr("Now playing"), page: -1 },
        { icon: "library_music", label: Theme.tr("Library"), page: 0 },
        { icon: "queue_music", label: Theme.tr("Queue"), page: 1 },
        { icon: "auto_awesome", label: Theme.tr("Visuals"), page: 2 },
        { icon: "devices", label: Theme.tr("Devices"), page: 4 },
        { icon: "settings", label: Theme.tr("Settings"), page: 3 }
    ].filter(i => i.page !== 4 || Core.state.server === "connected")
    // Phones: at most five; the rest behind "More" (page -2).
    readonly property var items: rail || all.length <= 5 ? all
        : all.slice(0, 4).concat([{ icon: "more_vert", label: Theme.tr("More"), page: -2 }])
    readonly property var more: rail || all.length <= 5 ? [] : all.slice(4)

    Menu {
        id: moreMenu
        Repeater {
            model: nav.more
            MenuItem {
                text: modelData.label
                icon.source: Theme.icon(modelData.icon)
                onTriggered: nav.selected(modelData.page)
            }
        }
    }

    color: Theme.surfaceStrong
    implicitHeight: rail ? 0 : 80
    implicitWidth: rail ? 88 : 0

    GridLayout {
        anchors.fill: parent
        anchors.topMargin: nav.rail ? 12 : 10
        anchors.bottomMargin: nav.rail ? 0 : 6
        flow: nav.rail ? GridLayout.TopToBottom : GridLayout.LeftToRight
        rows: nav.rail ? nav.items.length + 1 : 1
        columns: nav.rail ? 1 : nav.items.length
        rowSpacing: 4
        columnSpacing: 0
        Repeater {
            model: nav.items
            delegate: AbstractButton {
                id: btn
                readonly property bool sel: modelData.page === nav.current
                    || (modelData.page === -2 && nav.more.some(m => m.page === nav.current))
                Layout.fillWidth: true
                Layout.fillHeight: !nav.rail
                Layout.preferredHeight: nav.rail ? 72 : -1
                text: modelData.label
                Accessible.name: modelData.label
                onClicked: {
                    if (modelData.page === -2) {
                        moreMenu.y = -moreMenu.implicitHeight
                        moreMenu.x = btn.x + btn.width - moreMenu.width
                        moreMenu.open()
                    } else {
                        nav.selected(modelData.page)
                    }
                }
                contentItem: Column {
                    spacing: 4
                    anchors.centerIn: parent
                    Rectangle {
                        anchors.horizontalCenter: parent.horizontalCenter
                        width: 60
                        height: 32
                        radius: 16
                        color: btn.sel ? Qt.alpha(Theme.accent, 0.22) : (btn.hovered ? Theme.surfaceVariant : "transparent")
                        Behavior on color { ColorAnimation { duration: 150 } }
                        Icon {
                            anchors.centerIn: parent
                            name: modelData.icon
                            color: btn.sel ? Theme.accent : Theme.textDim
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: modelData.label
                        font.pixelSize: 12
                        font.weight: btn.sel ? Font.DemiBold : Font.Normal
                        color: btn.sel ? Theme.text : Theme.textDim
                    }
                }
            }
        }
        Item { visible: nav.rail; Layout.fillHeight: true }
    }
}
