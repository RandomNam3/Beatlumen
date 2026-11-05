pragma Singleton
import QtQuick
import Beatlumen.Core

// Which songs are favorites, for hearts that follow changes anywhere.
QtObject {
    property var ids: ({})
    readonly property int revision: Core.state.favoritesRev || 0
    onRevisionChanged: reload()
    Component.onCompleted: reload()

    function reload() {
        const m = {}
        for (const id of Core.call("favorites.ids") || [])
            m[id] = true
        ids = m
    }
    function has(id) {
        return ids[id] === true
    }
    function set(trackIds, on) {
        Core.call("favorites.set", { ids: trackIds, value: on })
    }
    function toggle(id) {
        set([id], !has(id))
    }
    // Radio stations and other live streams have no lasting song to mark.
    function canMark(id) {
        return !!id && String(id).indexOf("radio:") !== 0
    }
}
