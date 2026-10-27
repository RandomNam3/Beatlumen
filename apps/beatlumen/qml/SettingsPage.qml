import QtQuick
import QtQuick.Controls
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

    ScrollView {
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
            RowLayout {
                Layout.leftMargin: 16
                spacing: 8
                Label { text: Theme.tr("Theme"); color: Theme.text; Layout.preferredWidth: 120 }
                Repeater {
                    model: [{ v: "system", l: Theme.tr("System") }, { v: "light", l: Theme.tr("Light") }, { v: "dark", l: Theme.tr("Dark") }]
                    delegate: Button {
                        text: modelData.l
                        checkable: true
                        flat: !checked
                        highlighted: checked
                        checked: (page.ui.theme || "system") === modelData.v
                        onClicked: Core.call("settings.set", { key: "ui.theme", value: modelData.v })
                    }
                }
            }
            ColumnLayout {
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                spacing: 8
                Label { text: Theme.tr("Accent colour"); color: Theme.text }
                Flow {
                    Layout.fillWidth: true
                    spacing: 10
                    Repeater {
                        model: ["", "#e91e63", "#ff5722", "#ff9800", "#ffc107", "#8bc34a", "#00bfa5", "#00b0ff", "#3d5afe", "#7c4dff", "#d500f9"]
                        delegate: AbstractButton {
                            width: 40; height: 40
                            Accessible.name: modelData === "" ? Theme.tr("System") : modelData
                            onClicked: Core.call("settings.set", { key: "ui.accent", value: modelData })
                            background: Rectangle {
                                radius: 20
                                color: modelData === "" ? Core.systemAccent : modelData
                                border.width: (page.ui.accent || "") === modelData ? 3 : 0
                                border.color: Theme.text
                                Icon { anchors.centerIn: parent; visible: modelData === ""; name: "hdr_auto"; color: "white"; size: 20 }
                            }
                        }
                    }
                }
            }
            RowLayout {
                Layout.leftMargin: 16
                spacing: 8
                Label { text: Theme.tr("Language"); color: Theme.text; Layout.preferredWidth: 120 }
                ComboBox {
                    model: [{ v: "de", l: "Deutsch" }, { v: "en", l: "English" }]
                    textRole: "l"
                    valueRole: "v"
                    currentIndex: Theme.language === "de" ? 0 : 1
                    onActivated: (i) => Core.call("settings.set", { key: "language", value: i === 0 ? "de" : "en" })
                }
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
                        Label { Layout.fillWidth: true; text: modelData; elide: Text.ElideMiddle; color: Theme.text }
                        IconButton { iconName: "close"; tip: Theme.tr("Remove"); onClicked: Core.call("library.removeFolder", { path: modelData }) }
                    }
                }
            }
            RowLayout {
                visible: Core.platform !== "web"
                Layout.leftMargin: 8
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
                Button { text: Theme.tr("Connect"); highlighted: true; onClicked: Core.call("server.connect", { url: url.text }) }
            }
            RowLayout {
                Layout.leftMargin: 8
                Button { visible: page.server === "login"; text: Theme.tr("Log in"); icon.source: Theme.icon("login"); onClicked: page.login() }
                Button { visible: page.server === "connected" && page.settings.serverToken !== ""; text: Theme.tr("Log out"); flat: true; icon.source: Theme.icon("logout"); onClicked: Core.call("server.logout") }
                Button { visible: Core.platform !== "web" && page.server !== "disabled"; text: Theme.tr("Disconnect"); flat: true; icon.source: Theme.icon("cloud_off"); onClicked: Core.call("server.disconnect") }
            }
            Label {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                text: Theme.tr("Only connections to this server are made. Use a VPN or reverse proxy with HTTPS outside your home network.")
                wrapMode: Text.WordWrap
                color: Theme.textFaint
                font.pixelSize: 12
            }

            // ---- performance ----------------------------------------------
            SectionLabel { text: Theme.tr("Performance") }
            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 16
                Label { text: Theme.tr("Render resolution"); color: Theme.text }
                Slider {
                    Layout.fillWidth: true
                    from: 0.25; to: 1; stepSize: 0.05
                    value: page.settings.renderScale || 1
                    onMoved: Core.call("settings.set", { key: "renderScale", value: value })
                }
                Label { text: Math.round((page.settings.renderScale || 1) * 100) + " %"; color: Theme.textDim }
            }

            // ---- about --------------------------------------------------------
            SectionLabel { text: Theme.tr("About") }
            RowLayout {
                Layout.leftMargin: 16
                spacing: 16
                Image { source: "qrc:/qt/qml/Beatlumen/assets/icon.png"; sourceSize: Qt.size(64, 64); Layout.preferredWidth: 64; Layout.preferredHeight: 64 }
                ColumnLayout {
                    spacing: 2
                    Label { text: "Beatlumen " + Core.version; font.pixelSize: 18; font.weight: Font.Bold; color: Theme.text }
                    Label { text: "GNU GPL v3.0 or later"; color: Theme.textDim }
                }
            }
            Label {
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
