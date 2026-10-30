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

    ScrollView {

        // Vertical only: an (invisible) horizontal bar would catch taps

        // along the bottom edge.

        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        id: sv
        anchors.fill: parent
        clip: true
        contentWidth: availableWidth
        ColumnLayout {
            width: sv.availableWidth
            spacing: 8
            PageHeader { Layout.fillWidth: true; title: Theme.tr("Settings") }

            // ---- appearance -------------------------------------------------
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
                            width: 40; height: 40
                            readonly property string tip: modelData === "" ? "accent:system" : modelData
                            Accessible.name: modelData === "" ? Theme.tr("System") : modelData
                            onClicked: Core.call("settings.set", { key: "ui.accent", value: modelData })
                            background: Rectangle {
                                radius: 20
                                color: modelData === "" ? Core.systemAccent : modelData
                                border.width: (page.ui.accent || "") === modelData ? 3 : 0
                                border.color: Theme.text
                                Icon {
                                    anchors.centerIn: parent
                                    readonly property bool selected: (page.ui.accent || "") === modelData
                                    visible: modelData === "" || selected
                                    name: modelData === "" ? "hdr_auto" : "check"
                                    color: Theme.onColor(parent.color)
                                    size: 20
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
                ComboBox {
                    model: [{ v: "de", l: "Deutsch" }, { v: "en", l: "English" }]
                    textRole: "l"
                    valueRole: "v"
                    currentIndex: Theme.language === "de" ? 0 : 1
                    onActivated: (i) => Core.call("settings.set", { key: "language", value: i === 0 ? "de" : "en" })
                }
            }

            // ---- playback ---------------------------------------------------
            SectionLabel { text: Theme.tr("Playback") }
            SwitchRow {
                Layout.fillWidth: true
                title: Theme.tr("Normalize volume")
                detail: Theme.tr("Plays all songs at a similar loudness. Each song is measured once when it is first played.")
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
                    Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Crossfade"); color: Theme.text; font.pixelSize: 15 }
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
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: Theme.tr("The next song fades in while the current one fades out.")
                    color: Theme.textDim
                    font.pixelSize: 13
                    wrapMode: Text.WordWrap
                }
            }
            SwitchRow {
                Layout.fillWidth: true
                title: Theme.tr("Skip silence")
                detail: Theme.tr("Moves on to the next song as soon as the current one has ended, without silent gaps.")
                on: page.settings.skipSilence !== false
                onSwitched: (on) => Core.call("settings.set", { key: "skipSilence", value: on })
            }

            // ---- library ----------------------------------------------------
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

            // ---- music sources (without a server) ---------------------------
            SectionLabel { text: Theme.tr("Music sources"); visible: page.sources.available === true }
            Label {
                visible: page.sources.available === true && (page.sources.list || []).length === 0
                textFormat: Text.PlainText
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                wrapMode: Text.Wrap
                color: Theme.textDim
                font.pixelSize: 13
                text: Theme.tr("Play the music of a Subsonic, Navidrome or Jellyfin server, a WebDAV or Nextcloud folder or a DLNA media server directly on this device.")
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

            // ---- server -----------------------------------------------------
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
                TextField {
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
                text: Theme.tr("Only connections to this server are made. Outside your home network, use an https:// address or a VPN.")
                wrapMode: Text.WordWrap
                color: Theme.textFaint
                font.pixelSize: 12
            }

            // ---- this device among the server's devices -----------------------
            SectionLabel { text: Theme.tr("This device"); visible: page.server === "connected" }
            RowLayout {
                visible: page.server === "connected"
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                spacing: 8
                Label { textFormat: Text.PlainText; text: Theme.tr("Name"); color: Theme.text; Layout.preferredWidth: 120 }
                TextField {
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
                detail: Theme.tr("Your other devices on this server may control the playback here and move music to this device.")
                on: page.settings.remoteControl !== false
                onSwitched: (on) => Core.call("settings.set", { key: "remoteControl", value: on })
            }

            // ---- account ----------------------------------------------------
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
                        : Theme.tr("With an account, you store end-to-end encrypted files on the server and share them; the server cannot read them.")
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    visible: parent.v.state === "ready"
                    wrapMode: Text.WordWrap
                    color: Theme.textFaint
                    font.pixelSize: 12
                    text: Theme.tr("Appearance, styles and playback options follow your account to your other devices (end-to-end encrypted).")
                }
                Button {
                    text: parent.v.state === "ready" ? Theme.tr("Encrypted files") : Theme.tr("Unlock or create account")
                    icon.source: Theme.icon(parent.v.state === "ready" ? "lock" : "lock_open")
                    onClicked: page.openVault()
                }
            }

            // ---- songs from server extensions --------------------------------
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
                ComboBox {
                    Layout.fillWidth: true
                    readonly property var choices: ["", "library", "vault"]
                    model: [Theme.tr("Ask"), Theme.tr("Library on the server"), Theme.tr("My encrypted files")]
                    currentIndex: Math.max(0, choices.indexOf(page.settings.extensionTarget || ""))
                    onActivated: (i) => Core.call("settings.set", { key: "extensionTarget", value: choices[i] })
                }
            }

            // ---- performance ----------------------------------------------
            // ---- AirPlay ----------------------------------------------------
            SectionLabel { text: Theme.tr("Speaker for other devices"); visible: Core.platform !== "web" }
            SwitchRow {
                visible: Core.platform !== "web"
                Layout.fillWidth: true
                title: Theme.tr("Receive AirPlay")
                detail: Theme.tr("This device appears as a speaker on iPhone, iPad and Mac; what they play is shown with visuals here.")
                on: page.settings.airplay === true
                onSwitched: (on) => Core.call("settings.set", { key: "airplay", value: on })
            }
            SwitchRow {
                visible: Core.platform !== "web"
                Layout.fillWidth: true
                title: Theme.tr("Receive DLNA / UPnP")
                detail: Theme.tr("Windows (\"Play to\"), TVs and apps like BubbleUPnP can send music to this device.")
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
                        font.pixelSize: 13
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
                TextField {
                    id: airplayName
                    Layout.fillWidth: true
                    placeholderText: Theme.tr("Speaker name") + " (Beatlumen)"
                    text: page.settings.airplayName || ""
                    maximumLength: 48
                    onEditingFinished: if (text !== (page.settings.airplayName || "")) Core.call("settings.set", { key: "airplayName", value: text })
                }
                TextField {
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
                    text: Theme.tr("Only reachable in the local network. Without a password anyone in this network can play music on this device.")
                          + (parent.dlnaOn ? " " + Theme.tr("DLNA has no password.") : "")
                    color: Theme.textFaint
                    font.pixelSize: 13
                    wrapMode: Text.WordWrap
                }
            }

            // ---- offline copies of server songs -------------------------------
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
                    text: Theme.tr("Songs kept on this device play without a connection to the server, with their song text and visuals.")
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: (offlineBox.off.count || 0) + " " + Theme.tr("songs") + " · " + ((offlineBox.off.bytes || 0) / 1e6).toFixed(1) + " MB"
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

            // ---- other apps (Android) ---------------------------------------
            SectionLabel { text: Theme.tr("Other apps"); visible: (Core.state.capture || {}).available === true }
            ColumnLayout {
                readonly property var cap: Core.state.capture || ({})
                visible: cap.available === true
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                spacing: 8
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: Core.platform === "android"
                          ? Theme.tr("Shows visuals for music that other apps play (Spotify, YouTube, radio apps, …) and controls them from here. Android asks for permission to record the screen each time; only the sound is used. Some apps do not allow recording, then only their title and controls are shown.")
                          : Theme.tr("Shows visuals for what the computer plays (Spotify, the browser, any player) and controls the playing app from here. Only the sound is used.")
                    color: Theme.textDim
                    wrapMode: Text.WordWrap
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
                        visible: parent.parent.cap.access !== true
                        text: Theme.tr("Allow control of other apps")
                        flat: true
                        icon.source: Theme.icon("tune")
                        onClicked: Core.call("capture.sessionAccess")
                    }
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    visible: parent.cap.access !== true
                    text: Theme.tr("For titles, covers and controls, Android needs notification access for Beatlumen. Notifications are not read.")
                    color: Theme.textFaint
                    font.pixelSize: 13
                    wrapMode: Text.WordWrap
                }
            }

            SectionLabel { text: Theme.tr("Performance") }
            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                Label { textFormat: Text.PlainText; text: Theme.tr("Render resolution"); color: Theme.text }
                Slider {
                    Layout.fillWidth: true
                    from: 0.25; to: 1; stepSize: 0.05
                    value: page.settings.renderScale || 1
                    onMoved: Core.call("settings.set", { key: "renderScale", value: value })
                }
                Label { textFormat: Text.PlainText; text: Math.round((page.settings.renderScale || 1) * 100) + " %"; color: Theme.textDim }
            }

            // ---- about --------------------------------------------------------
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
            Item { Layout.preferredHeight: 24 }
        }
    }
}
