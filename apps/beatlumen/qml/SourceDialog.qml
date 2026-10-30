import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Adds or edits a music source the app uses by itself (Subsonic, Jellyfin,
// WebDAV/Nextcloud, DLNA media server).
AppDialog {
    id: dlg
    // The edited source (from Core.state.sources.list), or null for a new one.
    property var source: null
    readonly property var kinds: [
        { id: "subsonic", name: "Subsonic / Navidrome", hint: "Navidrome, Gonic, Airsonic and other Subsonic servers.", url: "https://music.example.org" },
        { id: "jellyfin", name: "Jellyfin", hint: "The music libraries of a Jellyfin server.", url: "https://jellyfin.example.org" },
        { id: "webdav", name: "WebDAV / Nextcloud", hint: "A folder on a WebDAV share. Nextcloud: use an app password.", url: "https://cloud.example.org/remote.php/dav/files/NAME" },
        { id: "sftp", name: "SFTP", hint: "A folder on a computer reachable with SSH (password login).", url: "sftp://nas.local" },
        { id: "dlna", name: Theme.tr("DLNA media server"), hint: "A media server in the local network.", url: "http://192.168.1.10:8200/rootDesc.xml" }
    ]
    readonly property var kind: kinds[kindBox.currentIndex]
    readonly property var state: Core.state.sources || ({})

    title: Theme.tr(source ? "Edit source" : "Add music source")
    anchors.centerIn: Overlay.overlay
    width: Math.min(480, (parent ? parent.width : 480) - 32)
    height: Math.min(implicitHeight, (parent ? parent.height : 640) - 32)
    standardButtons: Dialog.NoButton

    function openFor(s) {
        source = s || null
        const i = s ? kinds.findIndex((k) => k.id === s.kind) : 0
        kindBox.currentIndex = Math.max(0, i)
        nameField.text = s ? s.name : ""
        urlField.text = s ? s.url : ""
        userField.text = s ? s.user : ""
        secretField.text = ""
        pathField.text = s ? s.path : ""
        open()
    }

    function save() {
        const r = Core.call("sources.save", {
            id: source ? source.id : "",
            kind: kind.id,
            name: nameField.text.trim(),
            url: urlField.text.trim(),
            user: userField.text.trim(),
            secret: secretField.text,
            path: pathField.text.trim(),
            enabled: source ? source.enabled : true
        })
        if (r && r.error)
            error.text = r.error
        else
            close()
    }

    onOpened: error.text = ""

    ScrollView {
        id: sv
        anchors.fill: parent
        clip: true
        contentWidth: availableWidth
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

        ColumnLayout {
            width: sv.availableWidth
            spacing: 6

            ComboBox {
                id: kindBox
                Layout.fillWidth: true
                enabled: !dlg.source
                model: dlg.kinds.map((k) => k.name)
                Accessible.name: Theme.tr("Kind")
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Theme.textDim
                font.pixelSize: 13
                text: Theme.tr(dlg.kind.hint)
            }
            TextField {
                id: nameField
                Layout.fillWidth: true
                placeholderText: Theme.tr("Name")
            }

            // DLNA: servers found in the network.
            RowLayout {
                visible: dlg.kind.id === "dlna"
                Layout.fillWidth: true
                Button {
                    text: Theme.tr("Search the network")
                    icon.source: Theme.icon("search")
                    enabled: !dlg.state.searching
                    onClicked: Core.call("sources.discover")
                }
                BusyIndicator { running: !!dlg.state.searching; visible: running; implicitWidth: 32; implicitHeight: 32 }
            }
            Repeater {
                model: dlg.kind.id === "dlna" ? (dlg.state.found || []) : []
                delegate: ItemDelegate {
                    Layout.fillWidth: true
                    text: modelData.name
                    icon.source: Theme.icon("storage")
                    onClicked: {
                        urlField.text = modelData.url
                        if (!nameField.text.trim())
                            nameField.text = modelData.name
                    }
                }
            }

            TextField {
                id: urlField
                Layout.fillWidth: true
                placeholderText: Theme.tr("Address") + " (" + dlg.kind.url + ")"
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
                Accessible.name: Theme.tr("Address")
            }
            TextField {
                id: userField
                visible: dlg.kind.id !== "dlna"
                Layout.fillWidth: true
                placeholderText: Theme.tr("User name")
                inputMethodHints: Qt.ImhNoAutoUppercase
            }
            TextField {
                id: secretField
                visible: dlg.kind.id !== "dlna"
                Layout.fillWidth: true
                echoMode: TextInput.Password
                placeholderText: dlg.source && dlg.source.hasSecret ? Theme.tr("Password (unchanged)") : Theme.tr("Password")
            }
            TextField {
                id: pathField
                visible: dlg.kind.id === "webdav" || dlg.kind.id === "sftp"
                Layout.fillWidth: true
                placeholderText: Theme.tr("Folder (empty: all)")
            }
            Label {
                id: error
                visible: text.length > 0
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Theme.danger
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button { text: Theme.tr("Cancel"); flat: true; onClicked: dlg.close() }
                AccentButton {
                    text: Theme.tr("Save")
                    highlighted: true
                    enabled: urlField.text.trim().length > 0
                    onClicked: dlg.save()
                }
            }
        }
    }
}
