import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// Heart that marks a song as a favorite.
IconButton {
    id: b
    property string trackId
    readonly property bool on: Favorites.has(trackId)
    visible: Favorites.canMark(trackId)
    iconName: on ? "favorite" : "favorite_border"
    active: on
    tip: on ? Theme.tr("Remove from favorites") : Theme.tr("Add to favorites")
    Accessible.role: Accessible.CheckBox
    Accessible.checkable: true
    Accessible.checked: on
    onClicked: Favorites.toggle(trackId)
}
