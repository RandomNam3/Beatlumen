import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Beatlumen.Core

// The play queue: tap to jump, drag the handle to reorder, swipe to remove.
Item {
    id: page
    readonly property int rev: Core.state.queueRev || 0
    property var items: []
    function reload() { items = Core.call("queue.list") || [] }
    onRevChanged: reload()
    Component.onCompleted: reload()

    TrackMenu { id: trackMenu; onAddToPlaylist: (ids) => picker.show(ids) }
    PlaylistPicker { id: picker }
    TextPrompt {
        id: savePrompt
        title: Theme.tr("Save as playlist")
        placeholder: Theme.tr("Playlist name")
        acceptText: Theme.tr("Save")
        onSubmitted: (t) => Core.call("playlist.create", { name: t, kind: "Track", fromQueue: true, server: Core.state.server === "connected" })
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 8
        PageHeader {
            Layout.fillWidth: true
            title: Theme.tr("Queue")
            subtitle: page.items.length > 0 ? page.items.length + " " + Theme.tr("songs") : ""
            IconButton {
                iconName: "shuffle"; tip: Theme.tr("Shuffle"); active: Core.state.shuffle === true
                onClicked: Core.call("shuffle")
            }
            IconButton {
                iconName: "playlist_add"; tip: Theme.tr("Save as playlist")
                enabled: page.items.length > 0
                onClicked: savePrompt.open()
            }
            IconButton {
                iconName: "delete"; tip: Theme.tr("Clear queue")
                enabled: page.items.length > 0
                onClicked: Core.call("queue.clear")
            }
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            EmptyState {
                anchors.centerIn: parent
                width: Math.min(parent.width - 48, 420)
                visible: page.items.length === 0
                iconName: "queue_music"
                title: Theme.tr("The queue is empty")
                text: Theme.tr("Songs you play or add appear here.")
            }
            ListView {
                id: list
                anchors.fill: parent
                clip: true
                model: page.items
                ScrollBar.vertical: ScrollBar { }
                property int dragFrom: -1
                property int dragTo: -1
                Component.onCompleted: positionTimer.start()
                Timer {
                    id: positionTimer
                    interval: 50
                    onTriggered: {
                        const i = page.items.findIndex(t => t.current)
                        if (i >= 0)
                            list.positionViewAtIndex(i, ListView.Center)
                    }
                }

                delegate: SwipeDelegate {
                    id: row
                    required property var modelData
                    required property int index
                    width: ListView.view.width
                    height: Theme.row
                    padding: 0
                    highlighted: modelData.current
                    opacity: list.dragFrom === index ? 0.4 : 1
                    onClicked: Core.call("queue.play", { pos: modelData.pos })
                    onPressAndHold: trackMenu.openFor([modelData.id], more, [{ text: Theme.tr("Remove from queue"), action: () => Core.call("queue.remove", { pos: modelData.pos }) }])
                    Component.onCompleted: if (modelData.local && !modelData.cover) Covers.request(modelData.id)
                    swipe.right: Rectangle {
                        width: parent.width
                        height: parent.height
                        color: Theme.danger
                        Icon { anchors.right: parent.right; anchors.rightMargin: 24; anchors.verticalCenter: parent.verticalCenter; name: "delete"; color: "white" }
                    }
                    swipe.onCompleted: Core.call("queue.remove", { pos: modelData.pos })
                    Rectangle {
                        visible: list.dragTo === row.index && list.dragFrom !== row.index
                        anchors.left: parent.left
                        anchors.right: parent.right
                        y: list.dragTo > list.dragFrom ? parent.height - 2 : 0
                        height: 2
                        color: Theme.accent
                    }
                    contentItem: RowLayout {
                        spacing: 8
                        Item {
                            Layout.preferredWidth: Theme.touch
                            Layout.fillHeight: true
                            Icon { anchors.centerIn: parent; name: "drag_handle"; color: Theme.textFaint }
                            DragHandler {
                                id: drag
                                target: null
                                yAxis.enabled: true
                                xAxis.enabled: false
                                onActiveChanged: {
                                    if (active) {
                                        list.dragFrom = row.index
                                        list.dragTo = row.index
                                    } else {
                                        if (list.dragFrom >= 0 && list.dragTo >= 0 && list.dragTo !== list.dragFrom)
                                            Core.call("queue.move", { from: page.items[list.dragFrom].pos, to: page.items[list.dragTo].pos })
                                        list.dragFrom = -1
                                        list.dragTo = -1
                                    }
                                }
                                onTranslationChanged: {
                                    const y = row.y + row.height / 2 + translation.y
                                    list.dragTo = Math.max(0, Math.min(page.items.length - 1, list.indexAt(10, y)))
                                }
                            }
                        }
                        CoverImage {
                            Layout.preferredWidth: 44
                            Layout.preferredHeight: 44
                            radius: 8
                            source: Covers.url(row.modelData.id, row.modelData.cover)
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label { Layout.fillWidth: true; text: row.modelData.title; elide: Text.ElideRight; color: row.modelData.current ? Theme.accent : Theme.text; font.pixelSize: 15; font.weight: row.modelData.current ? Font.DemiBold : Font.Medium }
                            Label { Layout.fillWidth: true; text: row.modelData.artist || Theme.tr("Unknown artist"); elide: Text.ElideRight; color: Theme.textDim; font.pixelSize: 13 }
                        }
                        Label { text: row.modelData.duration > 0 ? Theme.fmtTime(row.modelData.duration) : ""; color: Theme.textFaint; font.pixelSize: 13 }
                        IconButton {
                            id: more
                            iconName: "more_vert"
                            iconColor: Theme.textDim
                            tip: Theme.tr("More")
                            onClicked: trackMenu.openFor([row.modelData.id], more, [{ text: Theme.tr("Remove from queue"), action: () => Core.call("queue.remove", { pos: row.modelData.pos }) }])
                        }
                    }
                }
            }
        }
    }
}
