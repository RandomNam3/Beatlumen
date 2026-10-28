import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Chooses where the music plays: this device or a speaker in the network
// (AirPlay). Opening the menu searches the network.
IconButton {
    id: btn
    property color fg: Theme.text
    readonly property var outputs: Core.state.outputs || ({})
    readonly property bool remote: (outputs.active || "local") !== "local"
    visible: Core.platform !== "web"
    iconName: remote ? "speaker" : "cast"
    iconColor: fg
    active: remote
    tip: remote ? Theme.tr("Playing on") + " " + (outputs.activeName || "") : Theme.tr("Play on…")
    onClicked: { Core.call("outputs.scan"); menu.open() }

    Menu {
        id: menu
        y: -height
        width: 300
        MenuItem {
            text: Theme.tr("This device")
            checkable: true
            checked: !btn.remote
            onTriggered: Core.call("outputs.select", { id: "local" })
        }
        Repeater {
            model: btn.outputs.devices || []
            delegate: MenuItem {
                required property var modelData
                text: modelData.name
                checkable: true
                checked: btn.outputs.active === modelData.id
                icon.source: Theme.icon(modelData.kind === "cast" ? "cast" : "speaker")
                onTriggered: {
                    if (modelData.needsPassword)
                        password.ask(modelData)
                    else
                        Core.call("outputs.select", { id: modelData.id })
                }
            }
        }
        MenuItem {
            enabled: false
            visible: btn.outputs.scanning === true || (btn.outputs.devices || []).length === 0
            height: visible ? implicitHeight : 0
            text: btn.outputs.scanning ? Theme.tr("Searching for speakers…") : Theme.tr("No speakers found")
        }
        MenuItem {
            enabled: false
            visible: btn.outputs.status === "connecting"
            height: visible ? implicitHeight : 0
            text: Theme.tr("Connecting…")
        }
    }

    TextPrompt {
        id: password
        property var device: ({})
        function ask(d) { device = d; text = ""; open() }
        parent: Overlay.overlay
        title: Theme.tr("Password for") + " " + (device.name || "")
        placeholder: Theme.tr("Password")
        password: true
        acceptText: Theme.tr("Connect")
        onSubmitted: (t) => Core.call("outputs.select", { id: device.id, password: t })
    }
}
