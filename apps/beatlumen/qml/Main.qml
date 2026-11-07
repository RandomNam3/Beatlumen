import QtQuick
import QtQuick.Controls.Material
import QtQuick.Layouts
import Beatlumen.Core

// Main window: the visualizer fills the window (rendered by the core
// underneath this scene); navigation, pages and controls float on top.
ApplicationWindow {
    id: win
    width: 1280
    height: 800
    minimumWidth: 320
    minimumHeight: 360
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
    // -1: visualizer only, 0 library, 2 visuals, 3 settings, 4 devices,
    // 5 start page, 6 statistics, 7 search
    property int page: -1
    // The queue leaves the visual in sight: a panel at the side, on a narrow
    // window a sheet that covers only the lower part.
    property bool queueOpen: false
    onQueueOpenChanged: placeQueue()
    onCompactChanged: placeQueue()
    function placeQueue() {
        if (queueOpen && compact)
            queueSheet.open()
        else
            queueSheet.close()
    }
    function showAlbum(album) {
        queueOpen = false
        page = 0
        Qt.callLater(() => library.item && library.item.openAlbum(album))
    }
    function showArtist(name) {
        queueOpen = false
        page = 0
        Qt.callLater(() => library.item && library.item.openArtist(name))
    }
    // Where the player's button returns to from the visual.
    property int lastPage: 5
    function toggleVisual() {
        page = page >= 0 ? -1 : lastPage
    }
    function openSearch() {
        page = 7
        Qt.callLater(() => searchLoader.item && searchLoader.item.focusField())
    }
    // Little height: a phone held sideways.
    readonly property bool dense: height < 520
    // The big phone controls need room.
    readonly property bool bigPlayer: compact && page < 0 && height >= 560
    property bool idle: false
    readonly property bool showLyrics: !!(Core.state.settings && Core.state.settings.ui && Core.state.settings.ui.lyrics)
    readonly property bool chromeVisible: page >= 0 || queueOpen || !idle || !playing || Theme.lyricsEditing || Theme.keepControls

    Component.onCompleted: {
        if (startFullscreen)
            showFullScreen()
        // Songs given on the command line play with their visual right away.
        else if (!Core.call("state").launchedWithFiles)
            page = 5
        welcomeTimer.start()
        if (Core.platform === "android" && !(Core.state.settings && Core.state.settings.ui && Core.state.settings.ui.askedPermission)) {
            Core.call("settings.set", { key: "ui.askedPermission", value: true })
            Core.requestMediaPermission()
        }
    }
    // Android's status and navigation bars go with the controls (a swipe
    // from the edge shows them briefly).
    onChromeVisibleChanged: if (Core.platform === "android") Core.setSystemBarsHidden(!chromeVisible)
    // The keyboard brings the controls back like the mouse does.
    onActiveFocusItemChanged: {
        poke()
        focusRing.target = activeFocusItem
    }
    // Space and the arrow keys belong to a control reached by keyboard.
    readonly property bool keyOnControl: !!activeFocusItem && activeFocusItem.visualFocus === true
    readonly property bool playerKeys: !Theme.lyricsEditing && !keyOnControl
    onPageChanged: {
        if (page >= 0)
            lastPage = page
        Core.call("ui.blur", { value: page >= 0 ? 2.0 : 0.0 })
        poke()
    }

    property double lastPoke: 0
    function poke() {
        lastPoke = Date.now()
        idle = false
        idleTimer.restart()
    }
    function goBack() {
        if (queueOpen) {
            queueOpen = false
            return true
        }
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
    function showCollection(ext, id) {
        page = 0
        Qt.callLater(() => library.item && library.item.openCollection(ext, id))
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

    Shortcut { enabled: win.playerKeys; sequence: "Space"; onActivated: { win.poke(); Core.call("toggle") } }
    Shortcut { sequences: ["Media Play", "Toggle Media Play/Pause"]; onActivated: Core.call("toggle") }
    Shortcut { sequences: ["Ctrl+Right", "Media Next"]; onActivated: Core.call("next") }
    Shortcut { sequences: ["Ctrl+Left", "Media Previous"]; onActivated: Core.call("prev") }
    Shortcut { enabled: win.playerKeys; sequence: "Right"; onActivated: { win.poke(); Core.call("seekBy", { delta: 5 }) } }
    Shortcut { enabled: win.playerKeys; sequence: "Left"; onActivated: { win.poke(); Core.call("seekBy", { delta: -5 }) } }
    Shortcut { sequence: "Ctrl+Up"; onActivated: Core.call("volume", { value: Math.min(1, (Core.state.volume || 0) + 0.05) }) }
    Shortcut { sequence: "Ctrl+Down"; onActivated: Core.call("volume", { value: Math.max(0, (Core.state.volume || 0) - 0.05) }) }
    Shortcut { sequences: ["F11", "Alt+Return"]; onActivated: win.toggleFullscreen() }
    Shortcut { sequence: "Escape"; onActivated: if (!win.goBack() && win.visibility === Window.FullScreen) win.showNormal() }
    Shortcut { sequence: "Ctrl+F"; onActivated: win.openSearch() }
    Shortcut {
        sequence: "Ctrl+L"
        enabled: !!Core.state.current && Favorites.canMark(Core.state.current.id)
        onActivated: Favorites.toggle(Core.state.current.id)
    }
    Shortcut { enabled: !Theme.lyricsEditing; sequence: "Ctrl+Z"; onActivated: Core.call("preset.undo") }
    Shortcut { enabled: !Theme.lyricsEditing; sequence: "Ctrl+Shift+Z"; onActivated: Core.call("preset.redo") }
    Shortcut { sequence: "Ctrl+Alt+Right"; onActivated: Core.call("backgrounds.next") }
    Shortcut { sequence: "Ctrl+Alt+Left"; onActivated: Core.call("backgrounds.prev") }

    // ---- layout -------------------------------------------------------------
    NavBar {
        id: rail
        visible: !win.compact
        rail: true
        dense: win.dense
        anchors.left: parent.left
        anchors.top: parent.top
        anchors.bottom: playerBar.top
        width: implicitWidth
        current: win.page
        // The page floats over the visual: its entry closes it again.
        onSelected: (i) => win.page = i >= 0 && i === win.page ? -1 : i
        onShowHelp: help.open()
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.ms(400) } }
    }

    // Tapping the visual toggles the controls (and closes pages on phones).
    TapHandler {
        // Tapping the visual while the controls are shown hides them; the
        // MouseArea below brings them back.
        onTapped: {
            // Ignore the tap that just brought the controls back.
            if (win.page < 0 && win.playing && Date.now() - win.lastPoke > 600)
                win.idle = true
        }
        onDoubleTapped: if (!Core.mobile) win.toggleFullscreen()
    }

    // Visualizer controls in the top right corner.
    Row {
        anchors.top: parent.top
        anchors.right: queueSide.visible ? queueSide.left : parent.right
        anchors.margins: 8
        visible: win.page < 0
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.ms(400) } }
        IconButton {
            iconName: "lyrics"; iconColor: "white"
            active: win.showLyrics
            tip: win.showLyrics ? Theme.tr("Hide song text") : Theme.tr("Show song text")
            onClicked: Core.call("settings.set", { key: "ui.lyrics", value: !win.showLyrics })
        }
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
        anchors.right: queueSide.visible ? queueSide.left : parent.right
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        // Slides down when the controls fade out.
        anchors.bottomMargin: !win.chromeVisible ? 8
                              : win.bigPlayer ? expanded.height + nav.height
                              : win.compact ? playerBar.height + nav.height
                              : playerBar.height
        Behavior on anchors.bottomMargin { NumberAnimation { duration: Theme.ms(450); easing.type: Easing.OutCubic } }
        visible: win.page < 0
        showSpectrum: true
        showLyrics: win.showLyrics
        controlsVisible: win.chromeVisible
        onAddLyrics: dialogs.openLyrics()
    }

    // While the controls are hidden, a short tap anywhere brings them back
    // (and is not passed on to the invisible buttons below).
    MouseArea {
        anchors.fill: parent
        z: 90
        enabled: !win.chromeVisible
        onPressed: win.poke()
    }

    // Phone now-playing controls.
    PlayerBar {
        id: expanded
        mode: "expanded"
        visible: win.bigPlayer
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: nav.top
        height: implicitHeight
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.ms(400) } }
        onOpenQueue: win.queueOpen = true
    }

    // Page panel.
    Rectangle {
        id: panel
        visible: win.page >= 0
        color: Theme.surface
        anchors.left: win.compact ? parent.left : rail.right
        anchors.top: parent.top
        anchors.bottom: playerBar.top
        anchors.right: queueSide.visible ? queueSide.left : parent.right

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
                    onOpenFiles: dialogs.openAudio()
                    onExportPlaylist: (id) => dialogs.exportPlaylist(id)
                    onConnectServer: win.page = 3
                    onUploadVault: dialogs.openUpload()
                }
            }
            // Page 1 is free: the queue has its own place below.
            Item { }
            Loader {
                id: visuals
                active: win.page === 2 || item !== null
                sourceComponent: VisualsPage {
                    onAddImages: dialogs.openImages()
                    onAddImageFolder: win.addImageFolder()
                }
            }
            Loader {
                id: settingsLoader
                active: win.page === 3 || item !== null
                sourceComponent: SettingsPage {
                    onAddFolder: win.addFolder()
                    onLogin: loginPrompt.open()
                    onOpenVault: { win.page = 0; Qt.callLater(() => library.item && library.item.openVault()) }
                }
            }
            Loader {
                active: win.page === 4 || item !== null
                sourceComponent: DevicesPage {
                    onOpenSettings: win.page = 3
                }
            }
            Loader {
                active: win.page === 5 || item !== null
                sourceComponent: HomePage {
                    onStartTour: win.startTour()
                    onAddFolder: win.addFolder()
                    onOpenFiles: dialogs.openAudio()
                    onConnectServer: win.page = 3
                    onOpenAlbum: (album) => win.showAlbum(album)
                    onOpenFavorites: {
                        win.page = 0
                        Qt.callLater(() => library.item && library.item.openFavorites())
                    }
                    onOpenCollection: (ext, id) => win.showCollection(ext, id)
                }
            }
            Loader {
                active: win.page === 6 || item !== null
                sourceComponent: StatsPage { }
            }
            Loader {
                id: searchLoader
                active: win.page === 7 || item !== null
                sourceComponent: SearchPage {
                    onOpenAlbum: (album) => win.showAlbum(album)
                    onOpenArtist: (name) => win.showArtist(name)
                    onOpenLabel: (name) => {
                        win.page = 0
                        Qt.callLater(() => library.item && library.item.openLabel(name))
                    }
                    onOpenCollection: (ext, id) => win.showCollection(ext, id)
                }
            }
        }
    }

    // The queue as a side panel.
    Rectangle {
        id: queueSide
        visible: win.queueOpen && !win.compact
        color: Theme.surface
        anchors.top: parent.top
        anchors.bottom: playerBar.top
        anchors.right: parent.right
        width: Math.min(420, win.width * 0.42)
        Accessible.role: Accessible.Pane
        Accessible.name: Theme.tr("Queue")
        Rectangle {
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: 1
            color: Theme.outline
        }
        Loader {
            anchors.fill: parent
            anchors.margins: 8
            active: queueSide.visible
            sourceComponent: QueuePage { onCloseRequested: win.queueOpen = false }
        }
    }
    // The queue as a sheet.
    BottomSheet {
        id: queueSheet
        title: Theme.tr("Queue")
        maxShare: 0.72
        onClosed: if (win.compact) win.queueOpen = false
        Loader {
            anchors.fill: parent
            anchors.leftMargin: 4
            anchors.rightMargin: 4
            active: queueSheet.opened || queueSheet.visible
            sourceComponent: QueuePage { onCloseRequested: win.queueOpen = false }
        }
    }

    PlayerBar {
        id: playerBar
        mode: win.compact ? "mini" : "full"
        dense: win.dense
        showLine: !(win.showLyrics && win.page < 0)
        visible: !win.bigPlayer
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: win.compact ? nav.top : parent.bottom
        height: visible ? implicitHeight : 0
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.ms(400) } }
        onOpenNowPlaying: win.compact ? win.page = -1 : win.toggleVisual()
        onOpenQueue: win.queueOpen = !win.queueOpen
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
        onShowHelp: help.open()
        opacity: win.chromeVisible ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: Theme.ms(400) } }
    }

    // ---- toasts -------------------------------------------------------------
    Column {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: (win.bigPlayer ? expanded : playerBar).top
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
                    textFormat: Text.PlainText
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
    function addFolder() { dialogs.addMusicFolder() }
    function addImageFolder() { dialogs.addImageFolder() }
    PlatformDialogs { id: dialogs }
    FocusHighlight { id: focusRing }
    // A screen reader hears when another song starts.
    readonly property string currentTitle: Core.state.current ? Core.state.current.title + ", " + (Core.state.current.artist || "") : ""
    onCurrentTitleChanged: {
        if (currentTitle !== "" && Accessible.announce)
            Accessible.announce(Theme.tr("Now playing") + ": " + currentTitle)
    }
    ExtensionTarget { }
    HelpDialog {
        id: help
        onStartTour: win.startTour()
        onStartSetup: setup.open()
    }
    TextPrompt {
        id: loginPrompt
        title: Theme.tr("Log in")
        placeholder: Theme.tr("Password")
        password: true
        acceptText: Theme.tr("Log in")
        onSubmitted: (t) => Core.call("server.login", { password: t })
    }
    // Visualising other apps without access to their media sessions: the
    // title, cover and full control need it; offered once per start.
    // First start: the setup. Not over songs someone just opened.
    Timer {
        id: welcomeTimer
        interval: 600
        onTriggered: {
            const s = Core.state.settings || {}
            const ui = s.ui || {}
            const nothing = (Core.state.libraryTracks || 0) === 0 && (s.libraryFolders || []).length === 0
                && (Core.state.server || "disabled") === "disabled"
            if (!ui.setupDone && !ui.welcomed && nothing && Core.platform !== "web" && !Core.call("state").launchedWithFiles)
                setup.open()
        }
    }
    SetupWizard {
        id: setup
        onAddFolder: win.addFolder()
        onOpenSettings: (category) => {
            win.page = 3
            Qt.callLater(() => { if (settingsLoader.item) settingsLoader.item.category = category })
        }
        onStartTour: win.startTour()
    }
    Tour { id: tourPopup }
    function startTour() {
        win.page = 5
        win.poke()
        const bar = win.compact ? nav : rail
        tourPopup.start([
            { item: bar.button(0), title: Theme.tr("Home"), text: Theme.tr("What you last played, what is new, and suggestions.") },
            { item: bar.button(1), title: Theme.tr("Search"), text: Theme.tr("Find songs, albums and artists, or browse by genre and mood.") },
            { item: bar.button(2), title: Theme.tr("Library"), text: Theme.tr("All your music: songs, albums, artists, folders, playlists and labels.") },
            { item: bar.button(3), title: Theme.tr("Visuals"), text: Theme.tr("Styles, animations and backgrounds. The epilepsy mode is here too.") },
            { item: playerBar, title: Theme.tr("Player"), text: Theme.tr("Heart: favorite. Cover: the visual. Right: output, volume, queue.") },
            { item: bar.button(4), title: Theme.tr("More"), text: Theme.tr("Statistics, devices, settings and help. You can start this tour again under Help.") }
        ])
    }

    // Songs or a link shared with the app by another app.
    AppDialog {
        id: shareAsk
        readonly property var share: Core.state.share || null
        onShareChanged: share ? open() : close()
        onRejected: Core.call("share.dismiss")
        anchors.centerIn: Overlay.overlay
        width: Math.min(460, win.width - 32)
        title: Theme.tr("Shared with Beatlumen")
        standardButtons: Dialog.NoButton
        ColumnLayout {
            anchors.fill: parent
            spacing: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WrapAnywhere
                maximumLineCount: 4
                elide: Text.ElideRight
                color: Theme.textDim
                text: !shareAsk.share ? ""
                      : shareAsk.share.link ? shareAsk.share.link
                      : shareAsk.share.files.join(", ")
            }
            Flow {
                Layout.fillWidth: true
                layoutDirection: Qt.RightToLeft
                spacing: 4
                AccentButton { text: Theme.tr("Play now"); highlighted: true; onClicked: Core.call("share.play") }
                Button { text: Theme.tr("Add to queue"); flat: true; onClicked: Core.call("share.queue") }
                Button {
                    visible: shareAsk.share && shareAsk.share.canKeep === true
                    text: Theme.tr("Keep in my files")
                    flat: true
                    onClicked: Core.call("share.keep")
                }
                Button { text: Theme.tr("Cancel"); flat: true; onClicked: Core.call("share.dismiss") }
            }
        }
    }
    // A new DLNA control point wants to play here.
    AppDialog {
        id: dlnaAsk
        readonly property var asking: (Core.state.dlnaReceiver || {}).asking || null
        onAskingChanged: asking ? open() : close()
        closePolicy: Popup.NoAutoClose
        anchors.centerIn: Overlay.overlay
        width: Math.min(460, win.width - 32)
        title: Theme.tr("Allow this device to play here?")
        standardButtons: Dialog.NoButton
        ColumnLayout {
            anchors.fill: parent
            spacing: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: dlnaAsk.asking
                      ? Theme.tr("%1 at %2 wants to play music on this device (DLNA).").arg(dlnaAsk.asking.agent || Theme.tr("A device")).arg(dlnaAsk.asking.ip)
                      : ""
            }
            Flow {
                Layout.fillWidth: true
                layoutDirection: Qt.RightToLeft
                spacing: 4
                AccentButton { text: Theme.tr("Always allow"); highlighted: true; onClicked: Core.call("dlna.answer", { allow: true, always: true }) }
                Button { text: Theme.tr("Allow once"); flat: true; onClicked: Core.call("dlna.answer", { allow: true, always: false }) }
                Button { text: Theme.tr("Refuse"); flat: true; onClicked: Core.call("dlna.answer", { allow: false }) }
            }
        }
    }
    AppDialog {
        id: captureAccess
        property bool asked: false
        modal: true
        anchors.centerIn: Overlay.overlay
        width: Math.min(440, win.width - 32)
        title: Theme.tr("Control other apps?")
        standardButtons: Dialog.NoButton
        ColumnLayout {
            anchors.fill: parent
            spacing: 12
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: Theme.textDim
                text: Theme.tr("For the title, the cover and the play buttons here, Android needs notification access for Beatlumen. Notifications are not read.")
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button { text: Theme.tr("Later"); flat: true; onClicked: captureAccess.close() }
                AccentButton {
                    text: Theme.tr("Allow")
                    highlighted: true
                    onClicked: {
                        captureAccess.close()
                        Core.call("capture.sessionAccess")
                    }
                }
            }
        }
    }
    Timer {
        id: captureAccessCheck
        interval: 1500
        onTriggered: {
            const cap = Core.state.capture || {}
            if (cap.running === true && cap.access !== true && !captureAccess.asked) {
                captureAccess.asked = true
                captureAccess.open()
            }
        }
    }
    Connections {
        target: Core
        function onStateChanged() {
            // The web client always talks to its own server: ask for the
            // password right away.
            if (Core.platform === "web" && Core.state.server === "login" && !loginPrompt.opened)
                loginPrompt.open()
            const cap = Core.state.capture || {}
            if (cap.running === true && cap.access !== true && !captureAccess.asked && !captureAccessCheck.running)
                captureAccessCheck.start()
        }
    }
}
