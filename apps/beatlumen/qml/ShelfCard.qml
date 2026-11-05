import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// A cover with two lines of text in a row of the start page. `hasPlay` adds
// a play button over the cover (the card itself opens something else then).
AbstractButton {
    id: card
    property string title
    property string subtitle
    // Why it is suggested (a third, quieter line).
    property string note
    property string cover
    property string seed: title
    property string placeholder: "music_note"
    property bool hasPlay: false
    property int side: 148
    signal playClicked()
    signal menuRequested(Item anchor)

    width: side
    implicitHeight: side + (note ? 76 : 58)
    padding: 0
    // Only the list takes focus by keyboard, clicks leave it alone.
    focusPolicy: Qt.NoFocus
    hoverEnabled: !Core.mobile

    Accessible.role: Accessible.Button
    Accessible.name: [title, subtitle, note].filter(x => x).join(", ")
    ToolTip.text: title
    ToolTip.visible: hovered && titleLabel.truncated
    ToolTip.delay: 600

    onPressAndHold: card.menuRequested(card)
    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: card.menuRequested(card)
    }
    Keys.onReturnPressed: card.clicked()
    Keys.onEnterPressed: card.clicked()
    Keys.onSpacePressed: card.clicked()
    Keys.onMenuPressed: card.menuRequested(card)

    contentItem: ColumnLayout {
        spacing: 8
        CoverImage {
            Layout.preferredWidth: card.side
            Layout.preferredHeight: card.side
            radius: 12
            source: card.cover
            seed: card.seed
            placeholder: card.placeholder
            Rectangle {
                anchors.fill: parent
                radius: 12
                color: "white"
                opacity: card.down ? 0.16 : card.hovered ? 0.08 : 0
            }
            RoundButton {
                visible: card.hasPlay && (card.hovered || card.activeFocus)
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                anchors.margins: 6
                implicitWidth: 44
                implicitHeight: 44
                padding: 0
                focusPolicy: Qt.NoFocus
                Material.background: Theme.accent
                Material.elevation: 2
                icon.source: Theme.icon("play_arrow")
                icon.color: Theme.onAccent
                icon.width: 26
                icon.height: 26
                display: AbstractButton.IconOnly
                Accessible.name: Theme.tr("Play")
                onClicked: card.playClicked()
            }
        }
        Label {
            id: titleLabel
            textFormat: Text.PlainText
            Layout.fillWidth: true
            text: card.title
            color: Theme.text
            font.pixelSize: 14
            font.weight: Font.Medium
            elide: Text.ElideRight
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.topMargin: -6
            text: card.subtitle
            color: Theme.textDim
            font.pixelSize: 13
            elide: Text.ElideRight
        }
        Label {
            textFormat: Text.PlainText
            Layout.fillWidth: true
            Layout.topMargin: -6
            visible: card.note !== ""
            text: card.note
            color: Theme.textDim
            font.pixelSize: 12
            font.italic: true
            elide: Text.ElideRight
        }
    }
    FocusRing {
        shown: card.activeFocus
        inset: -3
        radius: 14
    }
}
