import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Settings, grouped by topic.
Item {
    id: page
    signal addFolder()
    signal login()
    signal openVault()
    readonly property var settings: Core.state.settings || ({})
    readonly property var ui: settings.ui || ({})
    readonly property string server: Core.state.server || "disabled"
    readonly property var sources: Core.state.sources || ({})
    readonly property var speechLocal: Core.state.speechLocal || ({})

    // Shown category ("" = all) and the search.
    property string category: "look"
    readonly property bool wide: width >= 720
    readonly property var categories: [
        { id: "look", label: Theme.tr("Display") },
        { id: "playback", label: Theme.tr("Playback") },
        { id: "library", label: Theme.tr("Library & sources") },
        { id: "server", label: Theme.tr("Server & account") },
        { id: "receive", label: Theme.tr("Receiving") },
        { id: "texts", label: Theme.tr("Song texts") },
        { id: "about", label: Theme.tr("About") }
    ].filter(c => c.id !== "texts" || speechLocal.available === true)
    // Whether the search found anything (set after the sections updated).
    property bool anyShown: true
    function updateShown() {
        anyShown = sectionsColumn.children.some(c => c.objectName === "section" && c.visible)
    }
    function shows(cat, words) {
        const q = settingsSearch.text.trim().toLowerCase()
        if (q.length > 0)
            return words.some(w => Theme.tr(w).toLowerCase().indexOf(q) >= 0 || w.toLowerCase().indexOf(q) >= 0)
        return category === cat
    }

    SourceDialog { id: sourceDialog }

    // Plain HTTP is fine at home; elsewhere the password and the login
    // would travel unencrypted.
    function insecureRemote(url) {
        const m = /^http:\/\/(\[[^\]]+\]|[^/:]+)/i.exec((url || "").trim())
        if (!m)
            return false
        const h = m[1].toLowerCase()
        return !(h === "localhost" || /\.(local|lan|home\.arpa|internal)$/.test(h)
                 || /^(10|127)\./.test(h) || /^192\.168\./.test(h) || /^172\.(1[6-9]|2\d|3[01])\./.test(h)
                 || /^169\.254\./.test(h) || /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(h)
                 || /^\[(::1|f[cd]|fe80)/.test(h) || !h.includes("."))
    }

    // On wide windows the categories sit at the side.
    Column {
        id: categoryColumn
        visible: page.wide
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.topMargin: 48
        width: 168
        spacing: 2
        Repeater {
            model: page.categories
            ItemDelegate {
                required property var modelData
                width: categoryColumn.width
                height: Theme.fingers ? 44 : 36
                highlighted: settingsSearch.text === "" && page.category === modelData.id
                Accessible.role: Accessible.Button
                Accessible.name: modelData.label
                // Plain text: a "&" in a name is no shortcut key.
                contentItem: Label {
                    textFormat: Text.PlainText
                    text: modelData.label
                    color: Theme.text
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                onClicked: { settingsSearch.text = ""; page.category = modelData.id }
            }
        }
    }

    ScrollView {

        // Vertical only: an (invisible) horizontal bar would catch taps

        // along the bottom edge.

        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        id: sv
        anchors.left: page.wide ? categoryColumn.right : parent.left
        anchors.leftMargin: page.wide ? 8 : 0
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        clip: true
        contentWidth: availableWidth
        ColumnLayout {
            id: sectionsColumn
            width: Math.min(sv.availableWidth, 720)
            spacing: 8
            PageHeader { Layout.fillWidth: true; title: Theme.tr("Settings") }

            // Search and categories.
            InputField {
                Accessible.name: placeholderText
                id: settingsSearch
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                placeholderText: Theme.tr("Search settings")
                inputMethodHints: Qt.ImhNoPredictiveText
                onTextChanged: Qt.callLater(page.updateShown)
            }
            Flickable {
                visible: !page.wide
                Layout.fillWidth: true
                Layout.preferredHeight: catRow.height
                contentWidth: catRow.width + 24
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                flickableDirection: Flickable.HorizontalFlick
                Row {
                    id: catRow
                    x: 12
                    spacing: 6
                    Repeater {
                        model: page.categories
                        Chip {
                            required property var modelData
                            text: modelData.label
                            checked: page.category === modelData.id
                            onClicked: { settingsSearch.text = ""; page.category = modelData.id }
                        }
                    }
                }
            }
            Label {
                visible: settingsSearch.text.length > 0 && !page.anyShown
                Layout.fillWidth: true
                Layout.leftMargin: 16
                textFormat: Text.PlainText
                color: Theme.textDim
                text: Theme.tr("No setting matches.")
            }

            // ---- accessibility ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("look", ["Accessibility", "Epilepsy mode", "Reduce motion", "High contrast", "Keep the controls visible", "Size of everything"])
                SectionLabel { text: Theme.tr("Accessibility") }
                AccessOptions { Layout.fillWidth: true }
            }

            // ---- appearance ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 12
                visible: page.shows("look", ["Accent colour", "Appearance", "Dark", "Language", "Light", "System", "Theme"])
                SectionLabel { text: Theme.tr("Appearance") }
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 4
                    Label { textFormat: Text.PlainText; text: Theme.tr("Theme"); color: Theme.text }
                    Flow {
                        Layout.fillWidth: true
                        spacing: 8
                        Repeater {
                            model: [{ v: "system", l: Theme.tr("System") }, { v: "light", l: Theme.tr("Light") }, { v: "dark", l: Theme.tr("Dark") }]
                            delegate: AccentButton {
                                text: modelData.l
                                checkable: true
                                flat: !checked
                                highlighted: checked
                                checked: (page.ui.theme || "system") === modelData.v
                                onClicked: Core.call("settings.set", { key: "ui.theme", value: modelData.v })
                            }
                        }
                    }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Label { textFormat: Text.PlainText; text: Theme.tr("Accent colour"); color: Theme.text }
                    Flow {
                        Layout.fillWidth: true
                        spacing: 10
                        Repeater {
                            model: ["", "#e91e63", "#ff5722", "#ff9800", "#ffc107", "#8bc34a", "#00bfa5", "#00b0ff", "#3d5afe", "#7c4dff", "#d500f9"]
                            delegate: AbstractButton {
                                width: Theme.fingers ? 40 : 28; height: width
                                readonly property string tip: modelData === "" ? "accent:system" : modelData
                                Accessible.name: modelData === "" ? Theme.tr("System") : modelData
                                onClicked: Core.call("settings.set", { key: "ui.accent", value: modelData })
                                background: Rectangle {
                                    radius: width / 2
                                    color: modelData === "" ? Core.systemAccent : modelData
                                    border.width: (page.ui.accent || "") === modelData ? 3 : 0
                                    border.color: Theme.text
                                    Icon {
                                        anchors.centerIn: parent
                                        readonly property bool selected: (page.ui.accent || "") === modelData
                                        visible: modelData === "" || selected
                                        name: modelData === "" ? "hdr_auto" : "check"
                                        color: Theme.onColor(parent.color)
                                        size: Theme.fingers ? 20 : 16
                                    }
                                }
                            }
                        }
                    }
                }
                RowLayout {
                    Layout.leftMargin: 16
                    spacing: 8
                    Label { textFormat: Text.PlainText; text: Theme.tr("Language"); color: Theme.text; Layout.preferredWidth: 120 }
                    Dropdown {
                        Accessible.name: Theme.tr("Language")
                        model: [{ v: "de", l: "Deutsch" }, { v: "en", l: "English" }]
                        textRole: "l"
                        valueRole: "v"
                        currentIndex: Theme.language === "de" ? 0 : 1
                        onActivated: (i) => Core.call("settings.set", { key: "language", value: i === 0 ? "de" : "en" })
                    }
                }
            }
            // ---- playback ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("playback", ["Crossfade", "No silent gaps between songs.", "Normalize volume", "Off", "Playback", "Plays all songs at a similar loudness.", "Skip silence"])
                SectionLabel { text: Theme.tr("Playback") }
                SwitchRow {
                    Layout.fillWidth: true
                    title: Theme.tr("Normalize volume")
                    detail: Theme.tr("Plays all songs at a similar loudness.")
                    on: page.settings.normalize === true
                    onSwitched: (on) => Core.call("settings.set", { key: "normalize", value: on })
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 0
                    RowLayout {
                        Layout.fillWidth: true
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Crossfade"); color: Theme.text; font.pixelSize: 14 }
                        Label {
                            textFormat: Text.PlainText
                            text: crossfadeSlider.value < 0.5 ? Theme.tr("Off") : Math.round(crossfadeSlider.value) + " s"
                            color: Theme.textDim
                        }
                    }
                    Slider {
                        id: crossfadeSlider
                        Layout.fillWidth: true
                        from: 0; to: 12; stepSize: 1
                        value: page.settings.crossfade || 0
                        onMoved: Core.call("settings.set", { key: "crossfade", value: value })
                        Accessible.name: Theme.tr("Crossfade")
                    }
                }
                SwitchRow {
                    Layout.fillWidth: true
                    title: Theme.tr("Skip silence")
                    detail: Theme.tr("No silent gaps between songs.")
                    on: page.settings.skipSilence !== false
                    onSwitched: (on) => Core.call("settings.set", { key: "skipSilence", value: on })
                }
            }
            // ---- equalizer ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("playback", ["Equalizer", "Bass, mids and treble.", "More bass", "More treble", "Voices", "Bit-perfect playback", "Signal path", "Sound card"])
                SectionLabel { text: Theme.tr("Sound") }
                DirectOutputPanel { Layout.fillWidth: true }
                EqualizerPanel { Layout.fillWidth: true }
            }
            // ---- library ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("library", ["Add music folder", "Allow access to music", "Music folders", "Remove", "Rescan"])
                SectionLabel { text: Theme.tr("Music folders"); visible: Core.platform !== "web" }
                Repeater {
                    model: Core.platform !== "web" ? (page.settings.libraryFolders || []) : []
                    delegate: ItemDelegate {
                        Layout.fillWidth: true
                        height: Theme.row - 8
                        contentItem: RowLayout {
                            Icon { name: "folder"; color: Theme.accent }
                            Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData; elide: Text.ElideMiddle; color: Theme.text }
                            IconButton { iconName: "close"; tip: Theme.tr("Remove"); onClicked: Core.call("library.removeFolder", { path: modelData }) }
                        }
                    }
                }
                Flow {
                    visible: Core.platform !== "web"
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    spacing: 4
                    Button { text: Theme.tr("Add music folder"); icon.source: Theme.icon("create_new_folder"); onClicked: page.addFolder() }
                    Button { text: Theme.tr("Rescan"); icon.source: Theme.icon("refresh"); flat: true; onClicked: Core.call("library.rescan") }
                    Button {
                        visible: Core.platform === "android"
                        text: Theme.tr("Allow access to music")
                        flat: true
                        onClicked: Core.requestMediaPermission()
                    }
                }
            }
            // ---- artist information from the internet ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("library", ["Artist information", "Look artists up on the internet", "Forget what was found"])
                SectionLabel { text: Theme.tr("Artist information") }
                SwitchRow {
                    Layout.fillWidth: true
                    title: Theme.tr("Look artists up on the internet")
                    detail: Theme.tr("Artist names and your IP address go to MusicBrainz, Wikimedia and ListenBrainz.")
                    info: Theme.tr("For artist pages and better recommendations. Nothing else goes out. Off, none of them is asked and nothing found earlier is shown.")
                    on: page.settings.onlineMetadata === true
                    onSwitched: (on) => Core.call("settings.set", { key: "onlineMetadata", value: on })
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    visible: page.server === "connected"
                    text: Theme.tr("A server has its own switch in its admin area.")
                    color: Theme.textDim
                    font.pixelSize: 12
                    wrapMode: Text.WordWrap
                }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    visible: ((Core.state.artists || {}).known || 0) > 0 || page.settings.onlineMetadata === true
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        readonly property var artists: Core.state.artists || ({})
                        text: Theme.tr("%1 artists known, %2 still to ask").arg(artists.known || 0).arg(artists.waiting || 0)
                        color: Theme.textDim
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }
                    Button {
                        flat: true
                        text: Theme.tr("Forget what was found")
                        onClicked: Core.call("artists.clear")
                    }
                }
            }
            // ---- music sources (without a server) ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("library", ["Add music source", "Listing songs…", "Music sources", "Off", "Subsonic, Navidrome, Jellyfin, WebDAV, Nextcloud or DLNA, directly on this device.", "Remove", "Sync now", "Trust key"])
                SectionLabel { text: Theme.tr("Music sources"); visible: page.sources.available === true }
                Label {
                    visible: page.sources.available === true && (page.sources.list || []).length === 0
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.Wrap
                    color: Theme.textDim
                    font.pixelSize: 12
                    text: Theme.tr("Subsonic, Navidrome, Jellyfin, WebDAV, Nextcloud or DLNA, directly on this device.")
                }
                Repeater {
                    model: page.sources.available === true ? (page.sources.list || []) : []
                    delegate: ItemDelegate {
                        id: srcRow
                        Layout.fillWidth: true
                        height: Theme.row
                        onClicked: sourceDialog.openFor(modelData)
                        contentItem: RowLayout {
                            Icon { name: modelData.kind === "dlna" ? "storage" : "cloud"; color: modelData.error ? Theme.danger : Theme.accent }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.name; elide: Text.ElideRight; color: Theme.text }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    font.pixelSize: 12
                                    color: modelData.error ? Theme.danger : Theme.textDim
                                    text: !modelData.enabled ? Theme.tr("Off")
                                        : modelData.syncing ? Theme.tr("Listing songs…")
                                        : modelData.untrustedKey ? Theme.tr("New server key %1: compare it with the server's before trusting it").arg(modelData.untrustedKey)
                                        : modelData.error ? modelData.error
                                        : Theme.tr("%1 songs").arg(modelData.songs)
                                }
                            }
                            BusyIndicator { running: modelData.syncing; visible: running; implicitWidth: 28; implicitHeight: 28 }
                            Button {
                                visible: !!modelData.untrustedKey
                                text: Theme.tr("Trust key")
                                flat: true
                                ToolTip.visible: hovered
                                ToolTip.text: modelData.untrustedKey
                                onClicked: Core.call("sources.trust", { id: modelData.id })
                            }
                            IconButton { iconName: "refresh"; tip: Theme.tr("Sync now"); enabled: !modelData.syncing && modelData.enabled; onClicked: Core.call("sources.sync", { id: modelData.id }) }
                            IconButton { iconName: "close"; tip: Theme.tr("Remove"); onClicked: Core.call("sources.remove", { id: modelData.id }) }
                        }
                    }
                }
                Flow {
                    visible: page.sources.available === true
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    Button { text: Theme.tr("Add music source"); icon.source: Theme.icon("add"); onClicked: sourceDialog.openFor(null) }
                }
            }
            // ---- speech recognition on this device ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("texts", ["Delete", "Download", "In use", "Recognise and time song texts", "Turn off", "Use", "Time all songs on this device", "songs left", "A speech model times song texts and writes down songs without one."])
                SectionLabel { text: Theme.tr("Recognise and time song texts"); visible: page.speechLocal.available === true }
                Label {
                    visible: page.speechLocal.available === true
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.Wrap
                    color: Theme.textDim
                    font.pixelSize: 12
                    text: Theme.tr("A speech model times song texts and writes down songs without one.")
                }
                Repeater {
                    id: speechModels
                    property int tick: 0
                    model: {
                        page.speechLocal.rev; tick
                        return page.speechLocal.available === true ? (Core.call("speech.list") || []) : []
                    }
                    delegate: ItemDelegate {
                        Layout.fillWidth: true
                        height: Theme.row
                        Accessible.role: Accessible.ListItem
                        Accessible.name: modelData.name || modelData.id || ""
                        onClicked: if (!modelData.current && !modelData.state) Core.call("speech.pick", { id: modelData.id })
                        contentItem: RowLayout {
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.id; color: Theme.text; elide: Text.ElideRight }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    font.pixelSize: 12
                                    color: modelData.state === "failed" ? Theme.danger : Theme.textDim
                                    text: modelData.state === "failed" ? modelData.error
                                        : [Theme.tr(modelData.note), Math.round(modelData.size / 1e6) + " MB", Theme.tr("%1 MB of memory").arg(modelData.memoryMb)].join(" · ")
                                }
                                ProgressBar {
                                    visible: modelData.state === "downloading"
                                    Layout.fillWidth: true
                                    value: modelData.total ? modelData.done / modelData.total : 0
                                }
                            }
                            Label {
                                visible: modelData.current === true
                                textFormat: Text.PlainText
                                text: Theme.tr("In use")
                                color: Theme.accent
                            }
                            Button {
                                visible: !modelData.current && !modelData.state
                                flat: true
                                text: modelData.downloaded ? Theme.tr("Use") : Theme.tr("Download")
                                onClicked: Core.call("speech.pick", { id: modelData.id })
                            }
                            IconButton {
                                visible: modelData.downloaded === true
                                iconName: "delete"
                                tip: Theme.tr("Delete")
                                onClicked: Core.call("speech.delete", { id: modelData.id })
                            }
                        }
                    }
                }
                Timer {
                    // Download progress.
                    interval: 1000
                    repeat: true
                    running: page.visible && page.speechLocal.available === true
                    onTriggered: speechModels.tick++
                }
                Flow {
                    visible: page.speechLocal.available === true && (page.settings.speechModel || "") !== ""
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Button { text: Theme.tr("Turn off"); flat: true; onClicked: Core.call("speech.pick", { id: "" }) }
                }
                Label {
                    // Once a song was done on the graphics card.
                    visible: page.speechLocal.ready === true && !!page.speechLocal.gpu
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.Wrap
                    color: Theme.textDim
                    font.pixelSize: 12
                    text: Theme.tr("Computed on the graphics card: %1").arg(page.speechLocal.gpu || "")
                }
                // All songs of this device, one after the other.
                Label {
                    visible: page.speechLocal.ready === true
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.Wrap
                    color: Theme.textDim
                    font.pixelSize: 12
                    text: Theme.tr("Songs already done are skipped unless the model is better.")
                }
                RowLayout {
                    visible: page.speechLocal.ready === true
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 16
                    readonly property var all: page.speechLocal.all || null
                    Button {
                        visible: !parent.all
                        text: Theme.tr("Time all songs on this device")
                        icon.source: Theme.icon("auto_awesome")
                        onClicked: {
                            const r = Core.call("speech.runAll")
                            if (r && r.error)
                                Core.call("toast", { text: r.error })
                        }
                    }
                    ProgressBar {
                        visible: !!parent.all
                        Layout.fillWidth: true
                        value: parent.all ? 1 - (parent.all.left + (page.speechLocal.running ? 1 - page.speechLocal.running.progress : 0)) / Math.max(1, parent.all.total) : 0
                    }
                    Label {
                        visible: !!parent.all
                        textFormat: Text.PlainText
                        color: Theme.textDim
                        text: parent.all ? (parent.all.left + (page.speechLocal.running ? 1 : 0)) + " " + Theme.tr("songs left") : ""
                    }
                    Button {
                        visible: !!parent.all
                        flat: true
                        text: Theme.tr("Stop")
                        onClicked: Core.call("speech.cancel")
                    }
                }
            }
            // ---- translating song texts on this device ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.speechLocal.available === true && page.shows("texts", ["Translate song texts", "Shows each line in this app's language too (about 300 MB once, stays on this device).", "Translation", "Delete"])
                SectionLabel { text: Theme.tr("Translation") }
                SwitchRow {
                    Layout.fillWidth: true
                    title: Theme.tr("Translate song texts")
                    detail: Theme.tr("Shows each line in this app's language too (about 300 MB once, stays on this device).")
                    on: page.settings.translate === true
                    onSwitched: (on) => Core.call("translate.set", { on: on })
                }
                Repeater {
                    id: trModels
                    property int tick: 0
                    // Only the ones on this device (or coming).
                    model: {
                        tick; page.settings.translate
                        const all = page.visible && page.speechLocal.available === true ? (Core.call("translate.list") || []) : []
                        return Array.isArray(all) ? all.filter(m => m.downloaded || m.state) : []
                    }
                    delegate: ItemDelegate {
                        Layout.fillWidth: true
                        height: Theme.row
                        contentItem: RowLayout {
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    color: Theme.text
                                    text: Theme.tr(Theme.languageName(modelData.from)) + " → " + Theme.tr(Theme.languageName(modelData.to))
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    font.pixelSize: 12
                                    color: Theme.textDim
                                    text: Math.round(modelData.size / 1e6) + " MB"
                                }
                                ProgressBar {
                                    visible: modelData.state === "downloading"
                                    Layout.fillWidth: true
                                    value: modelData.total ? modelData.done / modelData.total : 0
                                }
                            }
                            IconButton {
                                visible: modelData.downloaded === true
                                iconName: "delete"
                                tip: Theme.tr("Delete")
                                onClicked: { Core.call("translate.delete", { id: modelData.id }); trModels.tick++ }
                            }
                        }
                    }
                }
                Timer {
                    // Download progress.
                    interval: 1000
                    repeat: true
                    running: page.visible && page.settings.translate === true
                    onTriggered: trModels.tick++
                }
            }
            // ---- server ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("server", ["Check", "Checking the web app…", "Connect", "Connected to", "Connecting…", "Connection failed", "Disconnect", "Log in", "Log out", "Login required", "Not connected", "Outside your home network, use https:// or a VPN.", "Server", "The server's certificate is not trusted by this device. Only trust it if this fingerprint (SHA-256) matches the certificate of your server:", "Trust this certificate", "Warning: the web app on the server was modified.", "Warning: this address is outside your home network and the connection is not encrypted. Use https:// or a VPN.", "Web app on the server: files match its manifest, but this app cannot check the release signature (unofficial build)", "Web app on the server: not checked yet", "Web app on the server: unchanged official release"])
                SectionLabel { text: Theme.tr("Server") }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    text: page.server === "connected" ? Theme.tr("Connected to") + " " + (Core.state.serverName || Core.state.serverUrl)
                        : page.server === "connecting" ? Theme.tr("Connecting…")
                        : page.server === "login" ? Theme.tr("Login required")
                        : page.server === "error" ? Theme.tr("Connection failed") + ": " + (Core.state.serverError || "")
                        : Theme.tr("Not connected")
                    color: page.server === "error" ? Theme.danger : Theme.textDim
                    wrapMode: Text.WordWrap
                }
                RowLayout {
                    visible: Core.platform !== "web"
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    InputField {
                        Accessible.name: placeholderText
                        id: url
                        Layout.fillWidth: true
                        placeholderText: "http://192.168.1.10:7878"
                        text: Core.state.serverUrl || page.settings.serverUrl || ""
                        inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
                        onAccepted: Core.call("server.connect", { url: text })
                    }
                    AccentButton { text: Theme.tr("Connect"); highlighted: true; onClicked: Core.call("server.connect", { url: url.text }) }
                }
                Flow {
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    spacing: 4
                    Button { visible: page.server === "login"; text: Theme.tr("Log in"); icon.source: Theme.icon("login"); onClicked: page.login() }
                    Button { visible: page.server === "connected" && page.settings.serverToken !== ""; text: Theme.tr("Log out"); flat: true; icon.source: Theme.icon("logout"); onClicked: Core.call("server.logout") }
                    Button { visible: Core.platform !== "web" && page.server !== "disabled"; text: Theme.tr("Disconnect"); flat: true; icon.source: Theme.icon("cloud_off"); onClicked: Core.call("server.disconnect") }
                }
                // Self-signed certificate (home server): show its fingerprint so
                // the user can compare it before trusting it.
                ColumnLayout {
                    readonly property string fp: Core.state.serverUntrustedCertificate || ""
                    visible: fp.length === 64
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 4
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: Theme.tr("The server's certificate is not trusted by this device. Only trust it if this fingerprint (SHA-256) matches the certificate of your server:")
                        wrapMode: Text.WordWrap
                        color: Theme.text
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: parent.fp.toUpperCase().match(/../g) ? parent.fp.toUpperCase().match(/../g).join(":") : ""
                        font.family: "monospace"
                        font.pixelSize: 12
                        wrapMode: Text.WrapAnywhere
                        color: Theme.textDim
                    }
                    Button {
                        text: Theme.tr("Trust this certificate")
                        onClicked: Core.call("server.trustCertificate", { fingerprint: parent.fp })
                    }
                }
                // The server's web app, checked from this app against the signed
                // manifest (the browser version cannot check itself).
                RowLayout {
                    readonly property var wc: Core.state.webCheck || ({})
                    readonly property string reported: Core.state.serverWebIntegrity || ""
                    visible: Core.platform !== "web" && page.server === "connected" && reported !== "none" && reported !== ""
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Icon {
                        name: parent.wc.state === "verified" ? "verified_user" : parent.wc.state === "failed" ? "warning" : "info"
                        size: 22
                        color: parent.wc.state === "failed" ? Theme.danger : parent.wc.state === "verified" ? Theme.accent : Theme.textDim
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: parent.wc.state === "failed" ? Theme.danger : Theme.textDim
                        text: parent.wc.state === "verified" ? Theme.tr("Web app on the server: unchanged official release") + " " + parent.wc.detail
                            : parent.wc.state === "unsigned" ? Theme.tr("Web app on the server: files match its manifest, but this app cannot check the release signature (unofficial build)")
                            : parent.wc.state === "failed" ? Theme.tr("Warning: the web app on the server was modified.") + " " + parent.wc.detail
                            : parent.wc.state === "running" ? Theme.tr("Checking the web app…") + " " + parent.wc.detail
                            : Theme.tr("Web app on the server: not checked yet")
                    }
                    Button {
                        text: Theme.tr("Check")
                        flat: true
                        enabled: parent.wc.state !== "running"
                        onClicked: Core.call("server.verifyWeb")
                    }
                }
                Label {
                    textFormat: Text.PlainText
                    visible: page.insecureRemote(url.text)
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    text: Theme.tr("Warning: this address is outside your home network and the connection is not encrypted. Use https:// or a VPN.")
                    wrapMode: Text.WordWrap
                    color: Theme.danger
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    text: Theme.tr("Outside your home network, use https:// or a VPN.")
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                }
            }
            // ---- this device among the server's devices ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("server", ["Allow remote control", "Device name", "Name", "This device", "Your other devices may control playback here."])
                SectionLabel { text: Theme.tr("This device"); visible: page.server === "connected" }
                RowLayout {
                    visible: page.server === "connected"
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Label { textFormat: Text.PlainText; text: Theme.tr("Name"); color: Theme.text; Layout.preferredWidth: 120 }
                    InputField {
                        id: deviceName
                        Layout.fillWidth: true
                        placeholderText: Core.state.deviceName || ""
                        text: page.settings.deviceName || ""
                        maximumLength: 48
                        onEditingFinished: if (text !== (page.settings.deviceName || "")) Core.call("settings.set", { key: "deviceName", value: text.trim() })
                        Accessible.name: Theme.tr("Device name")
                    }
                }
                SwitchRow {
                    visible: page.server === "connected"
                    Layout.fillWidth: true
                    title: Theme.tr("Allow remote control")
                    detail: Theme.tr("Your other devices may control playback here.")
                    on: page.settings.remoteControl !== false
                    onSwitched: (on) => Core.call("settings.set", { key: "remoteControl", value: on })
                }
            }
            // ---- stream quality (the server converts to Opus) ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.server === "connected" && Core.platform !== "web"
                         && page.shows("server", ["Streaming quality", "Original", "On Wi-Fi", "On mobile data", "Smaller streams save data. Offline copies stay originals.", "Now on mobile data"])
                SectionLabel { text: Theme.tr("Streaming quality") }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textDim
                    text: Theme.tr("Smaller streams save data. Offline copies stay originals.")
                          + (Core.state.metered ? " " + Theme.tr("Now on mobile data") + "." : "")
                }
                Repeater {
                    model: [
                        { key: "streamKbps", label: Theme.tr("On Wi-Fi") },
                        { key: "streamKbpsMetered", label: Theme.tr("On mobile data") }
                    ]
                    RowLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: modelData.label; color: Theme.text }
                        Dropdown {
                            readonly property var rates: [0, 256, 160, 128, 96, 64, 48]
                            model: rates.map(r => r === 0 ? Theme.tr("Original") : r + " kbit/s")
                            currentIndex: Math.max(0, rates.indexOf(page.settings[modelData.key] || 0))
                            onActivated: (i) => Core.call("settings.set", { key: modelData.key, value: rates[i] })
                            Accessible.name: Theme.tr("Streaming quality") + " " + modelData.label
                        }
                    }
                }
            }
            // ---- account ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("server", ["Account", "Appearance and playback options follow your account (end-to-end encrypted).", "Encrypted files", "Unlock or create account", "Unlocked as", "Store and share end-to-end encrypted files; the server cannot read them."])
                SectionLabel { text: Theme.tr("Account"); visible: page.server === "connected" || page.server === "login" }
                ColumnLayout {
                    readonly property var v: Core.state.vault || ({})
                    visible: page.server === "connected" || page.server === "login"
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 6
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: parent.v.state === "ready" ? Theme.tr("Unlocked as") + " " + parent.v.user
                            : Theme.tr("Store and share end-to-end encrypted files; the server cannot read them.")
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: parent.v.state === "ready"
                        wrapMode: Text.WordWrap
                        color: Theme.textFaint
                        font.pixelSize: 12
                        text: Theme.tr("Appearance and playback options follow your account (end-to-end encrypted).")
                    }
                    Button {
                        text: parent.v.state === "ready" ? Theme.tr("Encrypted files") : Theme.tr("Unlock or create account")
                        icon.source: Theme.icon(parent.v.state === "ready" ? "lock" : "lock_open")
                        onClicked: page.openVault()
                    }
                }
            }
            // ---- songs from server extensions ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("server", ["Ask", "Library on the server", "My encrypted files", "Save in", "Songs from"])
                SectionLabel {
                    text: Theme.tr("Songs from") + " " + ((((Core.state.extensions || {}).list || [])[0] || {}).name || "")
                    visible: (((Core.state.extensions || {}).list) || []).length > 0
                }
                RowLayout {
                    visible: (((Core.state.extensions || {}).list) || []).length > 0
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Label { textFormat: Text.PlainText; text: Theme.tr("Save in"); color: Theme.text; Layout.preferredWidth: 120 }
                    Dropdown {
                        Accessible.name: Theme.tr("Save in")
                        Layout.fillWidth: true
                        readonly property var choices: ["", "library", "vault"]
                        model: [Theme.tr("Ask"), Theme.tr("Library on the server"), Theme.tr("My encrypted files")]
                        currentIndex: Math.max(0, choices.indexOf(page.settings.extensionTarget || ""))
                        onActivated: (i) => Core.call("settings.set", { key: "extensionTarget", value: choices[i] })
                    }
                }
            }
            // ---- AirPlay ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("receive", ["AirPlay is not available", "DLNA has no password.", "Forget", "New devices are asked for before they play here.", "Without a password, anyone in your network can play here.", "Password (optional)", "Password set (enter a new one to change it)", "Ready, visible as", "Receive AirPlay", "Receive DLNA / UPnP", "Receiving from", "Remove password", "Set password", "Speaker for other devices", "Speaker name", "Starting…", "This device appears as a speaker on iPhone, iPad and Mac.", "Windows, TVs and apps like BubbleUPnP can send music here.", "not available", "playing what a device sent"])
                SectionLabel { text: Theme.tr("Speaker for other devices"); visible: Core.platform !== "web" }
                SwitchRow {
                    visible: Core.platform !== "web"
                    Layout.fillWidth: true
                    title: Theme.tr("Receive AirPlay")
                    detail: Theme.tr("This device appears as a speaker on iPhone, iPad and Mac.")
                    on: page.settings.airplay === true
                    onSwitched: (on) => Core.call("settings.set", { key: "airplay", value: on })
                }
                SwitchRow {
                    visible: Core.platform !== "web" && Core.platform !== "android" && page.settings.airplay === true
                    Layout.fillWidth: true
                    title: Theme.tr("AirPlay 2 (experimental)")
                    detail: Theme.tr("For newer iPhones and Macs. If they cannot connect, turn it off.")
                    on: page.settings.airplay2 === true
                    onSwitched: (on) => Core.call("settings.set", { key: "airplay2", value: on })
                }
                SwitchRow {
                    visible: Core.platform !== "web"
                    Layout.fillWidth: true
                    title: Theme.tr("Receive DLNA / UPnP")
                    detail: Theme.tr("Windows, TVs and apps like BubbleUPnP can send music here.")
                    on: page.settings.dlnaReceiver === true
                    onSwitched: (on) => Core.call("settings.set", { key: "dlnaReceiver", value: on })
                }
                ColumnLayout {
                    readonly property var ap: Core.state.airplay || ({})
                    readonly property var dl: Core.state.dlnaReceiver || ({})
                    readonly property bool airplayOn: page.settings.airplay === true
                    readonly property bool dlnaOn: page.settings.dlnaReceiver === true
                    visible: Core.platform !== "web" && (airplayOn || dlnaOn)
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: parent.dlnaOn
                        text: "DLNA: " + (parent.dl.error ? Theme.tr("not available") + ": " + parent.dl.error
                            : parent.dl.active ? Theme.tr("playing what a device sent")
                            : parent.dl.running ? Theme.tr("Ready, visible as") + " „" + parent.dl.name + "“"
                            : Theme.tr("Starting…"))
                        color: parent.dl.error ? Theme.danger : Theme.textDim
                        wrapMode: Text.WordWrap
                    }
                    RowLayout {
                        visible: parent.dlnaOn
                        Layout.fillWidth: true
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: Theme.textDim
                            font.pixelSize: 12
                            text: (parent.parent.dl.allowed || []).length > 0
                                  ? Theme.tr("Allowed devices: %1").arg(parent.parent.dl.allowed.join(", "))
                                  : Theme.tr("New devices are asked for before they play here.")
                        }
                        Button {
                            visible: (parent.parent.dl.allowed || []).length > 0
                            text: Theme.tr("Forget")
                            flat: true
                            onClicked: Core.call("dlna.forget")
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: parent.airplayOn
                        text: parent.ap.error ? Theme.tr("AirPlay is not available") + ": " + parent.ap.error
                            : parent.ap.active ? Theme.tr("Receiving from") + " " + parent.ap.client
                            : parent.ap.running ? Theme.tr("Ready, visible as") + " „" + parent.ap.name + "“"
                            : Theme.tr("Starting…")
                        color: parent.ap.error ? Theme.danger : Theme.textDim
                        wrapMode: Text.WordWrap
                    }
                    InputField {
                        Accessible.name: placeholderText
                        id: airplayName
                        Layout.fillWidth: true
                        placeholderText: Theme.tr("Speaker name") + " (Beatlumen)"
                        text: page.settings.airplayName || ""
                        maximumLength: 48
                        onEditingFinished: if (text !== (page.settings.airplayName || "")) Core.call("settings.set", { key: "airplayName", value: text })
                    }
                    InputField {
                        Accessible.name: placeholderText
                        id: airplayPassword
                        visible: parent.airplayOn
                        Layout.fillWidth: true
                        placeholderText: page.settings.airplayPasswordSet ? Theme.tr("Password set (enter a new one to change it)") : Theme.tr("Password (optional)")
                        echoMode: TextInput.Password
                        maximumLength: 64
                        inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText | Qt.ImhSensitiveData
                        onAccepted: if (text.length > 0) { Core.call("settings.set", { key: "airplayPassword", value: text }); text = "" }
                    }
                    Flow {
                        visible: parent.airplayOn
                        Layout.fillWidth: true
                        spacing: 4
                        Button {
                            visible: airplayPassword.text.length > 0
                            text: Theme.tr("Set password")
                            onClicked: { Core.call("settings.set", { key: "airplayPassword", value: airplayPassword.text }); airplayPassword.text = "" }
                        }
                        Button {
                            visible: page.settings.airplayPasswordSet === true
                            flat: true
                            text: Theme.tr("Remove password")
                            onClicked: Core.call("settings.set", { key: "airplayPassword", value: "" })
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: Theme.tr("Without a password, anyone in your network can play here.")
                              + (parent.dlnaOn ? " " + Theme.tr("DLNA has no password.") : "")
                        color: Theme.textFaint
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }
                }
            }
            // ---- what was played ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("library", ["Listening history", "Also keep it on the server", "Delete listening history"])
                SectionLabel { text: Theme.tr("Listening history") }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textDim
                    text: Theme.tr("Kept on this device for the start page and statistics.")
                }
                SwitchRow {
                    Layout.fillWidth: true
                    visible: page.server === "connected"
                    title: Theme.tr("Also keep it on the server")
                    detail: Theme.tr("Also for recommendations across devices. The server's admin could read it.")
                    info: Theme.tr("Only the server's songs, and only from now on: local and encrypted files never leave this device.")
                    on: page.settings.historySync === true
                    onSwitched: (v) => Core.call("settings.set", { key: "historySync", value: v })
                }
                Button {
                    Layout.leftMargin: 16
                    flat: true
                    text: Theme.tr("Delete listening history")
                    icon.source: Theme.icon("delete")
                    onClicked: historyDelete.open()
                }
                AppDialog {
                    id: historyDelete
                    readonly property bool withServer: page.server === "connected" && page.settings.historySync === true
                    anchors.centerIn: Overlay.overlay
                    width: Math.min(420, page.width - 32)
                    title: Theme.tr("Delete listening history")
                    standardButtons: Dialog.Cancel | Dialog.Ok
                    onAccepted: {
                        Core.call("history.clear", { server: page.server === "connected" })
                        Core.call("toast", { text: Theme.tr("Listening history deleted") })
                    }
                    Label {
                        textFormat: Text.PlainText
                        width: parent.width
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: page.server === "connected" ? Theme.tr("Delete what you played on this device and on the server?")
                                                          : Theme.tr("Delete what you played on this device?")
                    }
                }
            }

            // ---- offline copies of server songs ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("library", ["No limit", "Offline", "Remove all offline copies", "Remove offline copy", "Saving", "Songs kept here play without a connection.", "Storage limit", "Unknown Album", "of", "songs"])
                SectionLabel { text: Theme.tr("Offline"); visible: offlineBox.visible }
                ColumnLayout {
                    id: offlineBox
                    readonly property var off: Core.state.offline || ({})
                    visible: off.available === true && (off.count > 0 || (off.saving || []).length > 0)
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("Songs kept here play without a connection.")
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: (offlineBox.off.count || 0) + " " + Theme.tr("songs") + " · " + ((offlineBox.off.bytes || 0) / 1e6).toFixed(1) + " MB"
                              + (offlineBox.off.limit > 0 ? " " + Theme.tr("of") + " " + (offlineBox.off.limit / 1073741824).toFixed(0) + " GB" : "")
                    }
                    ProgressBar {
                        visible: offlineBox.off.limit > 0
                        Layout.fillWidth: true
                        value: Math.min(1, (offlineBox.off.bytes || 0) / Math.max(1, offlineBox.off.limit || 1))
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Storage limit"); color: Theme.text }
                        Dropdown {
                            Accessible.name: Theme.tr("Storage limit")
                            readonly property var sizes: [0, 1024, 2048, 5120, 10240, 20480, 51200]
                            model: sizes.map(s => s === 0 ? Theme.tr("No limit") : (s / 1024) + " GB")
                            currentIndex: Math.max(0, sizes.indexOf(page.settings.offlineLimitMb || 0))
                            onActivated: (i) => Core.call("settings.set", { key: "offlineLimitMb", value: sizes[i] })
                        }
                    }
                    // What takes the space, by album.
                    Repeater {
                        model: {
                            offlineBox.off.count // reload when it changes
                            return (Core.call("offline.overview") || []).slice(0, 30)
                        }
                        RowLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; elide: Text.ElideRight; color: Theme.text; text: modelData.album || Theme.tr("Unknown Album") }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    color: Theme.textDim
                                    font.pixelSize: 12
                                    text: [modelData.artist, modelData.songs + " " + Theme.tr("songs"), (modelData.bytes / 1e6).toFixed(1) + " MB"].filter(x => x).join(" · ")
                                }
                            }
                            IconButton { iconName: "delete"; tip: Theme.tr("Remove offline copy"); onClicked: Core.call("offline.remove", { ids: modelData.ids }) }
                        }
                    }
                    Repeater {
                        model: offlineBox.off.saving || []
                        ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: 2
                            Label { textFormat: Text.PlainText; Layout.fillWidth: true; elide: Text.ElideRight; color: Theme.textDim; text: Theme.tr("Saving") + ": " + modelData.title }
                            ProgressBar { Layout.fillWidth: true; value: Math.min(1, modelData.progress || 0) }
                        }
                    }
                    Button {
                        visible: (offlineBox.off.count || 0) > 0
                        flat: true
                        text: Theme.tr("Remove all offline copies")
                        icon.source: Theme.icon("delete")
                        onClicked: Core.call("offline.remove", { all: true })
                    }
                }
            }
            // ---- other apps (Android) ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("receive", ["Allow control of other apps", "For titles, covers and controls, Android needs notification access for Beatlumen. Notifications are not read.", "Other apps", "Visuals for music in other apps. Only the sound is used.", "Visuals for what the computer plays. Only the sound is used.", "Stop", "Visualize other apps", "Visualizing and controlling", "Visualizing other apps; controlling them needs notification access", "Visualizing other apps; no playing app found to control", "not available"])
                SectionLabel { text: Theme.tr("Other apps"); visible: (Core.state.capture || {}).available === true }
                ColumnLayout {
                    readonly property var cap: Core.state.capture || ({})
                    visible: cap.available === true
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    spacing: 8
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 4
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: Core.platform === "android"
                                  ? Theme.tr("Visuals for music in other apps. Only the sound is used.")
                                  : Theme.tr("Visuals for what the computer plays. Only the sound is used.")
                            color: Theme.textDim
                            wrapMode: Text.WordWrap
                        }
                        InfoButton {
                            visible: Core.platform === "android"
                            info: Theme.tr("Android asks for permission to record the screen each time. Some apps do not allow recording: then only their title and controls are shown.")
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: parent.cap.running === true || !!parent.cap.error
                        text: parent.cap.running ? (parent.cap.app ? Theme.tr("Visualizing and controlling") + " " + parent.cap.app
                                                                   : parent.cap.access ? Theme.tr("Visualizing other apps; no playing app found to control")
                                                                                       : Theme.tr("Visualizing other apps; controlling them needs notification access"))
                                                 : Theme.tr("not available") + ": " + parent.cap.error
                        color: parent.cap.running ? Theme.textDim : Theme.danger
                        wrapMode: Text.WordWrap
                    }
                    Flow {
                        Layout.fillWidth: true
                        spacing: 8
                        AccentButton {
                            text: parent.parent.cap.running || parent.parent.cap.requested ? Theme.tr("Stop") : Theme.tr("Visualize other apps")
                            highlighted: !(parent.parent.cap.running || parent.parent.cap.requested)
                            icon.source: Theme.icon("graphic_eq")
                            onClicked: Core.call(parent.parent.cap.running || parent.parent.cap.requested ? "capture.stop" : "capture.start")
                        }
                        Button {
                            visible: Core.platform === "android" && parent.parent.cap.access !== true
                            text: Theme.tr("Allow control of other apps")
                            flat: true
                            icon.source: Theme.icon("tune")
                            onClicked: Core.call("capture.sessionAccess")
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        visible: Core.platform === "android" && parent.cap.access !== true
                        text: Theme.tr("For titles, covers and controls, Android needs notification access for Beatlumen. Notifications are not read.")
                        color: Theme.textFaint
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }
                }
            }
            // ---- performance ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("look", ["Performance", "Render resolution"])
                SectionLabel { text: Theme.tr("Performance") }
                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    Label { textFormat: Text.PlainText; text: Theme.tr("Render resolution"); color: Theme.text }
                    Slider {
                        Accessible.name: Theme.tr("Render resolution")
                        Layout.fillWidth: true
                        from: 0.25; to: 1; stepSize: 0.05
                        value: page.settings.renderScale || 1
                        onMoved: Core.call("settings.set", { key: "renderScale", value: value })
                    }
                    Label { textFormat: Text.PlainText; text: Math.round((page.settings.renderScale || 1) * 100) + " %"; color: Theme.textDim }
                }
            }
            // ---- microphone test (a developer check, only found through the search) ----
            ColumnLayout {
                id: micTest
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                readonly property var mic: Core.state.mic || ({})
                visible: mic.available === true && settingsSearch.text.trim().length >= 3
                         && page.shows("", ["Microphone test", "Microphones"])
                property var devices: []
                property var streams: []
                // Streams this check opened (closed again when it hides).
                property var opened: []
                function refresh() {
                    const r = Core.call("mic.list") || {}
                    devices = r.devices || []
                    streams = r.streams || []
                }
                function streamOf(id) {
                    return streams.find(s => s.device === id && opened.indexOf(s.stream) >= 0)
                }
                function toggle(id) {
                    const s = streamOf(id)
                    if (s) {
                        Core.call("mic.close", { stream: s.stream })
                        opened = opened.filter(x => x !== s.stream)
                    } else {
                        const r = Core.call("mic.open", { device: id }) || {}
                        if (r.stream)
                            opened = opened.concat([r.stream])
                    }
                    refresh()
                }
                function shown() {
                    if (visible) {
                        refresh()
                        micListAgain.restart()
                    } else {
                        for (const s of opened)
                            Core.call("mic.close", { stream: s })
                        opened = []
                    }
                }
                onVisibleChanged: shown()
                Component.onCompleted: if (visible) shown()
                Component.onDestruction: { for (const s of opened) Core.call("mic.close", { stream: s }) }
                // The platform lists its inputs in the background.
                Timer { id: micListAgain; interval: 600; onTriggered: micTest.refresh() }
                Timer {
                    interval: 50
                    repeat: true
                    running: micTest.visible && micTest.opened.length > 0
                    onTriggered: micTest.streams = (Core.call("mic.levels") || {}).streams || []
                }
                SectionLabel { text: Theme.tr("Microphone test") }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                    text: Theme.tr("Check input levels. Nothing is saved.")
                }
                Repeater {
                    model: micTest.devices
                    ColumnLayout {
                        id: micRow
                        required property var modelData
                        readonly property var stream: micTest.streamOf(modelData.id)
                        // -60..0 dB onto 0..1.
                        function frac(db) { return Math.max(0, Math.min(1, (db + 60) / 60)) }
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        spacing: 4
                        RowLayout {
                            Layout.fillWidth: true
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    color: Theme.text
                                    text: micRow.modelData.name
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    color: micRow.stream && micRow.stream.error ? Theme.danger : Theme.textDim
                                    font.pixelSize: 12
                                    text: micRow.stream && micRow.stream.error ? micRow.stream.error
                                        : [micRow.modelData.channels + " ch", micRow.modelData.rate ? micRow.modelData.rate + " Hz" : "",
                                           micRow.modelData.default ? Theme.tr("default") : "",
                                           micRow.stream ? Math.round(micRow.stream.levelDb) + " dB · " + Theme.tr("peak") + " " + Math.round(micRow.stream.peakDb) + " dB" : ""]
                                          .filter(x => x).join(" · ")
                                }
                            }
                            Button {
                                flat: true
                                text: micRow.stream ? Theme.tr("Close") : Theme.tr("Open")
                                icon.source: Theme.icon(micRow.stream ? "close" : "graphic_eq")
                                onClicked: micTest.toggle(micRow.modelData.id)
                            }
                        }
                        Item {
                            visible: !!micRow.stream
                            Layout.fillWidth: true
                            Layout.preferredHeight: 8
                            Rectangle { anchors.fill: parent; radius: 4; color: Theme.surfaceVariant }
                            Rectangle {
                                height: parent.height
                                radius: 4
                                color: micRow.stream && micRow.stream.peak > 0.98 ? Theme.danger : Theme.accent
                                width: parent.width * micRow.frac(micRow.stream ? micRow.stream.levelDb : -100)
                            }
                            Rectangle {
                                width: 2
                                height: parent.height
                                color: Theme.text
                                x: (parent.width - 2) * micRow.frac(micRow.stream ? micRow.stream.peakDb : -100)
                            }
                        }
                    }
                }
                Label {
                    visible: micTest.devices.length === 0
                    textFormat: Text.PlainText
                    Layout.leftMargin: 16
                    color: Theme.textDim
                    text: Theme.tr("No microphones found")
                }
            }
            // ---- about ----
            ColumnLayout {
                objectName: "section"
                Layout.fillWidth: true
                spacing: 6
                visible: page.shows("about", ["About", "Copy link", "Source code"])
                SectionLabel { text: Theme.tr("About") }
                RowLayout {
                    Layout.leftMargin: 16
                    spacing: 16
                    Image { source: "qrc:/qt/qml/Beatlumen/assets/icon.png"; sourceSize: Qt.size(64, 64); Layout.preferredWidth: 64; Layout.preferredHeight: 64 }
                    ColumnLayout {
                        spacing: 2
                        Label { textFormat: Text.PlainText; text: "Beatlumen " + Core.version; font.pixelSize: 18; font.weight: Font.Bold; color: Theme.text }
                        Label { textFormat: Text.PlainText; text: "GNU AGPL v3.0 or later"; color: Theme.textDim }
                    }
                }
                // AGPL: users get the source code of the version they use.
                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        text: Theme.tr("Source code") + ": " + (Core.state.sourceUrl || "")
                        color: Theme.textDim
                        wrapMode: Text.WrapAnywhere
                    }
                    Button {
                        text: Theme.tr("Copy link")
                        flat: true
                        icon.source: Theme.icon("content_copy")
                        onClicked: Core.copyText(Core.state.sourceUrl || "")
                    }
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                    text: "Qt 6 (LGPL v3) · Inter font (SIL OFL 1.1) · Material Symbols (Apache 2.0) · effect shaders after midori and three.js (MIT) · webgl-noise (MIT) · minimp3 (CC0) · libopus, libvorbis (BSD) · Apple ALAC (Apache 2.0) · font-rs (Apache 2.0)"
                }
            }
            Item { Layout.preferredHeight: 24 }
        }
    }
}
