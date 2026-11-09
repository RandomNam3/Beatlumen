import QtQuick
import QtQuick.Layouts

// Lets the page a StackLayout switches to come in instead of appearing at
// once: it fades in and slides the last bit of the way, sideways in the
// direction of the switch (tabs) or up from below (pages). Only opacity and a
// transform move, nothing has to be laid out again.
//
//     StackLayout { id: stack; SwitchMotion { stack: stack } }
QtObject {
    id: motion
    property StackLayout stack
    // "x": from the side the new page lies on, "y": rising a little.
    property string axis: "x"
    property int previous: -1

    readonly property Translate shift: Translate { }
    readonly property ParallelAnimation run: ParallelAnimation {
        id: run
        property Item target
        NumberAnimation {
            target: run.target
            property: "opacity"
            from: 0
            to: 1
            duration: Theme.motion(Theme.normal)
            easing.type: Theme.easeOut
        }
        NumberAnimation {
            target: motion.shift
            property: motion.axis
            to: 0
            duration: Theme.motion(Theme.normal)
            easing.type: Theme.easeOut
        }
    }

    readonly property Connections watch: Connections {
        target: motion.stack
        function onCurrentIndexChanged() {
            const stack = motion.stack
            const index = stack.currentIndex
            const item = index >= 0 && index < stack.children.length ? stack.children[index] : null
            const from = motion.previous
            motion.previous = index
            if (!item || from < 0 || from === index)
                return
            run.stop()
            if (!Theme.animate) {
                item.opacity = 1
                motion.shift.x = 0
                motion.shift.y = 0
                return
            }
            // Fading only: photosensitive mode and the battery saver.
            const travel = Theme.slide ? (motion.axis === "x" ? 32 * (index > from ? 1 : -1) : 16) : 0
            motion.shift.x = motion.axis === "x" ? travel : 0
            motion.shift.y = motion.axis === "y" ? travel : 0
            item.transform = [motion.shift]
            item.opacity = 0
            run.target = item
            Theme.smoothFrames(Theme.normal)
            run.start()
        }
    }
    Component.onCompleted: previous = stack ? stack.currentIndex : -1
}
