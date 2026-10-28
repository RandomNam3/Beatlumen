import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Title, artist and the live spectrum over the visualizer. The spectrum
// itself is drawn by the core into the rectangle of `spectrumArea`.
Item {
    id: np
    property bool showSpectrum: true
    readonly property var cur: Core.state.current || null
    readonly property bool compact: width < 600

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
        }
        Item {
            id: spectrumArea
            Layout.fillWidth: true
            Layout.preferredHeight: np.compact ? 36 : 56
            Layout.maximumWidth: 720
            visible: np.cur !== null
        }
    }

    function updateOverlay() {
        if (!spectrumArea.visible || !np.visible || !np.showSpectrum || np.opacity < 0.01) {
            Core.call("ui.overlay", { x: 0, y: 0, w: 0, h: 0, alpha: 0 })
            return
        }
        const p = spectrumArea.mapToItem(null, 0, 0)
        Core.call("ui.overlay", { x: p.x, y: p.y, w: spectrumArea.width, h: spectrumArea.height, alpha: np.opacity })
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
