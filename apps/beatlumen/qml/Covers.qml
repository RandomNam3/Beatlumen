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

    // What an Image may load. Native apps show pictures only through the
    // core's providers (decoded by its own decoders, image://server and
    // image://thumb), the app's own files and PNGs the core made; any other
    // URL shows the placeholder instead of reaching Qt's image readers.
    // The browser build shows server URLs as they are.
    readonly property bool nativeApp: Qt.platform.os !== "wasm"
    function safe(u) {
        if (!u || !nativeApp)
            return u || ""
        const s = String(u)
        return s.startsWith("image://") || s.startsWith("qrc:") || s.startsWith("data:image/png;") ? s : ""
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
