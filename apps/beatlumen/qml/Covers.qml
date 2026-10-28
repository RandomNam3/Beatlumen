pragma Singleton
import QtQuick
import Beatlumen.Core

// Cover URLs of local tracks are extracted on demand by the core. Views ask
// with request(id) and bind to url(id, fallback); `revision` changes when
// new covers arrive.
QtObject {
    property var urls: ({})
    property int revision: 0
    property var pending: []

    function url(id, fallback) {
        revision // dependency
        const u = urls[id]
        return u !== undefined ? u : (fallback || "")
    }

    function request(id) {
        if (urls[id] !== undefined || pending.indexOf(id) >= 0)
            return
        pending.push(id)
        flushTimer.restart()
    }

    property Timer flushTimer: Timer {
        interval: 30
        onTriggered: {
            const ids = pending
            pending = []
            if (ids.length > 0)
                Core.call("covers", { ids: ids })
        }
    }

    property Connections conn: Connections {
        target: Core
        function onCoverReady(id, url) {
            urls[id] = url
            revision++
        }
    }
}
