import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The first song of an extension: where do its songs go? In the library on
// the server (an ordinary file the server can read and analyse, shown in the
// user's library only) or in the user's encrypted files (end-to-end, the
// server cannot read them); remembered afterwards, changed in the settings.
AppDialog {
    id: target
    readonly property bool asking: (Core.state.extensions || {}).asking === true
    readonly property bool vaultReady: (Core.state.vault || {}).state === "ready"
    // Closed by answering, or left without an answer.
    property bool answered: false
    onAskingChanged: {
        if (asking) {
            answered = false
            open()
        } else {
            close()
        }
    }
    onClosed: if (!answered && asking) Core.call("extensions.target", { target: "" })
    function choose(where) {
        answered = true
        Core.call("extensions.target", { target: where })
    }
    modal: true
    anchors.centerIn: Overlay.overlay
    width: Math.min(460, (parent ? parent.width : 460) - 32)
    title: Theme.tr("Where should the song stay?")
    standardButtons: Dialog.Cancel
    onAboutToShow: Theme.nameButtons(this)
    ColumnLayout {
        anchors.fill: parent
        spacing: 6
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: Theme.textDim
            text: Theme.tr("It plays right away. A copy stays in one of two places, chosen once:")
        }
        component Choice: ItemDelegate {
            id: ch
            property string heading
            property string detail
            property string iconName
            Layout.fillWidth: true
            Accessible.name: heading
            contentItem: RowLayout {
                spacing: 14
                Icon { name: ch.iconName; size: 26; color: ch.enabled ? Theme.accent : Theme.textFaint }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: ch.heading; font.pixelSize: 15; font.weight: Font.Medium; color: ch.enabled ? Theme.text : Theme.textFaint; wrapMode: Text.WordWrap }
                    Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: ch.detail; font.pixelSize: 13; color: Theme.textDim; wrapMode: Text.WordWrap }
                }
            }
        }
        Choice {
            heading: target.vaultReady ? Theme.tr("Library on the server (recommended)") : Theme.tr("Library on the server")
            detail: target.vaultReady ? Theme.tr("A normal, unencrypted music file on the server, shown only in your library. The server can read it, so it can analyse it for the visuals, song texts and karaoke.")
                                      : Theme.tr("Without an account it lands in the library everyone on this server shares.")
            iconName: "library_music"
            onClicked: target.choose("library")
        }
        Choice {
            heading: Theme.tr("My encrypted files")
            detail: target.vaultReady ? Theme.tr("Encrypted on this device: not even the server can read it. Only you and whoever you share it with can play it.")
                                      : Theme.tr("Unlock your account first (Settings → Account).")
            iconName: "lock"
            enabled: target.vaultReady
            onClicked: target.choose("vault")
        }
    }
}
