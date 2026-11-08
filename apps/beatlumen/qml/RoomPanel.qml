import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Listening together: make a room or join one with its code or link. In a room:
// how others get in, who is there, and how well it keeps time.
Rectangle {
    id: panel
    readonly property var room: Core.state.room || ({})
    readonly property bool active: room.active === true
    readonly property bool connected: Core.state.server === "connected"
    readonly property var members: room.members || []
    implicitHeight: col.implicitHeight + 28
    radius: Theme.radius
    color: Theme.surfaceVariant

    function spaced(code) {
        const c = String(code || "")
        return c.length > 4 ? c.slice(0, 4) + " " + c.slice(4) : c
    }
    function status(m) {
        if (room.phase === "preparing")
            return m.ready === true ? Theme.tr("Has the song") : Theme.tr("Loading…")
        if (m.driftMs === undefined)
            return ""
        const off = Math.round(Math.abs(m.driftMs))
        return off < 30 ? Theme.tr("In time") : off + " ms"
    }
    function role(m) {
        if (m.host === true)
            return Theme.tr("Host")
        return m.guest === true ? Theme.tr("Guest") : ""
    }

    ColumnLayout {
        id: col
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: 14 }
        spacing: 8

        RowLayout {
            Layout.fillWidth: true
            spacing: 10
            Icon { name: "person"; size: 22; color: Theme.accent }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: Theme.tr("Listen together")
                font.pixelSize: 16
                font.weight: Font.Medium
                color: Theme.text
            }
        }

        // ---- not in a room ------------------------------------------------
        Label {
            visible: !panel.active
            textFormat: Text.PlainText
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: Theme.textDim
            font.pixelSize: 13
            text: Theme.tr("Everyone hears the same song at the same moment. Only songs of the server can be played in a room.")
        }
        Flow {
            visible: !panel.active
            Layout.fillWidth: true
            spacing: 8
            AccentButton {
                visible: panel.connected
                text: Theme.tr("Create room")
                highlighted: true
                enabled: panel.room.entering !== true
                onClicked: Core.call("room.create", { guests: true })
            }
            InputField {
                id: codeField
                width: Math.min(260, panel.width - 28)
                placeholderText: Theme.tr("Code or link")
                onAccepted: joinButton.clicked()
            }
            Button {
                id: joinButton
                text: Theme.tr("Join")
                enabled: codeField.text.trim() !== "" && panel.room.entering !== true
                onClicked: Core.call("room.join", { code: codeField.text })
            }
        }
        Label {
            visible: !panel.active && (panel.room.error || "") !== ""
            textFormat: Text.PlainText
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: Theme.danger
            font.pixelSize: 13
            text: panel.room.error || ""
        }

        // ---- in a room ----------------------------------------------------
        RowLayout {
            visible: panel.active
            Layout.fillWidth: true
            spacing: 8
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: panel.spaced(panel.room.code)
                font.pixelSize: 24
                font.weight: Font.Medium
                font.letterSpacing: 2
                color: Theme.text
                elide: Text.ElideRight
            }
            IconButton {
                iconName: "content_copy"
                tip: Theme.tr("Copy code")
                onClicked: Core.copyText(panel.room.code)
            }
            IconButton {
                iconName: "share"
                tip: Theme.tr("Copy link")
                onClicked: Core.copyText(panel.room.link)
            }
        }
        SwitchRow {
            visible: panel.active && panel.room.host === true
            Layout.fillWidth: true
            title: Theme.tr("Allow guests")
            detail: Core.state.serverRoomGuests === false ? Theme.tr("Turned off for this server by its admin.")
                                                          : Theme.tr("People without a login can join with the code or link. They get this room's songs and nothing else, for as long as it lasts.")
            enabled: Core.state.serverRoomGuests !== false
            on: panel.room.guests === true
            onSwitched: (on) => Core.call("room.guests", { on: on })
        }
        Repeater {
            model: panel.active ? panel.members : []
            RowLayout {
                required property var modelData
                Layout.fillWidth: true
                spacing: 8
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: modelData.name || Theme.tr("Guest")
                    font.pixelSize: 14
                    elide: Text.ElideRight
                    color: Theme.text
                }
                Label {
                    textFormat: Text.PlainText
                    text: panel.role(modelData)
                    visible: text !== ""
                    font.pixelSize: 12
                    color: Theme.textDim
                }
                Label {
                    textFormat: Text.PlainText
                    text: panel.status(modelData)
                    visible: text !== ""
                    font.pixelSize: 12
                    color: Theme.textDim
                }
            }
        }
        Label {
            visible: panel.active && panel.room.host !== true
            textFormat: Text.PlainText
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: Theme.textDim
            font.pixelSize: 13
            text: Theme.tr("The host chooses what plays.")
        }
        Button {
            visible: panel.active
            flat: true
            text: panel.room.host === true ? Theme.tr("End room") : Theme.tr("Leave room")
            onClicked: Core.call("room.leave")
        }
    }
}
