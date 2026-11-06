import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The main navigation: a bar at the bottom on phones, a rail at the side
// otherwise (`dense`: icons only, for little height). What is used less sits
// behind "More".
Rectangle {
    id: nav
    property int current: -1
    property bool rail: false
    property bool dense: false
    signal selected(int index)
    signal showHelp()
    // The button of the i-th entry (the last: More), for the tour.
    function button(i) {
        return i < navRepeater.count ? navRepeater.itemAt(i) : moreButton
    }

    // `page`: what the item opens, see Main.qml.
    readonly property var items: [
        { icon: "home", label: Theme.tr("Home"), page: 5 },
        { icon: "search", label: Theme.tr("Search"), page: 7 },
        { icon: "library_music", label: Theme.tr("Library"), page: 0 },
        { icon: "auto_awesome", label: Theme.tr("Visuals"), page: 2 }
    ]
    readonly property var moreItems: [
        { icon: "bar_chart", label: Theme.tr("Statistics"), page: 6 },
        { icon: "devices", label: Theme.tr("Devices"), page: 4 },
        { icon: "settings", label: Theme.tr("Settings"), page: 3 }
    ].filter(i => i.page !== 4 || Core.state.server === "connected")

    color: Theme.surfaceStrong
    implicitHeight: rail ? 0 : Theme.fingers ? 72 : 56
    implicitWidth: rail ? (dense ? 56 : 72) : 0

    component NavButton: AbstractButton {
        id: btn
        property string iconName
        property bool sel: false
        focusPolicy: Qt.TabFocus
        Layout.fillWidth: true
        Layout.fillHeight: !nav.rail
        Layout.preferredHeight: nav.rail ? (nav.dense ? 44 : 60) : -1
        Accessible.role: Accessible.Button
        Accessible.name: text
        ToolTip.text: text
        ToolTip.visible: nav.dense && hovered
        ToolTip.delay: 300
        Keys.onReturnPressed: clicked()
        Keys.onEnterPressed: clicked()
        contentItem: Item {
            Column {
                anchors.centerIn: parent
                spacing: 4
                Rectangle {
                    anchors.horizontalCenter: parent.horizontalCenter
                    width: nav.dense ? 40 : 52
                    height: 28
                    radius: 16
                    color: btn.sel ? Qt.alpha(Theme.accent, 0.22) : (btn.hovered ? Theme.surfaceVariant : "transparent")
                    Behavior on color { ColorAnimation { duration: Theme.ms(150) } }
                    Icon {
                        anchors.centerIn: parent
                        name: btn.iconName
                        color: btn.sel ? Theme.accentText : Theme.textDim
                    }
                }
                Label {
                    textFormat: Text.PlainText
                    anchors.horizontalCenter: parent.horizontalCenter
                    visible: !nav.dense
                    text: btn.text
                    font.pixelSize: 12
                    font.weight: btn.sel ? Font.DemiBold : Font.Normal
                    color: btn.sel ? Theme.text : Theme.textDim
                }
            }
        }
        FocusRing { shown: btn.visualFocus; inset: 2 }
    }

    GridLayout {
        anchors.fill: parent
        anchors.topMargin: nav.rail ? (nav.dense ? 4 : 12) : 6
        anchors.bottomMargin: nav.rail ? (nav.dense ? 4 : 8) : 4
        flow: nav.rail ? GridLayout.TopToBottom : GridLayout.LeftToRight
        rows: nav.rail ? nav.items.length + 2 : 1
        columns: nav.rail ? 1 : nav.items.length + 1
        rowSpacing: nav.dense ? 2 : 4
        columnSpacing: 0
        Repeater {
            id: navRepeater
            model: nav.items
            delegate: NavButton {
                required property var modelData
                iconName: modelData.icon
                text: modelData.label
                sel: modelData.page === nav.current
                onClicked: nav.selected(modelData.page)
            }
        }
        Item { visible: nav.rail; Layout.fillHeight: true }
        NavButton {
            id: moreButton
            iconName: "more_horiz"
            text: Theme.tr("More")
            sel: nav.moreItems.some(m => m.page === nav.current)
            onClicked: {
                // Next to the rail, above the bar.
                if (nav.rail)
                    moreMenu.popup(moreButton, moreButton.width, moreButton.height - moreMenu.implicitHeight)
                else
                    moreMenu.popup(moreButton, moreButton.width - moreMenu.width, -moreMenu.implicitHeight)
            }
            Menu {
                id: moreMenu
                Repeater {
                    model: nav.moreItems.concat([{ icon: "info", label: Theme.tr("Help"), page: -1 }])
                    MenuItem {
                        required property var modelData
                        text: modelData.label
                        icon.source: Theme.icon(modelData.icon)
                        onTriggered: modelData.page >= 0 ? nav.selected(modelData.page) : nav.showHelp()
                    }
                }
            }
        }
    }
}
