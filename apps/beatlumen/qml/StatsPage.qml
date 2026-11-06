import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// What you listened to: this device, or all devices if the server knows.
Item {
    id: page
    signal openLibrary()

    property string range: "month"
    // All devices: only the server knows that (with the user's agreement).
    readonly property bool canAll: Core.state.server === "connected" && (Core.state.settings || {}).historySync === true
    property bool allDevices: false
    readonly property bool useServer: canAll && allDevices
    property var stats: ({})
    property var library: ({})
    property bool loading: false

    // Seconds the local time is ahead of UTC.
    readonly property int utcOffset: -new Date().getTimezoneOffset() * 60

    function reload() {
        library = Core.call("stats.library") || {}
        if (useServer) {
            loading = true
            Core.call("stats.fetch", { range: range, utcOffset: utcOffset })
        } else {
            loading = false
            stats = Core.call("stats.overview", { range: range, utcOffset: utcOffset }) || {}
        }
    }
    Component.onCompleted: reload()
    onRangeChanged: reload()
    onUseServerChanged: reload()
    onVisibleChanged: if (visible) reload()
    readonly property int historyRev: Core.state.historyRev || 0
    onHistoryRevChanged: if (visible && !useServer) reload()
    readonly property int statsRev: Core.state.statsRev || 0
    onStatsRevChanged: {
        if (useServer) {
            loading = false
            stats = Core.call("stats.server") || {}
        }
    }

    readonly property var weekdays: [Theme.tr("Mon"), Theme.tr("Tue"), Theme.tr("Wed"), Theme.tr("Thu"), Theme.tr("Fri"), Theme.tr("Sat"), Theme.tr("Sun")]
    readonly property var timeline: stats.timeline || []
    function timelineLabel(t) {
        const p = String(t).split("-")
        // Days as day.month, months by their short name.
        if (p.length === 3)
            return Theme.language === "de" ? parseInt(p[2]) + "." + parseInt(p[1]) + "." : parseInt(p[1]) + "/" + parseInt(p[2])
        return Qt.locale(Theme.language).monthName(parseInt(p[1]) - 1, Locale.ShortFormat) + (parseInt(p[1]) === 1 ? " " + p[0] : "")
    }

    component Tile: Rectangle {
        property string value
        property string label
        Layout.fillWidth: true
        Layout.minimumWidth: 130
        implicitHeight: tileColumn.implicitHeight + 24
        radius: Theme.smallRadius
        color: Theme.surfaceVariant
        Accessible.role: Accessible.StaticText
        Accessible.name: label + ": " + value
        ColumnLayout {
            id: tileColumn
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            anchors.margins: 12
            spacing: 2
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: value
                color: Theme.text
                font.pixelSize: 26
                font.weight: Font.Bold
                elide: Text.ElideRight
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                text: label
                color: Theme.textDim
                font.pixelSize: 13
                wrapMode: Text.WordWrap
            }
        }
    }

    // A ranked list with quiet bars behind the names.
    component TopList: ColumnLayout {
        id: list
        property string title
        property var items: []
        property var name: (i) => i.name
        property var detail: (i) => ""
        readonly property real peak: items.length > 0 ? Math.max(1, ...items.map(i => i.plays)) : 1
        Layout.fillWidth: true
        Layout.alignment: Qt.AlignTop
        Layout.preferredWidth: 260
        visible: items.length > 0
        spacing: 2
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.bottomMargin: 4
            text: list.title
            color: Theme.text
            font.pixelSize: 15
            font.weight: Font.DemiBold
            elide: Text.ElideRight
        }
        Repeater {
            model: list.items
            Item {
                required property var modelData
                required property int index
                Layout.fillWidth: true
                implicitHeight: 44
                Accessible.role: Accessible.ListItem
                Accessible.name: (index + 1) + ". " + list.name(modelData) + ", "
                                 + Theme.tr("%1 plays").arg(modelData.plays) + ", " + Theme.fmtDuration(modelData.seconds)
                Rectangle {
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    anchors.topMargin: 2
                    anchors.bottomMargin: 2
                    width: parent.width * modelData.plays / list.peak
                    radius: 4
                    color: Qt.alpha(Theme.accent, 0.20)
                }
                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    spacing: 8
                    Label {
                        textFormat: Text.PlainText
                        text: index + 1
                        color: Theme.textDim
                        font.pixelSize: 13
                        font.features: { "tnum": 1 }
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            text: list.name(modelData)
                            color: Theme.text
                            font.pixelSize: 14
                            elide: Text.ElideRight
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            visible: text !== ""
                            text: list.detail(modelData)
                            color: Theme.textDim
                            font.pixelSize: 12
                            elide: Text.ElideRight
                        }
                    }
                    Label {
                        textFormat: Text.PlainText
                        text: modelData.plays + "×"
                        color: Theme.textDim
                        font.pixelSize: 13
                        font.features: { "tnum": 1 }
                    }
                }
            }
        }
    }

    ScrollView {
        id: scroll
        anchors.fill: parent
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        contentWidth: availableWidth
        clip: true

        ColumnLayout {
            width: scroll.availableWidth
            spacing: 18

            PageHeader {
                Layout.fillWidth: true
                title: Theme.tr("Statistics")
            }

            // Which period, whose plays.
            Flow {
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                spacing: 6
                Repeater {
                    model: [
                        { id: "week", label: Theme.tr("7 days") },
                        { id: "month", label: Theme.tr("30 days") },
                        { id: "year", label: Theme.tr("Year") },
                        { id: "all", label: Theme.tr("All time") }
                    ]
                    Chip {
                        required property var modelData
                        text: modelData.label
                        checked: page.range === modelData.id
                        onClicked: page.range = modelData.id
                    }
                }
                Item { width: 12; height: 1; visible: page.canAll }
                Chip {
                    visible: page.canAll
                    text: Theme.tr("This device")
                    checked: !page.allDevices
                    onClicked: page.allDevices = false
                }
                Chip {
                    visible: page.canAll
                    text: Theme.tr("All devices")
                    checked: page.allDevices
                    onClicked: page.allDevices = true
                }
            }

            EmptyState {
                Layout.fillWidth: true
                Layout.leftMargin: 16
                Layout.rightMargin: 24
                Layout.topMargin: 16
                visible: !page.loading && (page.stats.seconds || 0) === 0
                iconName: "bar_chart"
                title: Theme.tr("Nothing played in this period")
                text: Theme.tr("Statistics fill up as you listen. They stay on this device unless you allow otherwise.")
            }

            GridLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                visible: (page.stats.seconds || 0) > 0
                columns: Math.max(2, Math.floor(width / 170))
                columnSpacing: 10
                rowSpacing: 10
                Tile { value: Theme.fmtDuration(page.stats.seconds || 0); label: Theme.tr("Listening time") }
                Tile { value: String(page.stats.plays || 0); label: Theme.tr("Songs played") }
                Tile { value: String(page.stats.activeDays || 0); label: Theme.tr("Days with music") }
                Tile { value: String(page.stats.streak || 0); label: Theme.tr("Days in a row (best: %1)").arg(page.stats.longestStreak || 0) }
                Tile { value: String(page.stats.skips || 0); label: Theme.tr("Songs skipped") }
            }

            ColumnLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                visible: (page.stats.seconds || 0) > 0
                spacing: 18

                BarChart {
                    title: Theme.tr("Listening time over the period")
                    values: page.timeline.map(p => p.seconds)
                    labels: page.timeline.map(p => page.timelineLabel(p.t))
                    labelEvery: Math.max(1, Math.ceil(page.timeline.length / Math.max(1, Math.floor(width / 64))))
                }
                GridLayout {
                    Layout.fillWidth: true
                    columns: width > 720 ? 2 : 1
                    columnSpacing: 24
                    rowSpacing: 18
                    BarChart {
                        Layout.alignment: Qt.AlignTop
                        Layout.preferredWidth: 300
                        title: Theme.tr("By time of day")
                        values: page.stats.byHour || []
                        labels: Array.from({ length: 24 }, (_, h) => h + ":00")
                        labelEvery: width > 420 ? 3 : 6
                        barHeight: 90
                    }
                    BarChart {
                        Layout.alignment: Qt.AlignTop
                        Layout.preferredWidth: 300
                        title: Theme.tr("By day of the week")
                        values: page.stats.byWeekday || []
                        labels: page.weekdays
                        barHeight: 90
                    }
                }
                GridLayout {
                    Layout.fillWidth: true
                    columns: Math.max(1, Math.min(4, Math.floor(width / 280)))
                    columnSpacing: 24
                    rowSpacing: 18
                    TopList {
                        title: Theme.tr("Top songs")
                        items: page.stats.topSongs || []
                        name: (i) => i.title
                        detail: (i) => i.artist
                    }
                    TopList {
                        title: Theme.tr("Top artists")
                        items: page.stats.topArtists || []
                    }
                    TopList {
                        title: Theme.tr("Top albums")
                        items: page.stats.topAlbums || []
                        name: (i) => i.title
                        detail: (i) => i.artist
                    }
                    TopList {
                        title: Theme.tr("Top genres")
                        items: page.stats.topGenres || []
                    }
                }
            }

            // The library at a glance.
            ColumnLayout {
                Layout.fillWidth: true
                Layout.leftMargin: 12
                Layout.rightMargin: 12
                visible: (page.library.tracks || 0) > 0
                spacing: 8
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: Theme.tr("Your library")
                    color: Theme.text
                    font.pixelSize: 15
                    font.weight: Font.DemiBold
                }
                GridLayout {
                    Layout.fillWidth: true
                    columns: Math.max(2, Math.floor(width / 170))
                    columnSpacing: 10
                    rowSpacing: 10
                    Tile { value: String(page.library.tracks || 0); label: Theme.tr("Songs") }
                    Tile { value: String(page.library.albums || 0); label: Theme.tr("Albums") }
                    Tile { value: String(page.library.artists || 0); label: Theme.tr("Artists") }
                    Tile { value: Theme.fmtDuration(page.library.seconds || 0); label: Theme.tr("Total length") }
                    Tile { value: String(page.library.favorites || 0); label: Theme.tr("Favorites") }
                }
            }

            Item { Layout.preferredHeight: 8 }
        }
    }
}
