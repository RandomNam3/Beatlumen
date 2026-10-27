import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Dialogs
import QtQuick.Layouts
import Beatlumen.Core

// Main window: the visualizer fills the window (rendered by the core
// underneath this scene); navigation, pages and controls float on top.
ApplicationWindow {
    id: win
    width: 1280
    height: 800
    minimumWidth: 320
    minimumHeight: 480
    visible: true
    title: Core.state.current ? Core.state.current.title + " – Beatlumen" : "Beatlumen"
    color: "black"
    background: null

    Material.theme: Theme.dark ? Material.Dark : Material.Light
    Material.accent: Theme.accent
    Material.primary: Theme.accent
    Material.foreground: Theme.text
    Material.background: Theme.surfaceStrong

    readonly property bool compact: width < 760
    readonly property bool playing: Core.state.play === "playing"
    // -1: visualizer only, 0 library, 1 queue, 2 visuals, 3 settings
    property int page: -1
    property bool idle: false
    readonly property bool chromeVisible: page >= 0 || !idle || !playing

    Component.onCompleted: {
        if (startFullscreen)
            showFullScreen()
        if (Core.platform === "android" && !(Core.state.settings && Core.state.settings.ui && Core.state.settings.ui.askedPermission)) {
            Core.call("settings.set", { key: "ui.askedPermission", value: true })
            Core.requestMediaPermission()
        }
    }
    onPageChanged: {
        Core.call("ui.blur", { value: page >= 0 ? 2.0 : 0.0 })
        poke()
    }

    function poke() {
        idle = false
        idleTimer.restart()
    }
    function goBack() {
        if (page === 0 && library.item && library.item.back())
            return true
        if (page === 2 && visuals.item && visuals.item.back())
            return true
        if (page >= 0) {
            page = -1
            return true
        }
        return false
    }
    function toggleFullscreen() {
        if (visibility === Window.FullScreen)
            showNormal()
        else
            showFullScreen()
    }

    Timer {
        id: idleTimer
        interval: 3000
        running: true
        onTriggered: win.idle = true
    }

    onClosing: (close) => {
        if (Core.mobile && goBack()) {
            close.accepted = false
            return
        }
        // Android back with nothing open while music plays: keep playing in
        // the background instead of quitting.
        if (Core.platform === "android" && (playing || Core.state.play === "loading") && Core.moveTaskToBack())
            close.accepted = false
    }

    // ---- global input -------------------------------------------------------
    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.NoButton
        hoverEnabled: true
        onPositionChanged: win.poke()
        z: 100
    }
    DropArea {
        anchors.fill: parent
        onDropped: (drop) => {
            if (drop.hasUrls)
                Core.openUrls(drop.urls)
        }
    }

    Shortcut { sequence: "Space"; onActivated: Core.call("toggle") }
    Shortcut { sequences: ["Media Play", "Toggle Media Play/Pause"]; onActivated: Core.call("toggle") }
    Shortcut { sequences: ["Ctrl+Right", "Media Next"]; onActivated: Core.call("next") }
    Shortcut { sequences: ["Ctrl+Left", "Media Previous"]; onActivated: Core.call("prev") }
    Shortcut { sequence: "Right"; onActivated: Core.call("seekBy", { delta: 5 }) }
    Shortcut { sequence: "Left"; onActivated: Core.call("seekBy", { delta: -5 }) }
    Shortcut { sequence: "Ctrl+Up"; onActivated: Core.call("volume", { value: Math.min(1, (Core.state.volume || 0) + 0.05) }) }
    Shortcut { sequence: "Ctrl+Down"; onActivated: Core.call("volume", { value: Math.max(0, (Core.state.volume || 0) - 0.05) }) }
    Shortcut { sequences: ["F11", "Alt+Return"]; onActivated: win.toggleFullscreen() }
    Shortcut { sequence: "Escape"; onActivated: if (!win.goBack() && win.visibility === Window.FullScreen) win.showNormal() }
    Shortcut { sequence: "Ctrl+F"; onActivated: win.page = 0 }
    Shortcut { sequence: "Ctrl+Z"; onActivated: Core.call("preset.undo") }
    Shortcut { sequence: "Ctrl+Shift+Z"; onActivated: Core.call("preset.redo") }
    Shortcut { sequence: "Ctrl+Alt+Right"; onActivated: Core.call("backgrounds.next") }
    Shortcut { sequence: "Ctrl+Alt+Left"; onActivated: Core.call("backgrounds.prev") }

    // ---- layout -------------------------------------------------------------
    NavBar {
        id: rail
        visible: !win.compact
        rail: true
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: playerBar.top
        width: 88
        current: win.page
        onSelected: (i) => win.page = i
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 400 } }
    }

    // Tapping the visual toggles the controls (and closes pages on phones).
    TapHandler {
        onTapped: {
            if (win.page >= 0 && win.compact)
                return
            win.idle = !win.idle
            if (!win.idle)
                idleTimer.restart()
        }
        onDoubleTapped: if (!Core.mobile) win.toggleFullscreen()
    }

    // Visualizer controls in the top right corner.
    Row {
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.margins: 8
        visible: win.page < 0 || !win.compact
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 400 } }
        IconButton {
            iconName: "wallpaper"; iconColor: "white"; tip: Theme.tr("Change background")
            onClicked: Core.call("backgrounds.next")
        }
        IconButton {
            visible: !Core.mobile
            iconName: win.visibility === Window.FullScreen ? "fullscreen_exit" : "fullscreen"
            iconColor: "white"
            tip: win.visibility === Window.FullScreen ? Theme.tr("Exit fullscreen") : Theme.tr("Fullscreen")
            onClicked: win.toggleFullscreen()
        }
    }

    NowPlaying {
        id: nowPlaying
        anchors.left: win.compact ? parent.left : rail.right
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.bottom: win.compact ? (win.page < 0 ? expanded.top : playerBar.top) : playerBar.top
        visible: win.page < 0
        showSpectrum: true
    }

    // Phone now-playing controls.
    PlayerBar {
        id: expanded
        mode: "expanded"
        visible: win.compact && win.page < 0
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: nav.top
        height: implicitHeight
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 400 } }
        onOpenQueue: win.page = 1
    }

    // Page panel.
    Rectangle {
        id: panel
        visible: win.page >= 0
        color: Theme.surface
        radius: win.compact ? 0 : Theme.radius
        anchors.left: win.compact ? parent.left : rail.right
        anchors.top: parent.top
        anchors.bottom: playerBar.top
        anchors.topMargin: win.compact ? 0 : 12
        anchors.bottomMargin: win.compact ? 0 : 12
        width: win.compact ? parent.width : Math.min(680, Math.max(420, (parent.width - rail.width) * 0.5))

        StackLayout {
            anchors.fill: parent
            anchors.margins: win.compact ? 4 : 8
            anchors.topMargin: 8
            currentIndex: Math.max(0, win.page)
            Loader {
                id: library
                active: win.page === 0 || item !== null
                sourceComponent: LibraryPage {
                    onAddFolder: win.addFolder()
                    onOpenFiles: audioDialog.open()
                    onConnectServer: win.page = 3
                }
            }
            Loader {
                active: win.page === 1 || item !== null
                sourceComponent: QueuePage { }
            }
            Loader {
                id: visuals
                active: win.page === 2 || item !== null
                sourceComponent: VisualsPage {
                    onAddImages: imageDialog.open()
                    onAddImageFolder: win.addImageFolder()
                }
            }
            Loader {
                active: win.page === 3 || item !== null
                sourceComponent: SettingsPage {
                    onAddFolder: win.addFolder()
                    onLogin: loginPrompt.open()
                }
            }
        }
    }

    PlayerBar {
        id: playerBar
        mode: win.compact ? "mini" : "full"
        visible: !win.compact || win.page >= 0
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: win.compact ? nav.top : parent.bottom
        height: visible ? implicitHeight : 0
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 400 } }
        onOpenNowPlaying: win.page = -1
        onOpenQueue: win.page = win.page === 1 ? -1 : 1
    }

    NavBar {
        id: nav
        visible: win.compact
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: visible ? implicitHeight : 0
        current: win.page
        onSelected: (i) => win.page = i
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 400 } }
    }

    // ---- toasts -------------------------------------------------------------
    Column {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: (win.compact ? (win.page < 0 ? expanded : playerBar) : playerBar).top
        anchors.bottomMargin: 12
        spacing: 8
        z: 50
        Repeater {
            model: ListModel { id: toasts }
            delegate: Rectangle {
                width: Math.min(win.width - 32, toastText.implicitWidth + 32)
                height: toastText.implicitHeight + 20
                radius: 12
                color: model.error ? Theme.danger : Theme.surfaceStrong
                Label {
                    id: toastText
                    anchors.centerIn: parent
                    width: Math.min(win.width - 64, implicitWidth)
                    text: model.text
                    color: model.error ? "white" : Theme.text
                    wrapMode: Text.WordWrap
                }
                Timer {
                    interval: model.error ? 6000 : 3500
                    running: true
                    onTriggered: toasts.remove(index)
                }
            }
        }
    }
    Connections {
        target: Core
        function onToast(text, error) {
            toasts.append({ text: text, error: error })
            if (toasts.count > 3)
                toasts.remove(0)
        }
    }

    // ---- dialogs -------------------------------------------------------------
    function addFolder() {
        if (Core.platform === "android")
            folderPicker.showFor(false)
        else
            folderDialog.open()
    }
    function addImageFolder() {
        if (Core.platform === "android")
            folderPicker.showFor(true)
        else
            imageFolderDialog.open()
    }

    FolderPicker {
        id: folderPicker
        property bool forImages: false
        function showFor(img) { forImages = img; images = img; show() }
        onChosen: (p) => forImages ? Core.call("backgrounds.addFolder", { path: p }) : Core.call("library.addFolder", { path: p })
    }
    FolderDialog {
        id: folderDialog
        title: Theme.tr("Add music folder")
        onAccepted: {
            const p = Core.localPaths([selectedFolder])
            if (p.length > 0)
                Core.call("library.addFolder", { path: p[0] })
        }
    }
    FolderDialog {
        id: imageFolderDialog
        title: Theme.tr("Add folder")
        onAccepted: {
            const p = Core.localPaths([selectedFolder])
            if (p.length > 0)
                Core.call("backgrounds.addFolder", { path: p[0] })
        }
    }
    FileDialog {
        id: audioDialog
        title: Theme.tr("Open files")
        fileMode: FileDialog.OpenFiles
        nameFilters: ["Audio (*.mp3 *.flac *.ogg *.oga *.opus *.m4a *.mp4 *.aac *.wav *.aif *.aiff *.mka *.webm *.mp2)", "Styles (*.hfx)", "* (*)"]
        onAccepted: Core.openUrls(selectedFiles)
    }
    FileDialog {
        id: imageDialog
        title: Theme.tr("Add images")
        fileMode: FileDialog.OpenFiles
        nameFilters: ["Images (*.jpg *.jpeg *.png *.webp *.gif *.bmp)"]
        onAccepted: {
            const paths = Core.localPaths(selectedFiles)
            Core.call("backgrounds.add", { sources: paths, show: true })
        }
    }
    TextPrompt {
        id: loginPrompt
        title: Theme.tr("Log in")
        placeholder: Theme.tr("Password")
        password: true
        acceptText: Theme.tr("Log in")
        onSubmitted: (t) => Core.call("server.login", { password: t })
    }
    Connections {
        target: Core
        function onStateChanged() {
            // The web client always talks to its own server: ask for the
            // password right away.
            if (Core.platform === "web" && Core.state.server === "login" && !loginPrompt.opened)
                loginPrompt.open()
        }
    }
}
