import QtQuick

// What can be done with one or more songs: a menu beside the pointer on a
// computer, a sheet for fingers. Made when first needed, a page may never ask.
Item {
    id: root
    property bool canEditLabels: false
    signal addToPlaylist(var ids)
    signal editLabels(string id)

    property var actions: null
    function openFor(trackIds, anchor, extraItems) {
        if (!actions)
            actions = (Theme.fingers ? sheet : menu).createObject(root)
        if (Theme.fingers)
            actions.openFor(trackIds, extraItems)
        else
            actions.openFor(trackIds, anchor, extraItems)
    }

    Component {
        id: menu
        TrackMenu {
            canEditLabels: root.canEditLabels
            onAddToPlaylist: (ids) => root.addToPlaylist(ids)
            onEditLabels: (id) => root.editLabels(id)
        }
    }
    Component {
        id: sheet
        TrackSheet {
            canEditLabels: root.canEditLabels
            onAddToPlaylist: (ids) => root.addToPlaylist(ids)
            onEditLabels: (id) => root.editLabels(id)
        }
    }
}
