import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Title, artist and the live spectrum over the visualizer. The spectrum
// itself is drawn by the core into the rectangle of `spectrumArea`.
Item {
    id: np
    property bool showSpectrum: true
    property bool showLyrics: false
    property bool controlsVisible: true
    // The controls are away: the shade reaches the window's edges.
    property bool shadeToEdges: false
    signal addLyrics()
    readonly property var cur: Core.state.current || null
    readonly property bool compact: width < 600 || height < 420

    // Keeps the title readable over bright pictures.
    Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.leftMargin: np.shadeToEdges ? -np.x : 0
        anchors.bottomMargin: np.shadeToEdges ? -(np.parent.height - np.y - np.height) : 0
        height: info.height + (np.compact ? 48 : 96)
        visible: np.cur !== null
        gradient: Gradient {
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 0.6; color: Qt.rgba(0, 0, 0, 0.35) }
            GradientStop { position: 1; color: Qt.rgba(0, 0, 0, 0.55) }
        }
    }

    ColumnLayout {
        id: info
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.leftMargin: np.compact ? 20 : 48
        anchors.rightMargin: np.compact ? 20 : 48
        anchors.bottomMargin: np.compact ? 12 : 36
        spacing: 6

        RowLayout {
            spacing: np.compact ? 14 : 22
            Layout.fillWidth: true
            CoverImage {
                visible: np.cur !== null
                Layout.preferredWidth: np.compact ? 72 : 112
                Layout.preferredHeight: np.compact ? 72 : 112
                radius: 12
                source: np.cur ? Covers.url(np.cur.id, np.cur.cover) : ""
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: np.cur ? np.cur.title : ""
                    color: "white"
                    font.pixelSize: np.compact ? 26 : 40
                    font.weight: Font.Bold
                    wrapMode: Text.WordWrap
                    maximumLineCount: 2
                    elide: Text.ElideRight
                    style: Text.Raised
                    styleColor: Qt.rgba(0, 0, 0, 0.35)
                }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    text: np.cur ? [np.cur.artist || Theme.tr("Unknown artist"), np.cur.album].filter(x => x).join(" · ") : ""
                    color: Qt.rgba(1, 1, 1, 0.82)
                    font.pixelSize: np.compact ? 15 : 19
                    elide: Text.ElideRight
                }
            }
            FavoriteButton {
                trackId: np.cur ? np.cur.id : ""
                iconColor: "white"
                iconSize: np.compact ? 26 : 30
            }
        }
        Item {
            id: spectrumArea
            Layout.fillWidth: true
            Layout.preferredHeight: np.compact ? 36 : 56
            Layout.maximumWidth: 1200
            visible: np.cur !== null && !np.overlayFull
        }
    }

    LyricsView {
        id: lyricsView
        anchors.left: info.left
        anchors.right: info.right
        anchors.top: parent.top
        anchors.bottom: info.top
        anchors.topMargin: np.compact ? 64 : 72
        anchors.bottomMargin: np.compact ? 16 : 28
        visible: np.showLyrics && np.cur !== null && !editor.visible
        controlsVisible: np.controlsVisible
        onAddLyrics: np.addLyrics()
        onEditTiming: (tap) => editor.open(lyricsView.info, tap)
    }

    LyricsEditor {
        id: editor
        anchors.fill: parent
        z: 50
    }

    // The overlay can fill the visual above the title.
    readonly property bool overlayFull: (Core.state.settings || {}).overlayFull === true
    Item {
        id: fullArea
        anchors.left: info.left
        anchors.right: info.right
        anchors.top: parent.top
        anchors.bottom: info.top
        anchors.topMargin: np.compact ? 56 : 64
        anchors.bottomMargin: 8
    }
    function updateOverlay() {
        if (!np.visible || !np.showSpectrum || np.opacity < 0.01 || (!np.overlayFull && !spectrumArea.visible)) {
            Core.call("ui.overlay", { x: 0, y: 0, w: 0, h: 0, alpha: 0 })
            return
        }
        const area = np.overlayFull ? fullArea : spectrumArea
        const p = area.mapToItem(null, 0, 0)
        Core.call("ui.overlay", { x: p.x, y: p.y, w: area.width, h: area.height, alpha: np.opacity })
    }
    Timer {
        // Geometry rarely changes; a cheap periodic sync also follows
        // animations of the surrounding layout.
        interval: 50
        running: true
        repeat: true
        onTriggered: np.updateOverlay()
    }
}
