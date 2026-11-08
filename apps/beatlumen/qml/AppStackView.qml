import QtQuick
import QtQuick.Controls

// A StackView whose pages come and go with the app's own motion: a pushed
// page slides in a little from the right and fades in, the page below fades
// back; going back does the same the other way. Without motion pages simply
// change, without sliding they only fade.
StackView {
    id: view
    readonly property real travel: Theme.slide ? Math.min(48, width * 0.08) : 0

    component Fade: NumberAnimation {
        property: "opacity"
        duration: Theme.motion(Theme.normal)
        easing.type: Theme.easeOut
    }
    component Move: NumberAnimation {
        property: "x"
        duration: Theme.motion(Theme.normal)
        easing.type: Theme.easeOut
    }

    onBusyChanged: if (busy) Theme.smoothFrames(Theme.normal)

    pushEnter: Theme.animate ? pushIn : null
    pushExit: Theme.animate ? pushOut : null
    popEnter: Theme.animate ? popIn : null
    popExit: Theme.animate ? popOut : null
    replaceEnter: Theme.animate ? pushIn : null
    replaceExit: Theme.animate ? pushOut : null

    readonly property Transition pushIn: Transition {
        Fade { from: 0; to: 1 }
        Move { from: view.travel; to: 0 }
    }
    readonly property Transition pushOut: Transition {
        Fade { from: 1; to: 0; duration: Theme.motion(Theme.fast) }
    }
    readonly property Transition popIn: Transition {
        Fade { from: 0; to: 1 }
        Move { from: -view.travel / 2; to: 0 }
    }
    readonly property Transition popOut: Transition {
        Fade { from: 1; to: 0; duration: Theme.motion(Theme.fast) }
        Move { from: 0; to: view.travel }
    }
}
