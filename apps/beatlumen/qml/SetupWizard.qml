import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The first start: language and epilepsy mode first (before anything moves),
// then how it looks, where the music comes from, the listening history, and a
// short tour. Every step can be skipped, and the whole can be left at any time.
Popup {
    id: wizard
    signal addFolder()
    signal openSettings(string category)
    signal startTour()

    readonly property var settings: Core.state.settings || ({})
    readonly property var ui: settings.ui || ({})
    readonly property bool connected: Core.state.server === "connected"
    // Which steps there are; the history only with a server.
    readonly property var steps: {
        const s = ["welcome", "look"]
        if (Core.platform !== "web")
            s.push("music")
        if (connected)
            s.push("history")
        s.push("online")
        s.push("tour")
        return s
    }
    property int step: 0
    readonly property string current: steps[Math.min(step, steps.length - 1)]

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    modal: true
    padding: 0
    closePolicy: Popup.CloseOnEscape
    onOpened: { step = 0; nextButton.forceActiveFocus() }
    onClosed: Core.call("settings.set", { key: "ui.setupDone", value: true })
    Accessible.role: Accessible.Dialog
    Accessible.name: Theme.tr("Setup")

    function next() {
        if (step + 1 < steps.length)
            step++
        else
            close()
    }

    background: Rectangle { color: Theme.dark ? "#121216" : "#fafafc" }

    contentItem: ColumnLayout {
        spacing: 0

        // Where you are, and the way out.
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                Layout.leftMargin: 8
                text: Theme.tr("Step %1 of %2").arg(wizard.step + 1).arg(wizard.steps.length)
                color: Theme.textDim
                font.pixelSize: 13
            }
            Button {
                flat: true
                text: Theme.tr("Skip setup")
                onClicked: wizard.close()
            }
        }

        ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentWidth: availableWidth
            clip: true
            ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
            ColumnLayout {
                width: Math.min(620, scroll.availableWidth)
                x: (scroll.availableWidth - width) / 2
                spacing: 12

                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    wrapMode: Text.WordWrap
                    color: Theme.text
                    font.pixelSize: 28
                    font.weight: Font.Bold
                    Accessible.role: Accessible.Heading
                    Accessible.name: text
                    text: {
                        switch (wizard.current) {
                        case "welcome": return Theme.tr("Welcome to Beatlumen")
                        case "look": return Theme.tr("How should it look?")
                        case "music": return Theme.tr("Where does your music come from?")
                        case "history": return Theme.tr("Personal recommendations?")
                        case "online": return Theme.tr("Artist information from the internet?")
                        default: return Theme.tr("Ready")
                        }
                    }
                }

                // ---- welcome ----------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "welcome"
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("Beatlumen plays your music and lets pictures and light move with it. A few questions, all of them can be skipped.")
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: Theme.tr("Language")
                            color: Theme.text
                        }
                        ComboBox {
                            readonly property var codes: ["", "de", "en"]
                            model: [Theme.tr("System"), "Deutsch", "English"]
                            currentIndex: Math.max(0, codes.indexOf(wizard.settings.language || ""))
                            onActivated: (i) => Core.call("settings.set", { key: "language", value: codes[i] })
                            Accessible.name: Theme.tr("Language")
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        Layout.topMargin: 8
                        wrapMode: Text.WordWrap
                        color: Theme.text
                        font.weight: Font.DemiBold
                        text: Theme.tr("The visuals flash and change colour with the music. If that is a problem for you (epilepsy, migraine), turn this on now:")
                    }
                    SwitchRow {
                        Layout.fillWidth: true
                        title: Theme.tr("Epilepsy mode")
                        detail: Theme.tr("Prevents flashes and fast colour changes as far as possible: brightness changes slowly, flash effects and hard cuts are off. This lowers the risk but cannot rule it out.")
                        on: wizard.settings.photosensitive === true
                        onSwitched: (v) => Core.call("settings.set", { key: "photosensitive", value: v })
                    }
                }

                // ---- look -----------------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "look"
                    spacing: 8
                    Flow {
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        spacing: 8
                        Repeater {
                            model: [{ v: "system", l: Theme.tr("System") }, { v: "light", l: Theme.tr("Light") }, { v: "dark", l: Theme.tr("Dark") }]
                            AccentButton {
                                required property var modelData
                                text: modelData.l
                                checkable: true
                                checked: (wizard.ui.theme || "system") === modelData.v
                                highlighted: checked
                                flat: !checked
                                onClicked: Core.call("settings.set", { key: "ui.theme", value: modelData.v })
                            }
                        }
                    }
                    AccessOptions {
                        Layout.fillWidth: true
                        showEpilepsy: false
                    }
                }

                // ---- music ------------------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "music"
                    spacing: 6
                    component Choice: ItemDelegate {
                        id: choice
                        property string iconName
                        property string detail
                        Layout.fillWidth: true
                        Accessible.role: Accessible.Button
                        Accessible.name: text
                        Accessible.description: detail
                        contentItem: RowLayout {
                            spacing: 14
                            Icon { name: choice.iconName; color: Theme.accent }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: choice.text; color: Theme.text; wrapMode: Text.WordWrap }
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: choice.detail; color: Theme.textDim; font.pixelSize: 12; wrapMode: Text.WordWrap }
                            }
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("You can change and add everything later in the settings.")
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        visible: (wizard.settings.libraryFolders || []).length > 0 || wizard.connected
                        wrapMode: Text.WordWrap
                        color: Theme.accentText
                        font.weight: Font.DemiBold
                        text: Theme.tr("%1 songs so far").arg(Core.state.libraryTracks || 0)
                    }
                    Choice {
                        iconName: "create_new_folder"
                        text: Theme.tr("Music on this device")
                        detail: Theme.tr("Add a folder with your songs.")
                        onClicked: wizard.addFolder()
                    }
                    Choice {
                        iconName: "cloud"
                        text: Theme.tr("A Beatlumen server")
                        detail: Theme.tr("Your library at home, analysed for the visuals, with accounts and encrypted files.")
                        onClicked: serverRow.visible = !serverRow.visible
                    }
                    RowLayout {
                        id: serverRow
                        visible: false
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        TextField {
                            id: serverField
                            Layout.fillWidth: true
                            placeholderText: "http://192.168.1.10:7878"
                            inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
                            Accessible.name: Theme.tr("Server address")
                            onAccepted: connectButton.clicked()
                        }
                        AccentButton {
                            id: connectButton
                            text: Theme.tr("Connect")
                            highlighted: true
                            enabled: serverField.text.trim() !== ""
                            onClicked: Core.call("server.connect", { url: serverField.text.trim() })
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        visible: serverRow.visible && Core.state.server !== "disabled"
                        wrapMode: Text.WordWrap
                        color: Core.state.server === "error" ? Theme.danger : Theme.textDim
                        text: Core.state.server === "connected" ? Theme.tr("Connected")
                            : Core.state.server === "login" ? Theme.tr("This server needs a password; log in in the settings.")
                            : Core.state.server === "error" ? (Core.state.serverError || Theme.tr("Could not connect"))
                            : Theme.tr("Connecting…")
                    }
                    Choice {
                        visible: (Core.state.sources || {}).available === true
                        iconName: "storage"
                        text: Theme.tr("Navidrome, Jellyfin, Nextcloud, a NAS …")
                        detail: Theme.tr("Play the music of other servers and storage directly.")
                        onClicked: { wizard.close(); wizard.openSettings("library") }
                    }
                    Choice {
                        iconName: "speaker"
                        text: Theme.tr("Use this device as a speaker")
                        detail: Theme.tr("iPhones, Macs, Windows and DLNA apps play here (AirPlay, DLNA) with visuals.")
                        onClicked: { wizard.close(); wizard.openSettings("receive") }
                    }
                }

                // ---- history ------------------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "history"
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("Your favorites and playlists give the server something to go by already. It suggests better if it also knows what you listen to.")
                    }
                    SwitchRow {
                        Layout.fillWidth: true
                        title: Theme.tr("Also keep it on the server")
                        detail: Theme.tr("What you play on this device also goes to your server, for recommendations and statistics across your devices. The server's admin could read it. Only the server's songs, and only from now on: local and encrypted files never leave this device.")
                        on: wizard.settings.historySync === true
                        onSwitched: (v) => Core.call("settings.set", { key: "historySync", value: v })
                    }
                }

                // ---- artist information ------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "online"
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("Artist pages with a biography, picture and members, and better recommendations, come from public music databases. Everything works without them too.")
                    }
                    SwitchRow {
                        Layout.fillWidth: true
                        title: Theme.tr("Look artists up on the internet")
                        detail: Theme.tr("The names of the artists in your library and your IP address go to MusicBrainz, Wikimedia and ListenBrainz, nothing else. Off, none of them is asked. You can change this at any time in the settings.")
                        on: wizard.settings.onlineMetadata === true
                        onSwitched: (v) => Core.call("settings.set", { key: "onlineMetadata", value: v })
                    }
                }

                // ---- tour -------------------------------------------------------------
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: wizard.current === "tour"
                    spacing: 12
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        wrapMode: Text.WordWrap
                        color: Theme.textDim
                        text: Theme.tr("Would you like a short tour of where everything is? It takes half a minute. You can start it again from More → Help.")
                    }
                    AccentButton {
                        Layout.leftMargin: 16
                        text: Theme.tr("Show me around")
                        highlighted: true
                        onClicked: { wizard.close(); wizard.startTour() }
                    }
                }
                Item { Layout.preferredHeight: 8 }
            }
        }

        // Back and on.
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 16
            Button {
                flat: true
                visible: wizard.step > 0
                text: Theme.tr("Back")
                onClicked: wizard.step--
            }
            Item { Layout.fillWidth: true }
            AccentButton {
                id: nextButton
                text: wizard.step + 1 < wizard.steps.length ? Theme.tr("Next") : Theme.tr("Done")
                highlighted: true
                onClicked: wizard.next()
            }
        }
    }
}
