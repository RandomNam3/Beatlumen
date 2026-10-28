import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// The account on the server and its end-to-end encrypted files: unlock,
// create an account, recover it; list, upload, play, share and delete
// files; compare safety numbers with other people.
Item {
    id: page
    property bool back: false
    signal backClicked()
    signal upload()

    readonly property var vault: Core.state.vault || ({})
    readonly property string vstate: vault.state || "locked"
    readonly property bool ready: vstate === "ready"
    readonly property int rev: vault.rev || 0
    readonly property bool connected: Core.state.server === "connected" || Core.state.server === "login"
    property var files: []
    property var contacts: []
    property int form: 0      // locked: 0 unlock, 1 create account, 2 forgot password
    property int tab: 0       // unlocked: 0 files, 1 people, 2 account

    function reload() {
        files = Core.call("vault.files") || []
        contacts = Core.call("vault.contacts") || []
    }
    function size(n) {
        if (n >= 1073741824) return (n / 1073741824).toFixed(1) + " GB"
        if (n >= 1048576) return (n / 1048576).toFixed(1) + " MB"
        return Math.max(1, Math.round(n / 1024)) + " KB"
    }
    function fileSubtitle(f) {
        if (f.problem) return f.problem
        if (!f.complete) return Theme.tr("Upload not finished")
        const parts = []
        if (f.artist) parts.push(f.artist)
        parts.push(page.size(f.size))
        if (!f.mine) parts.push(Theme.tr("from") + " " + f.owner)
        else if (f.recipients.length > 0) parts.push(Theme.tr("shared with") + " " + f.recipients.join(", "))
        return parts.join(" · ")
    }
    function statusText(s) {
        return s === "verified" ? Theme.tr("Verified")
             : s === "changed" ? Theme.tr("Key changed!")
             : Theme.tr("Not verified")
    }

    onRevChanged: reload()
    // After locking, offer unlocking first.
    onReadyChanged: { form = 0; tab = 0 }
    Component.onCompleted: { reload(); if (ready) Core.call("vault.refresh") }

    ColumnLayout {
        anchors.fill: parent
        spacing: 8

        PageHeader {
            Layout.fillWidth: true
            title: Theme.tr("Vault")
            subtitle: page.ready ? (page.vault.user + " · " + page.size(page.vault.used || 0)
                                    + (page.vault.quota > 0 ? " / " + page.size(page.vault.quota) : ""))
                                 : Theme.tr("Only you and the people you share with can read them")
            back: page.back
            onBackClicked: page.backClicked()
            IconButton {
                visible: page.ready
                iconName: "upload"
                tip: Theme.tr("Upload files")
                onClicked: page.upload()
            }
            IconButton {
                visible: page.ready
                iconName: "lock"
                tip: Theme.tr("Lock")
                onClicked: Core.call("vault.logout")
            }
        }

        // Small note for the browser: the code comes from the server.
        Label {
            textFormat: Text.PlainText
            visible: page.vault.web === true
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            text: Theme.tr("In the browser, the protection depends on the server delivering an unmodified web app. For the best protection use the Beatlumen app.")
            wrapMode: Text.WordWrap
            color: Theme.textFaint
            font.pixelSize: 12
        }

        RowLayout {
            visible: page.vstate === "busy"
            Layout.fillWidth: true
            Layout.leftMargin: 16
            spacing: 12
            BusyIndicator { running: parent.visible; implicitWidth: 32; implicitHeight: 32 }
            Label { textFormat: Text.PlainText; text: page.vault.busy || ""; color: Theme.textDim }
        }
        Label {
            textFormat: Text.PlainText
            visible: (page.vault.error || "") !== ""
            Layout.fillWidth: true
            Layout.leftMargin: 16
            Layout.rightMargin: 16
            text: page.vault.error || ""
            wrapMode: Text.WordWrap
            color: Theme.danger
        }

        // ---- locked: unlock / create / recover ---------------------------
        EmptyState {
            visible: !page.connected
            Layout.fillWidth: true
            Layout.margins: 24
            iconName: "cloud_off"
            title: Theme.tr("Not connected to a server")
            text: Theme.tr("Connect to a Beatlumen server in the settings first.")
        }
        ScrollView {
            visible: page.connected && !page.ready
            Layout.fillWidth: true
            Layout.fillHeight: true
            ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
            contentWidth: availableWidth
            enabled: page.vstate !== "busy"
            ColumnLayout {
                id: forms
                width: parent.width
                spacing: 10
                Flow {
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    spacing: 6
                    Repeater {
                        model: [Theme.tr("Unlock"), Theme.tr("Create account"), Theme.tr("Forgot password")]
                        AccentButton {
                            required property string modelData
                            required property int index
                            text: modelData
                            checkable: true
                            flat: !checked
                            highlighted: checked
                            checked: page.form === index
                            onClicked: { page.form = index; Core.call("vault.clearError") }
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
                    text: page.form === 0 ? Theme.tr("Your password unlocks the keys on this device; it never leaves it.")
                        : page.form === 1 ? Theme.tr("You need an invitation code from the server's admin. Choose a long password: nobody, not even the admin, can reset it without your recovery code.")
                        : Theme.tr("Enter the recovery code you wrote down when creating the account, and choose a new password.")
                }
                TextField {
                    id: invite
                    visible: page.form === 1
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    placeholderText: Theme.tr("Invitation code")
                    inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
                }
                TextField {
                    id: user
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    placeholderText: Theme.tr("User name")
                    text: page.vault.user || ""
                    inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText | Qt.ImhPreferLowercase
                }
                TextField {
                    id: code
                    visible: page.form === 2
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    placeholderText: Theme.tr("Recovery code")
                    font.family: "monospace"
                    inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
                }
                TextField {
                    id: password
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    placeholderText: page.form === 2 ? Theme.tr("New password") : Theme.tr("Password")
                    echoMode: TextInput.Password
                    onAccepted: submit.clicked()
                }
                TextField {
                    id: repeat
                    visible: page.form !== 0
                    Layout.fillWidth: true
                    Layout.leftMargin: 16
                    Layout.rightMargin: 16
                    placeholderText: Theme.tr("Repeat password")
                    echoMode: TextInput.Password
                    onAccepted: submit.clicked()
                }
                Label {
                    textFormat: Text.PlainText
                    visible: page.form !== 0 && repeat.text.length > 0 && repeat.text !== password.text
                    Layout.leftMargin: 16
                    text: Theme.tr("The passwords do not match")
                    color: Theme.danger
                }
                AccentButton {
                    id: submit
                    Layout.leftMargin: 16
                    highlighted: true
                    icon.source: Theme.icon(page.form === 0 ? "lock_open" : "key")
                    text: page.form === 0 ? Theme.tr("Unlock") : page.form === 1 ? Theme.tr("Create account") : Theme.tr("Recover account")
                    enabled: user.text.length > 0 && password.text.length > 0
                             && (page.form === 0 || repeat.text === password.text)
                             && (page.form !== 1 || invite.text.length > 0)
                             && (page.form !== 2 || code.text.length > 0)
                    onClicked: {
                        if (page.form === 0)
                            Core.call("vault.login", { user: user.text, password: password.text })
                        else if (page.form === 1)
                            Core.call("vault.register", { invite: invite.text, user: user.text, password: password.text })
                        else
                            Core.call("vault.recover", { user: user.text, code: code.text, password: password.text })
                        password.clear()
                        repeat.clear()
                    }
                }
            }
        }

        // ---- unlocked ----------------------------------------------------
        Flow {
            visible: page.ready
            Layout.fillWidth: true
            Layout.leftMargin: 12
            spacing: 6
            Repeater {
                model: [Theme.tr("Files"), Theme.tr("People"), Theme.tr("Account")]
                AccentButton {
                    required property string modelData
                    required property int index
                    text: modelData
                    checkable: true
                    flat: !checked
                    highlighted: checked
                    checked: page.tab === index
                    onClicked: page.tab = index
                }
            }
        }

        StackLayout {
            visible: page.ready
            Layout.fillWidth: true
            Layout.fillHeight: true
            currentIndex: page.tab

            // Files
            Item {
                ListView {
                    id: fileList
                    anchors.fill: parent
                    clip: true
                    model: page.files
                    ScrollBar.vertical: ScrollBar { }
                    header: Column {
                        width: fileList.width
                        Repeater {
                            model: page.vault.uploads || []
                            ItemDelegate {
                                required property var modelData
                                width: fileList.width
                                height: 56
                                contentItem: ColumnLayout {
                                    spacing: 4
                                    Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: (modelData.error ? Theme.tr("Upload failed:") + " " : Theme.tr("Encrypting and uploading") + " ") + modelData.name; elide: Text.ElideRight; color: modelData.error ? Theme.danger : Theme.text }
                                    ProgressBar { Layout.fillWidth: true; visible: !modelData.error; value: modelData.progress; indeterminate: modelData.progress === 0 }
                                }
                            }
                        }
                    }
                    delegate: ItemDelegate {
                        id: row
                        required property var modelData
                        required property int index
                        width: fileList.width
                        height: 68
                        onClicked: if (modelData.audio) Core.call("play", { ids: page.files.filter(f => f.audio).map(f => f.id), start: page.files.filter(f => f.audio).findIndex(f => f.id === modelData.id) })
                        contentItem: RowLayout {
                            spacing: 14
                            Icon {
                                name: row.modelData.problem ? "warning" : row.modelData.mine ? "lock" : "share"
                                size: 24
                                color: row.modelData.problem ? Theme.danger : Theme.textDim
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: row.modelData.title || row.modelData.name || row.modelData.id; font.pixelSize: 16; elide: Text.ElideRight; color: Theme.text }
                                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: page.fileSubtitle(row.modelData); font.pixelSize: 13; elide: Text.ElideRight; color: row.modelData.problem ? Theme.danger : Theme.textDim }
                            }
                            IconButton {
                                iconName: "more_vert"
                                tip: Theme.tr("More")
                                onClicked: fileMenu.openFor(row.modelData, this)
                            }
                        }
                    }
                    EmptyState {
                        anchors.centerIn: parent
                        width: Math.min(parent.width - 48, 420)
                        visible: page.files.length === 0 && (page.vault.uploads || []).length === 0
                        iconName: "lock"
                        title: Theme.tr("No encrypted files yet")
                        text: Theme.tr("Files are encrypted on this device before they are uploaded. The server stores them but cannot read them.")
                        AccentButton { text: Theme.tr("Upload files"); highlighted: true; icon.source: Theme.icon("upload"); onClicked: page.upload() }
                    }
                }
            }

            // People
            ListView {
                id: people
                clip: true
                model: page.contacts
                ScrollBar.vertical: ScrollBar { }
                header: Label {
                    textFormat: Text.PlainText
                    width: people.width
                    padding: 16
                    text: Theme.tr("Compare the safety number with each person, in person or on a call. If both see the same number, nobody swapped your keys.")
                    wrapMode: Text.WordWrap
                    color: Theme.textDim
                }
                delegate: ItemDelegate {
                    id: prow
                    required property var modelData
                    width: people.width
                    height: 64
                    onClicked: safety.show(modelData)
                    contentItem: RowLayout {
                        spacing: 14
                        Icon {
                            name: prow.modelData.status === "verified" ? "verified_user" : prow.modelData.status === "changed" ? "warning" : "person"
                            size: 24
                            color: prow.modelData.status === "changed" ? Theme.danger : prow.modelData.status === "verified" ? Theme.accent : Theme.textDim
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label { textFormat: Text.PlainText; text: prow.modelData.name; font.pixelSize: 16; color: Theme.text }
                            Label { textFormat: Text.PlainText; text: page.statusText(prow.modelData.status); font.pixelSize: 13; color: prow.modelData.status === "changed" ? Theme.danger : Theme.textDim }
                        }
                    }
                }
            }

            // Account
            ScrollView {
                ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
                contentWidth: availableWidth
                ColumnLayout {
                    width: parent.width
                    spacing: 10
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        text: Theme.tr("Key fingerprint") + ": " + (page.vault.fingerprint || "")
                        font.family: "monospace"
                        wrapMode: Text.WrapAnywhere
                        color: Theme.textDim
                    }
                    Button { Layout.leftMargin: 16; text: Theme.tr("Change password"); icon.source: Theme.icon("key"); onClicked: changePw.open() }
                    Button { Layout.leftMargin: 16; text: Theme.tr("New recovery code"); icon.source: Theme.icon("refresh"); onClicked: newCode.open() }
                    Button { Layout.leftMargin: 16; text: Theme.tr("Lock"); flat: true; icon.source: Theme.icon("lock"); onClicked: Core.call("vault.logout") }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 16
                        Layout.rightMargin: 16
                        text: Theme.tr("The keys stay in memory only while unlocked. After a restart the password is needed again.")
                        wrapMode: Text.WordWrap
                        color: Theme.textFaint
                        font.pixelSize: 12
                    }
                }
            }
        }
    }

    // ---- menus and dialogs -------------------------------------------------

    Menu {
        id: fileMenu
        property var file: ({ recipients: [] })
        function openFor(f, anchor) {
            file = f
            parent = anchor
            x = anchor.width - width
            y = anchor.height
            open()
        }
        MenuItem { text: Theme.tr("Play"); icon.source: Theme.icon("play_arrow"); enabled: fileMenu.file.audio === true; onTriggered: Core.call("play", { ids: [fileMenu.file.id], start: 0 }) }
        MenuItem { text: Theme.tr("Add to queue"); icon.source: Theme.icon("queue_music"); enabled: fileMenu.file.audio === true; onTriggered: Core.call("enqueue", { ids: [fileMenu.file.id] }) }
        MenuItem { text: Theme.tr("Share…"); icon.source: Theme.icon("share"); visible: fileMenu.file.mine === true && fileMenu.file.complete === true && !fileMenu.file.problem; height: visible ? implicitHeight : 0; onTriggered: shareDialog.show(fileMenu.file) }
        Repeater {
            model: fileMenu.file.mine ? fileMenu.file.recipients : []
            MenuItem {
                required property string modelData
                text: Theme.tr("Stop sharing with") + " " + modelData
                icon.source: Theme.icon("close")
                onTriggered: Core.call("vault.revoke", { id: fileMenu.file.id.replace("vault:", ""), user: modelData })
            }
        }
        MenuItem {
            text: fileMenu.file.mine ? Theme.tr("Delete") : Theme.tr("Remove from my files")
            icon.source: Theme.icon("delete")
            onTriggered: confirmDelete.open()
        }
    }

    Dialog {
        id: confirmDelete
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(420, page.width - 32)
        title: fileMenu.file.mine ? Theme.tr("Delete file?") : Theme.tr("Remove from my files?")
        standardButtons: Dialog.Cancel | Dialog.Ok
        Label {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.WordWrap
            text: fileMenu.file.mine ? Theme.tr("The file is deleted on the server for you and everyone you shared it with.") : Theme.tr("You lose access; the owner keeps the file.")
        }
        onAccepted: Core.call(fileMenu.file.mine ? "vault.delete" : "vault.leave", { id: fileMenu.file.id.replace("vault:", "") })
    }

    Dialog {
        id: shareDialog
        property var file: ({})
        function show(f) { file = f; shareError.text = ""; open() }
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(440, page.width - 32)
        title: Theme.tr("Share") + " " + (file.title || "")
        standardButtons: Dialog.Cancel
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: Theme.tr("The file key is encrypted for the person you choose; the server cannot read it.")
            }
            Repeater {
                model: page.contacts.filter(c => (shareDialog.file.recipients || []).indexOf(c.name) < 0)
                ItemDelegate {
                    required property var modelData
                    Layout.fillWidth: true
                    text: modelData.name + (modelData.status === "verified" ? " ✓" : modelData.status === "changed" ? " ⚠" : "")
                    icon.source: Theme.icon(modelData.status === "changed" ? "warning" : "person")
                    onClicked: {
                        const r = Core.call("vault.share", { id: shareDialog.file.id.replace("vault:", ""), user: modelData.name })
                        if (r && r.error) shareError.text = r.error
                        else shareDialog.close()
                    }
                }
            }
            Label { id: shareError; textFormat: Text.PlainText; Layout.fillWidth: true; visible: text !== ""; wrapMode: Text.WordWrap; color: Theme.danger }
            Label {
                textFormat: Text.PlainText
                visible: page.contacts.length === 0
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: Theme.tr("Nobody else has an account on this server yet.")
                color: Theme.textFaint
            }
        }
    }

    Dialog {
        id: safety
        property var contact: ({})
        function show(c) { contact = c; open() }
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(440, page.width - 32)
        title: Theme.tr("Safety number with") + " " + (contact.name || "")
        standardButtons: Dialog.Close
        ColumnLayout {
            anchors.fill: parent
            spacing: 10
            Label {
                textFormat: Text.PlainText
                visible: safety.contact.status === "changed"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.danger
                text: Theme.tr("The server now shows a different key for this person. This happens when they created a new account, or when someone tries to intercept shared files. Compare the safety number before you accept it.")
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                text: (safety.contact.safetyNumber || "").split(" ").reduce((acc, g, i) => acc + g + (i % 3 === 2 ? "\n" : "  "), "").trim()
                font.family: "monospace"
                font.pixelSize: 20
                color: Theme.text
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: Theme.tr("Ask the other person to open your name in their list and compare the numbers.")
            }
            AccentButton {
                visible: safety.contact.status === "pinned"
                highlighted: true
                text: Theme.tr("The numbers match")
                icon.source: Theme.icon("verified_user")
                onClicked: { Core.call("vault.verify", { user: safety.contact.name }); safety.close() }
            }
            Button {
                visible: safety.contact.status === "changed"
                text: Theme.tr("Accept the new key")
                onClicked: { Core.call("vault.acceptKey", { user: safety.contact.name }); safety.close() }
            }
        }
    }

    // Shown once after creating the account or a new code.
    Dialog {
        id: codeDialog
        modal: true
        closePolicy: Popup.NoAutoClose
        visible: (page.vault.recoveryCode || "") !== ""
        anchors.centerIn: Overlay.overlay
        width: Math.min(460, page.width - 32)
        title: Theme.tr("Recovery code")
        ColumnLayout {
            anchors.fill: parent
            spacing: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.text
                text: Theme.tr("Write this code down and keep it in a safe place. If you forget your password, it is the only way to get your files back. It is shown only once.")
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                text: (page.vault.recoveryCode || "").split("-").reduce((acc, g, i) => acc + g + (i % 4 === 3 ? "\n" : "-"), "").replace(/[-\n]$/, "")
                font.family: "monospace"
                font.pixelSize: 18
                color: Theme.accent
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button { text: Theme.tr("Copy"); flat: true; icon.source: Theme.icon("content_copy"); onClicked: Core.copyText(page.vault.recoveryCode) }
                AccentButton { text: Theme.tr("I have written it down"); highlighted: true; onClicked: Core.call("vault.dismissCode") }
            }
        }
    }

    Dialog {
        id: changePw
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(420, page.width - 32)
        title: Theme.tr("Change password")
        standardButtons: Dialog.Cancel
        onOpened: { cur.clear(); np.clear(); np2.clear(); cur.forceActiveFocus() }
        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            TextField { id: cur; Layout.fillWidth: true; placeholderText: Theme.tr("Current password"); echoMode: TextInput.Password }
            TextField { id: np; Layout.fillWidth: true; placeholderText: Theme.tr("New password"); echoMode: TextInput.Password }
            TextField { id: np2; Layout.fillWidth: true; placeholderText: Theme.tr("Repeat password"); echoMode: TextInput.Password }
            AccentButton {
                Layout.alignment: Qt.AlignRight
                highlighted: true
                text: Theme.tr("Change password")
                enabled: cur.text.length > 0 && np.text.length >= 10 && np.text === np2.text
                onClicked: { Core.call("vault.changePassword", { current: cur.text, password: np.text }); changePw.close() }
            }
        }
    }

    TextPrompt {
        id: newCode
        title: Theme.tr("New recovery code")
        placeholder: Theme.tr("Current password")
        password: true
        acceptText: Theme.tr("Create")
        onSubmitted: (t) => Core.call("vault.newRecoveryCode", { password: t })
    }
}
