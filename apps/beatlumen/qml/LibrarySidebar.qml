import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Beside the library in wide windows: its sections, filters for the songs, and
// what the user pinned.
Item {
    id: side
    // [{ label, icon }]
    property var sections: []
    property int section: 0
    property string filter: "all"
    // [{ kind, id, name }]
    property var pinned: []
    signal sectionChosen(int index)
    signal filterChosen(string filter)
    signal pinnedOpened(var item)
    signal unpin(var item)

    readonly property bool serverOn: Core.state.server === "connected"
    readonly property var filters: [
        { id: "all", label: Theme.tr("All songs"), icon: "library_music" },
        { id: "favorites", label: Theme.tr("Favorites"), icon: "favorite_border" },
        { id: "local", label: Theme.tr("On this device"), icon: "folder" },
        { id: "server", label: Theme.tr("On the server"), icon: "cloud", hidden: !serverOn },
        { id: "offline", label: Theme.tr("Kept offline"), icon: "download_done", hidden: !serverOn }
    ].filter(f => !f.hidden)

    component Row: ItemDelegate {
        id: row
        property string iconName
        property bool sel: false
        width: parent ? parent.width : implicitWidth
        height: Theme.fingers ? 44 : 34
        leftPadding: 10
        rightPadding: 6
        highlighted: sel
        Accessible.name: text
        Accessible.checkable: true
        Accessible.checked: sel
        contentItem: RowLayout {
            spacing: 10
            Icon { name: row.iconName; size: 18; color: row.sel ? Theme.accentText : Theme.textDim }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: row.text
                color: row.sel ? Theme.accentText : Theme.text
                font.pixelSize: 14
                elide: Text.ElideRight
            }
        }
    }

    function pinIcon(kind) {
        return { album: "album", artist: "person", playlist: "queue_music", label: "label" }[kind] || "label"
    }

    Flickable {
        anchors.fill: parent
        contentHeight: column.implicitHeight
        boundsBehavior: Flickable.StopAtBounds
        clip: true
        ScrollBar.vertical: ScrollBar { }
        Column {
            id: column
            width: parent.width
            spacing: 2
            Repeater {
                model: side.sections
                Row {
                    required property var modelData
                    required property int index
                    iconName: modelData.icon
                    text: modelData.label
                    sel: side.section === index
                    onClicked: side.sectionChosen(index)
                }
            }
            SectionLabel {
                visible: side.section === 0
                text: Theme.tr("Filter")
                topPadding: 12
            }
            Repeater {
                model: side.section === 0 ? side.filters : []
                Row {
                    required property var modelData
                    iconName: modelData.icon
                    text: modelData.label
                    sel: side.filter === modelData.id
                    onClicked: side.filterChosen(modelData.id)
                }
            }
            SectionLabel {
                visible: side.pinned.length > 0
                text: Theme.tr("Pinned")
                topPadding: 12
            }
            Repeater {
                model: side.pinned
                Row {
                    id: pinRow
                    required property var modelData
                    iconName: side.pinIcon(modelData.kind)
                    text: modelData.name
                    onClicked: side.pinnedOpened(modelData)
                    IconButton {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        visible: pinRow.hovered || Theme.fingers
                        iconName: "close"
                        iconSize: 16
                        implicitWidth: 28
                        implicitHeight: 28
                        tip: Theme.tr("Unpin")
                        onClicked: side.unpin(pinRow.modelData)
                    }
                }
            }
        }
    }
}
