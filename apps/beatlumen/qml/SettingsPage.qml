import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Settings, grouped by topic.
Item {
    id: page
    signal addFolder()
    signal login()
    readonly property var settings: Core.state.settings || ({})
    readonly property var ui: settings.ui || ({})
    readonly property string server: Core.state.server || "disabled"

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

            // ---- performance ----------------------------------------------
            // ---- AirPlay ----------------------------------------------------
            SectionLabel { text: "AirPlay"; visible: Core.platform !== "web" }
            SwitchRow {
                visible: Core.platform !== "web"
                Layout.fillWidth: true
                title: Theme.tr("Receive AirPlay")
                detail: Theme.tr("This device appears as a speaker on iPhone, iPad and Mac; what they play is shown with visuals here.")
                on: page.settings.airplay === true
                onSwitched: (on) => Core.call("settings.set", { key: "airplay", value: on })
            }
            ColumnLayout {
                readonly property var ap: Core.state.airplay || ({})
                visible: Core.platform !== "web" && page.settings.airplay === true
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                spacing: 8
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
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
                    Layout.fillWidth: true
                    placeholderText: page.settings.airplayPasswordSet ? Theme.tr("Password set (enter a new one to change it)") : Theme.tr("Password (optional)")
                    echoMode: TextInput.Password
                    maximumLength: 64
                    inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText | Qt.ImhSensitiveData
                    onAccepted: if (text.length > 0) { Core.call("settings.set", { key: "airplayPassword", value: text }); text = "" }
                }
                Flow {
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
