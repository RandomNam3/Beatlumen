import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// What public databases know about an artist, on the artist's page. Only with
// the switch on; without it the page is what the library knows.
ColumnLayout {
    id: panel
    property string name
    // Who is in the library (their names are links).
    property var known: []
    property var info: ({})
    signal openArtist(string name)
    readonly property string state_: info.state || "off"
    readonly property var artist: info.artist || null
    // The picture, for the page's header.
    readonly property string image: artist ? (artist.image || "") : ""
    spacing: 10

    function refresh() {
        info = Core.call("artist.info", { name: panel.name }) || ({})
    }
    onNameChanged: refresh()
    Component.onCompleted: refresh()
    property int seenRev: -1
    Connections {
        target: Core
        function onStateChanged() {
            const rev = (Core.state.artists || {}).rev || 0
            const on = (Core.state.settings || {}).onlineMetadata === true ? 1 : 0
            if (rev * 2 + on === panel.seenRev)
                return
            panel.seenRev = rev * 2 + on
            panel.refresh()
        }
    }

    function kindText(kind) {
        switch (kind) {
        case "person": return Theme.tr("Person")
        case "group": return Theme.tr("Band")
        case "orchestra": return Theme.tr("Orchestra")
        case "choir": return Theme.tr("Choir")
        default: return ""
        }
    }
    function yearOf(date) {
        return (date || "").substring(0, 4)
    }
    function years(a) {
        const begin = yearOf(a.begin), end = yearOf(a.end)
        if (begin && end)
            return begin + " – " + end
        if (begin && !a.ended)
            return Theme.tr("since %1").arg(begin)
        return begin || end
    }
    function summary(a) {
        const place = [a.beginArea, a.area].filter((p, i, all) => p && all.indexOf(p) === i).join(", ")
        return [kindText(a.kind), place, years(a)].filter(x => x).join(" · ")
    }
    function people(list) {
        return (list || []).map(m => m.ended ? m.name + " (" + Theme.tr("until %1").arg(yearOf(m.end) || "?") + ")" : m.name).join(", ")
    }
    function linkName(kind) {
        switch (kind) {
        case "homepage": return Theme.tr("Website")
        case "bandcamp": return "Bandcamp"
        case "soundcloud": return "SoundCloud"
        case "youtube": return "YouTube"
        case "discogs": return "Discogs"
        default: return kind
        }
    }
    function inLibrary(n) {
        const wanted = (n || "").toLowerCase()
        return known.some(k => (k.name || "").toLowerCase() === wanted)
    }

    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: panel.state_ === "off"
        text: Theme.tr("More about artists from the internet can be switched on in the settings (Library & sources).")
        color: Theme.textFaint
        font.pixelSize: 13
        wrapMode: Text.WordWrap
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: panel.state_ === "pending" && panel.artist === null
        text: Theme.tr("Looking for information about this artist…")
        color: Theme.textDim
        font.pixelSize: 13
    }
    Label {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        visible: panel.state_ === "none"
        text: Theme.tr("Nothing certain was found about this artist.")
        color: Theme.textFaint
        font.pixelSize: 13
        wrapMode: Text.WordWrap
    }

    ColumnLayout {
        visible: panel.artist !== null
        Layout.fillWidth: true
        spacing: 10

        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: text !== ""
            text: panel.artist ? panel.summary(panel.artist) : ""
            color: Theme.text
            font.pixelSize: 14
            wrapMode: Text.WordWrap
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: panel.artist !== null && panel.artist.ambiguous === true
            text: Theme.tr("There are several artists of this name; this may be another one.")
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: text !== ""
            text: panel.artist ? panel.artist.genres.map(g => Theme.capitalized(g)).join(" · ") : ""
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
        Label {
            id: bio
            property bool open: false
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: text !== ""
            text: panel.artist ? panel.artist.biography : ""
            color: Theme.text
            font.pixelSize: 14
            lineHeight: 1.2
            wrapMode: Text.WordWrap
            maximumLineCount: open ? 1000 : 4
            elide: Text.ElideRight
        }
        Flow {
            Layout.fillWidth: true
            visible: bio.visible
            spacing: 4
            Button {
                flat: true
                visible: bio.truncated || bio.open
                text: bio.open ? Theme.tr("Show less") : Theme.tr("Show more")
                onClicked: bio.open = !bio.open
            }
            Button {
                flat: true
                visible: !!panel.artist && panel.artist.biographyUrl !== ""
                text: Theme.tr("Wikipedia (CC BY-SA)")
                onClicked: Qt.openUrlExternally(panel.artist.biographyUrl)
            }
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: !!panel.artist && panel.artist.members.length > 0
            text: Theme.tr("Members") + ": " + (panel.artist ? panel.people(panel.artist.members) : "")
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            visible: !!panel.artist && panel.artist.groups.length > 0
            text: Theme.tr("Part of") + ": " + (panel.artist ? panel.people(panel.artist.groups) : "")
            color: Theme.textDim
            font.pixelSize: 13
            wrapMode: Text.WordWrap
        }

        Label {
            textFormat: Text.PlainText
            visible: similarFlow.visible
            text: Theme.tr("Similar artists")
            color: Theme.text
            font.pixelSize: 14
            font.weight: Font.DemiBold
        }
        Flow {
            id: similarFlow
            Layout.fillWidth: true
            visible: !!panel.artist && panel.artist.similar.length > 0
            spacing: 6
            Repeater {
                model: panel.artist ? panel.artist.similar : []
                delegate: Button {
                    required property var modelData
                    readonly property bool mine: panel.inLibrary(modelData.name)
                    flat: true
                    // Only who is in the library leads anywhere.
                    enabled: mine
                    highlighted: mine
                    text: modelData.name
                    onClicked: panel.openArtist(modelData.name)
                }
            }
        }

        Flow {
            Layout.fillWidth: true
            visible: !!panel.artist && panel.artist.links.length > 0
            spacing: 4
            Repeater {
                model: panel.artist ? panel.artist.links : []
                delegate: Button {
                    required property var modelData
                    flat: true
                    text: panel.linkName(modelData.kind)
                    onClicked: Qt.openUrlExternally(modelData.url)
                }
            }
        }

        Button {
            flat: true
            visible: !!panel.artist && panel.artist.imagePage !== ""
            text: Theme.tr("Picture on Wikimedia Commons")
            onClicked: Qt.openUrlExternally(panel.artist.imagePage)
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: Theme.tr("Data: MusicBrainz, Wikidata, Wikipedia, ListenBrainz.")
                + (panel.artist && panel.artist.imageCredit ? " " + Theme.tr("Picture") + ": " + panel.artist.imageCredit : "")
            color: Theme.textFaint
            font.pixelSize: 12
            wrapMode: Text.WordWrap
        }
    }
}
