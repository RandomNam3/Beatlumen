import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A smart playlist: rules instead of songs. It is worked out again whenever it
// is opened, so new songs that fit join by themselves.
AppDialog {
    id: dlg
    parent: Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(560, parent ? parent.width - 24 : 560)
    height: Math.min(implicitHeight, parent ? parent.height - 24 : 640)
    title: list.id ? Theme.tr("Edit smart playlist") : Theme.tr("New smart playlist")
    standardButtons: Dialog.Save | Dialog.Cancel
    closePolicy: Popup.CloseOnEscape

    signal saved(string id)
    property var list: ({ id: "", name: "", rules: [], any: false, order: "artist", limit: 0 })
    property var rules: []
    property var labels: []
    property var artists: []
    property string error: ""
    property int previewCount: 0

    readonly property var kinds: [
        { id: "label", name: Theme.tr("Genre, mood, label…"), not: true },
        { id: "artist", name: Theme.tr("Artist"), not: true },
        { id: "year", name: Theme.tr("Year"), range: true },
        { id: "bpm", name: Theme.tr("Tempo (BPM)"), range: true },
        { id: "rating", name: Theme.tr("Rated at least (stars)"), number: true, from: 1, to: 5, value: 4 },
        { id: "favorite", name: Theme.tr("Favorite"), not: true },
        { id: "plays", name: Theme.tr("Played at least (times)"), number: true, from: 1, to: 9999, value: 1, not: true },
        { id: "playedWithin", name: Theme.tr("Played within (days)"), number: true, from: 1, to: 36500, value: 30, not: true },
        { id: "addedWithin", name: Theme.tr("Added within (days)"), number: true, from: 1, to: 36500, value: 30, not: true }
    ]
    readonly property var orders: [
        { id: "artist", name: Theme.tr("By artist and album") },
        { id: "newest", name: Theme.tr("Newest first") },
        { id: "mostPlayed", name: Theme.tr("Most played first") },
        { id: "longestAgo", name: Theme.tr("Not heard for the longest time first") },
        { id: "random", name: Theme.tr("Random") }
    ]

    function kindOf(id) {
        return kinds.find(k => k.id === id) || kinds[0]
    }
    function edit(existing) {
        list = existing || { id: "", name: "", rules: [], any: false, order: "artist", limit: 0 }
        rules = (list.rules || []).map(r => Object.assign({}, r))
        if (rules.length === 0)
            rules = [{ kind: "label", value: "", not: false }]
        labels = (Core.call("labels.list") || []).map(l => l.name)
        artists = (Core.call("library.artists") || []).map(a => a.name)
        error = ""
        nameField.text = list.name || ""
        anyBox.currentIndex = list.any ? 1 : 0
        orderBox.currentIndex = Math.max(0, orders.findIndex(o => o.id === list.order))
        limitField.value = list.limit || 0
        updatePreview()
        open()
    }
    function currentList() {
        return {
            id: list.id || "",
            name: nameField.text,
            rules: rules,
            any: anyBox.currentIndex === 1,
            order: orders[orderBox.currentIndex].id,
            limit: limitField.value
        }
    }
    function setRule(index, change) {
        const next = rules.slice()
        next[index] = Object.assign({}, next[index], change)
        rules = next
        updatePreview()
    }
    function setKind(index, id) {
        const k = kindOf(id)
        const rule = { kind: id, not: false }
        if (id === "label" || id === "artist")
            rule.value = ""
        if (k.number) {
            if (id === "rating")
                rule.min = k.value / 5
            else if (id === "plays")
                rule.min = k.value
            else
                rule.days = k.value
        }
        const next = rules.slice()
        next[index] = rule
        rules = next
        updatePreview()
    }
    function numberOf(rule) {
        if (rule.kind === "rating")
            return Math.round((rule.min || 0.8) * 5)
        if (rule.kind === "plays")
            return rule.min || 1
        return rule.days || 30
    }
    function setNumber(index, rule, value) {
        if (rule.kind === "rating")
            setRule(index, { min: value / 5 })
        else if (rule.kind === "plays")
            setRule(index, { min: value })
        else
            setRule(index, { days: value })
    }
    function updatePreview() {
        previewCount = (Core.call("smart.preview", { list: currentList() }) || {}).count || 0
    }

    onAccepted: {
        const r = Core.call("smart.save", { list: currentList() }) || {}
        if (r.id) {
            saved(r.id)
        } else {
            error = r.error || ""
            open()
        }
    }

    contentItem: ScrollView {
        id: scroll
        contentWidth: availableWidth
        ColumnLayout {
            width: scroll.availableWidth
            spacing: 10
            TextField {
                id: nameField
                Layout.fillWidth: true
                placeholderText: Theme.tr("Name")
                Accessible.name: Theme.tr("Name")
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                visible: dlg.error !== ""
                text: dlg.error
                color: Theme.danger
                wrapMode: Text.WordWrap
            }
            RowLayout {
                Layout.fillWidth: true
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Songs that fit"); color: Theme.text; wrapMode: Text.WordWrap }
                ComboBox {
                    id: anyBox
                    model: [Theme.tr("all rules"), Theme.tr("at least one rule")]
                    Accessible.name: Theme.tr("Songs that fit")
                    implicitContentWidthPolicy: ComboBox.WidestText
                    onActivated: dlg.updatePreview()
                }
            }
            Repeater {
                model: dlg.rules
                delegate: ColumnLayout {
                    id: ruleRow
                    required property var modelData
                    required property int index
                    readonly property var kind: dlg.kindOf(modelData.kind)
                    Layout.fillWidth: true
                    spacing: 4
                    RowLayout {
                        Layout.fillWidth: true
                        ComboBox {
                            Layout.fillWidth: true
                            model: dlg.kinds.map(k => k.name)
                            currentIndex: dlg.kinds.findIndex(k => k.id === ruleRow.modelData.kind)
                            Accessible.name: Theme.tr("Rule")
                            onActivated: (i) => dlg.setKind(ruleRow.index, dlg.kinds[i].id)
                        }
                        IconButton {
                            iconName: "close"
                            tip: Theme.tr("Remove")
                            onClicked: {
                                const next = dlg.rules.slice()
                                next.splice(ruleRow.index, 1)
                                dlg.rules = next
                                dlg.updatePreview()
                            }
                        }
                    }
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8
                        ComboBox {
                            visible: ruleRow.kind.not === true
                            model: [Theme.tr("is"), Theme.tr("is not")]
                            implicitContentWidthPolicy: ComboBox.WidestText
                            currentIndex: ruleRow.modelData.not ? 1 : 0
                            Accessible.name: Theme.tr("Rule")
                            onActivated: (i) => dlg.setRule(ruleRow.index, { not: i === 1 })
                        }
                        ComboBox {
                            visible: ruleRow.modelData.kind === "label"
                            Layout.fillWidth: true
                            model: dlg.labels.map(l => Theme.labelName(l))
                            currentIndex: dlg.labels.indexOf(ruleRow.modelData.value || "")
                            displayText: currentIndex >= 0 ? currentText : Theme.tr("Please choose")
                            Accessible.name: Theme.tr("Genre, mood, label…")
                            onActivated: (i) => dlg.setRule(ruleRow.index, { value: dlg.labels[i] })
                        }
                        ComboBox {
                            visible: ruleRow.modelData.kind === "artist"
                            Layout.fillWidth: true
                            model: dlg.artists
                            currentIndex: dlg.artists.indexOf(ruleRow.modelData.value || "")
                            displayText: currentIndex >= 0 ? currentText : Theme.tr("Please choose")
                            Accessible.name: Theme.tr("Artist")
                            onActivated: (i) => dlg.setRule(ruleRow.index, { value: dlg.artists[i] })
                        }
                        SpinBox {
                            visible: ruleRow.kind.number === true
                            from: ruleRow.kind.from || 0
                            to: ruleRow.kind.to || 9999
                            editable: true
                            value: dlg.numberOf(ruleRow.modelData)
                            Accessible.name: ruleRow.kind.name
                            onValueModified: dlg.setNumber(ruleRow.index, ruleRow.modelData, value)
                        }
                        TextField {
                            visible: ruleRow.kind.range === true
                            Layout.fillWidth: true
                            placeholderText: Theme.tr("from")
                            text: ruleRow.modelData.min !== undefined && ruleRow.modelData.min !== null ? String(ruleRow.modelData.min) : ""
                            inputMethodHints: Qt.ImhDigitsOnly
                            validator: IntValidator { bottom: 0; top: 9999 }
                            Accessible.name: ruleRow.kind.name + " " + Theme.tr("from")
                            onEditingFinished: dlg.setRule(ruleRow.index, { min: text === "" ? null : Number(text) })
                        }
                        TextField {
                            visible: ruleRow.kind.range === true
                            Layout.fillWidth: true
                            placeholderText: Theme.tr("to")
                            text: ruleRow.modelData.max !== undefined && ruleRow.modelData.max !== null ? String(ruleRow.modelData.max) : ""
                            inputMethodHints: Qt.ImhDigitsOnly
                            validator: IntValidator { bottom: 0; top: 9999 }
                            Accessible.name: ruleRow.kind.name + " " + Theme.tr("to")
                            onEditingFinished: dlg.setRule(ruleRow.index, { max: text === "" ? null : Number(text) })
                        }
                    }
                }
            }
            Button {
                flat: true
                text: Theme.tr("Add rule")
                icon.source: Theme.icon("add")
                onClicked: dlg.rules = dlg.rules.concat([{ kind: "label", value: "", not: false }])
            }
            RowLayout {
                Layout.fillWidth: true
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("Order"); color: Theme.text; wrapMode: Text.WordWrap }
                ComboBox {
                    id: orderBox
                    Layout.fillWidth: true
                    Layout.maximumWidth: scroll.availableWidth * 0.7
                    model: dlg.orders.map(o => o.name)
                    Accessible.name: Theme.tr("Order")
                }
            }
            RowLayout {
                Layout.fillWidth: true
                Label { textFormat: Text.PlainText; Layout.fillWidth: true; text: Theme.tr("At most (songs, 0: all)"); color: Theme.text; wrapMode: Text.WordWrap }
                SpinBox {
                    id: limitField
                    from: 0
                    to: 5000
                    stepSize: 25
                    editable: true
                    Accessible.name: Theme.tr("At most (songs, 0: all)")
                    onValueModified: dlg.updatePreview()
                }
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: Theme.tr("%1 songs fit right now").arg(dlg.previewCount)
                color: Theme.textDim
                font.pixelSize: 13
            }
        }
    }
}
