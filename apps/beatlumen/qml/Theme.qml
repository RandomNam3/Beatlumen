pragma Singleton
import QtQuick
import QtQuick.Controls.Material
import Beatlumen.Core

// Colours, metrics, icons and translations shared by all views.
QtObject {
    id: theme

    readonly property var settings: Core.state.settings || ({})
    readonly property var ui: settings.ui || ({})

    // "system", "light" or "dark".
    readonly property string mode: ui.theme || "system"
    readonly property bool dark: mode === "dark" || (mode === "system" && Core.systemDark)
    // "" = system accent, otherwise "#rrggbb".
    readonly property color accent: (ui.accent && ui.accent.length > 0) ? ui.accent : Core.systemAccent

    readonly property color surface: dark ? Qt.rgba(0.07, 0.07, 0.09, 0.86) : Qt.rgba(0.98, 0.97, 0.99, 0.90)
    readonly property color surfaceStrong: dark ? Qt.rgba(0.09, 0.09, 0.11, 0.97) : Qt.rgba(1, 1, 1, 0.98)
    readonly property color surfaceVariant: dark ? Qt.rgba(1, 1, 1, 0.06) : Qt.rgba(0, 0, 0, 0.05)
    readonly property color outline: dark ? Qt.rgba(1, 1, 1, 0.12) : Qt.rgba(0, 0, 0, 0.12)
    readonly property color text: dark ? "#f2f1f5" : "#1b1a20"
    readonly property color textDim: dark ? Qt.rgba(1, 1, 1, 0.66) : Qt.rgba(0, 0, 0, 0.62)
    readonly property color textFaint: dark ? Qt.rgba(1, 1, 1, 0.42) : Qt.rgba(0, 0, 0, 0.40)
    readonly property color danger: "#ef5350"
    // Text/icon colour on top of the accent colour.
    readonly property color onAccent: onColor(accent)

    // Black or white, whichever contrasts more with `c` (WCAG luminance).
    function onColor(c) {
        const lin = (v) => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)
        const l = 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
        return l > 0.179 ? "#1b1a20" : "#ffffff"
    }

    // A stable colour for a name (placeholders, avatars); `strength` from
    // 0 (background) to 1 (foreground).
    function seedColor(seed, strength) {
        let h = 0
        for (let i = 0; i < seed.length; ++i)
            h = (h * 31 + seed.charCodeAt(i)) % 360
        const l = dark ? 0.16 + 0.56 * strength : 0.90 - 0.50 * strength
        return Qt.hsla(h / 360, dark ? 0.42 : 0.50, l, 1)
    }

    // Touch friendly metrics (device independent pixels).
    readonly property int touch: 48
    readonly property int row: 64
    readonly property int gap: 16
    readonly property int radius: 16
    readonly property int smallRadius: 10

    function icon(name) {
        return "qrc:/qt/qml/Beatlumen/icons/" + name + ".svg"
    }

    function fmtTime(sec) {
        if (!(sec >= 0) || !isFinite(sec))
            sec = 0
        sec = Math.floor(sec)
        const h = Math.floor(sec / 3600)
        const m = Math.floor((sec % 3600) / 60)
        const s = sec % 60
        const ss = (s < 10 ? "0" : "") + s
        return h > 0 ? h + ":" + (m < 10 ? "0" : "") + m + ":" + ss : m + ":" + ss
    }

    // ---- translations (English source, German dictionary) ---------------
    readonly property string language: settings.language || Qt.locale().name.substring(0, 2)

    function tr(s) {
        if (language === "de") {
            const t = de[s]
            if (t !== undefined)
                return t
        }
        return s
    }

    readonly property var de: ({
        "Now playing": "Wiedergabe",
        "Sign in with a passkey": "Mit Passkey anmelden",
        "Opens the sign-in page in the browser, where passkeys, password managers and browser extensions can be used.": "Öffnet die Anmeldeseite im Browser, wo Passkeys, Passwortmanager und Browser-Erweiterungen genutzt werden können.",
        "Continue in the browser. It must show this code:": "Fahre im Browser fort. Er muss diesen Code zeigen:",
        "Passkeys and password": "Passkeys und Passwort",
        "Passkey or saved password": "Passkey oder gespeichertes Passwort",
        "Add": "Hinzufügen",
        "Unlocking…": "Entsperren…",
        "Use as background": "Als Hintergrund verwenden",
        "Shown as background": "Als Hintergrund gesetzt",
        "Encrypted images": "Verschlüsselte Bilder",
        "Playing on": "Wiedergabe auf",
        "Speaker for other devices": "Lautsprecher für andere Geräte",
        "Receive DLNA / UPnP": "DLNA / UPnP empfangen",
        "Other apps": "Andere Apps",
        "Control other apps?": "Andere Apps steuern?",
        "For the title, the cover and the play buttons here, Android needs notification access for Beatlumen. Notifications are not read.": "Für Titel, Cover und die Wiedergabe-Tasten hier braucht Beatlumen unter Android Zugriff auf Benachrichtigungen. Benachrichtigungen werden nicht gelesen.",
        "Search on": "Suchen auf",
        "Search failed": "Suche fehlgeschlagen",
        "In library": "In der Bibliothek",
        "Song": "Lied",
        "Lyric video": "Lyrics-Video",
        "Video": "Video",
        "Audio": "Audio",
        "Radio edit": "Radio Edit",
        "Where to save songs?": "Wohin speichern?",
        "The song plays right away while the server stores it. Your choice is remembered; you can change it in the settings.": "Das Lied spielt sofort, während der Server es speichert. Deine Wahl wird gemerkt; du kannst sie in den Einstellungen ändern.",
        "Library on the server": "Bibliothek auf dem Server",
        "Everyone who uses this server can play it.": "Alle, die diesen Server nutzen, können es abspielen.",
        "My encrypted files": "Meine verschlüsselten Dateien",
        "Only you (and whoever you share it with) can play it.": "Nur du (und mit wem du es teilst) kannst es abspielen.",
        "Unlock your account first (Settings → Account).": "Entsperre zuerst dein Konto (Einstellungen → Konto).",
        "Songs from": "Lieder von",
        "Save in": "Speichern in",
        "Ask": "Fragen",
        "Shows visuals for music that other apps play (Spotify, YouTube, radio apps, …) and controls them from here. Android asks for permission to record the screen each time; only the sound is used. Some apps do not allow recording, then only their title and controls are shown.": "Zeigt Effekte zur Musik, die andere Apps abspielen (Spotify, YouTube, Radio-Apps, …), und steuert sie von hier aus. Android fragt jedes Mal nach der Erlaubnis, den Bildschirm aufzunehmen; genutzt wird nur der Ton. Manche Apps erlauben keine Aufnahme, dann werden nur ihr Titel und die Steuerung angezeigt.",
        "Visualizing": "Visualisiert",
        "Stop": "Beenden",
        "Visualizing other apps": "Visualisiert andere Apps",
        "Visualizing and controlling": "Visualisiert und steuert",
        "Visualizing other apps; no playing app found to control": "Visualisiert andere Apps; keine spielende App zum Steuern gefunden",
        "Visualizing other apps; controlling them needs notification access": "Visualisiert andere Apps; zum Steuern fehlt der Zugriff auf Benachrichtigungen",
        "Visualize other apps": "Andere Apps visualisieren",
        "Stop visualizing other apps": "Andere Apps nicht mehr visualisieren",
        "Allow control of other apps": "Steuerung anderer Apps erlauben",
        "For titles, covers and controls, Android needs notification access for Beatlumen. Notifications are not read.": "Für Titel, Cover und Steuerung braucht Beatlumen unter Android Zugriff auf Benachrichtigungen. Benachrichtigungen werden nicht gelesen.",
        "Windows (\"Play to\"), TVs and apps like BubbleUPnP can send music to this device.": "Windows („Wiedergeben auf“), Fernseher und Apps wie BubbleUPnP können Musik an dieses Gerät senden.",
        "not available": "nicht verfügbar",
        "playing what a device sent": "spielt, was ein Gerät gesendet hat",
        "DLNA has no password.": "DLNA hat kein Passwort.",
        "Play on…": "Wiedergabe auf …",
        "This device": "Dieses Gerät",
        "Searching for speakers…": "Suche Lautsprecher …",
        "No speakers found": "Keine Lautsprecher gefunden",
        "Connecting…": "Verbinde …",
        "Password for": "Passwort für",
        "Stay unlocked on this device": "Auf diesem Gerät entsperrt bleiben",
        "The keys are kept in this device's protected storage, so you sign in only once here. Anyone who can use this device unlocked can then open your files.": "Die Schlüssel liegen im geschützten Speicher dieses Geräts, du meldest dich hier also nur einmal an. Wer das Gerät entsperrt benutzen kann, kann dann auch deine Dateien öffnen.",
        "Lock forgets the keys on this device; you then sign in again.": "„Sperren“ vergisst die Schlüssel auf diesem Gerät; danach meldest du dich neu an.",
        "Uses your password manager, e.g. Google Password Manager or Bitwarden.": "Nutzt deinen Passwortmanager, z. B. den Google Passwortmanager oder Bitwarden.",
        "Sign in in the browser": "Im Browser anmelden",
        "Add a passkey on this device": "Passkey auf diesem Gerät hinzufügen",
        "Passkey on this device": "Passkey auf diesem Gerät",
        "Your password manager stores the passkey; it then unlocks your encrypted files without the password.": "Dein Passwortmanager speichert den Passkey; er entsperrt deine verschlüsselten Dateien dann ohne Passwort.",
        "Notify me about shared files": "Bei neuen Freigaben benachrichtigen",
        "On, through UnifiedPush. Messages are end-to-end encrypted; no phone number or e-mail address is needed.": "Aktiv über UnifiedPush. Nachrichten sind Ende-zu-Ende-verschlüsselt; Telefonnummer oder E-Mail-Adresse werden nicht gebraucht.",
        "No UnifiedPush app found. Install one, e.g. ntfy (F-Droid or Play Store), then lock and unlock again.": "Keine UnifiedPush-App gefunden. Installiere eine, z. B. ntfy (F-Droid oder Play Store), dann sperren und erneut entsperren.",
        "Setting up…": "Wird eingerichtet…",
        "Failed:": "Fehlgeschlagen:",
        "Not available on this platform yet.": "Auf dieser Plattform noch nicht verfügbar.",
        "Web app on the server: unchanged official release": "Web-App auf dem Server: unverändertes offizielles Release",
        "Web app on the server: files match its manifest, but this app cannot check the release signature (unofficial build)": "Web-App auf dem Server: Dateien stimmen mit dem Manifest überein, aber diese App kann die Release-Signatur nicht prüfen (inoffizieller Build)",
        "Warning: the web app on the server was modified.": "Warnung: Die Web-App auf dem Server wurde verändert.",
        "Checking the web app…": "Web-App wird geprüft…",
        "Web app on the server: not checked yet": "Web-App auf dem Server: noch nicht geprüft",
        "Check": "Prüfen",
        "Accept the new key": "Neuen Schlüssel akzeptieren",
        "Account": "Konto",
        "Ask the other person to open your name in their list and compare the numbers.": "Bitte die andere Person, deinen Namen in ihrer Liste zu öffnen und die Nummern zu vergleichen.",
        "Change password": "Passwort ändern",
        "Compare the safety number with each person, in person or on a call. If both see the same number, nobody swapped your keys.": "Vergleiche die Sicherheitsnummer mit jeder Person, persönlich oder am Telefon. Sehen beide dieselbe Nummer, hat niemand eure Schlüssel ausgetauscht.",
        "Connect to a Beatlumen server in the settings first.": "Verbinde dich zuerst in den Einstellungen mit einem Beatlumen-Server.",
        "Copy": "Kopieren",
        "Create account": "Konto erstellen",
        "Current password": "Aktuelles Passwort",
        "Delete file?": "Datei löschen?",
        "Encrypted files": "Verschlüsselte Dateien",
        "Encrypting and uploading": "Verschlüsseln und hochladen:",
        "Enter the recovery code you wrote down when creating the account, and choose a new password.": "Gib den Wiederherstellungscode ein, den du beim Erstellen des Kontos notiert hast, und wähle ein neues Passwort.",
        "Files": "Dateien",
        "Files are encrypted on this device before they are uploaded. The server stores them but cannot read them.": "Dateien werden auf diesem Gerät verschlüsselt, bevor sie hochgeladen werden. Der Server speichert sie, kann sie aber nicht lesen.",
        "Forgot password": "Passwort vergessen",
        "I have written it down": "Ich habe ihn notiert",
        "In the browser, the protection depends on the server delivering an unmodified web app. For the best protection use the Beatlumen app.": "Im Browser hängt der Schutz davon ab, dass der Server die Web-App unverändert ausliefert. Den besten Schutz bietet die Beatlumen-App.",
        "Invitation code": "Einladungscode",
        "Key changed!": "Schlüssel geändert!",
        "Key fingerprint": "Schlüssel-Fingerabdruck",
        "Lock": "Sperren",
        "New password": "Neues Passwort",
        "New recovery code": "Neuer Wiederherstellungscode",
        "No encrypted files yet": "Noch keine verschlüsselten Dateien",
        "Nobody else has an account on this server yet.": "Noch niemand sonst hat ein Konto auf diesem Server.",
        "Not connected to a server": "Nicht mit einem Server verbunden",
        "Not verified": "Nicht überprüft",
        "Only you and the people you share with can read them": "Nur du und die Personen, mit denen du teilst, können sie lesen",
        "People": "Personen",
        "Recover account": "Konto wiederherstellen",
        "Recovery code": "Wiederherstellungscode",
        "Remove from my files": "Aus meinen Dateien entfernen",
        "Remove from my files?": "Aus meinen Dateien entfernen?",
        "Repeat password": "Passwort wiederholen",
        "Safety number with": "Sicherheitsnummer mit",
        "Share": "Teilen:",
        "Share…": "Teilen…",
        "Stop sharing with": "Nicht mehr teilen mit",
        "The file is deleted on the server for you and everyone you shared it with.": "Die Datei wird auf dem Server für dich und alle, mit denen du sie geteilt hast, gelöscht.",
        "The file key is encrypted for the person you choose; the server cannot read it.": "Der Dateischlüssel wird für die gewählte Person verschlüsselt; der Server kann ihn nicht lesen.",
        "The keys stay in memory only while unlocked. After a restart the password is needed again.": "Die Schlüssel liegen nur im Speicher, solange entsperrt ist. Nach einem Neustart wird das Passwort wieder gebraucht.",
        "The numbers match": "Die Nummern stimmen überein",
        "The passwords do not match": "Die Passwörter stimmen nicht überein",
        "The server now shows a different key for this person. This happens when they created a new account, or when someone tries to intercept shared files. Compare the safety number before you accept it.": "Der Server zeigt für diese Person jetzt einen anderen Schlüssel. Das passiert, wenn sie ein neues Konto erstellt hat – oder wenn jemand versucht, geteilte Dateien abzufangen. Vergleiche die Sicherheitsnummer, bevor du ihn akzeptierst.",
        "Unlock": "Entsperren",
        "Unlock or create account": "Entsperren oder Konto erstellen",
        "Unlocked as": "Entsperrt als",
        "Upload failed:": "Hochladen fehlgeschlagen:",
        "Upload files": "Dateien hochladen",
        "Upload not finished": "Hochladen nicht abgeschlossen",
        "User name": "Benutzername",
        "Verified": "Überprüft",
        "With an account, you store end-to-end encrypted files on the server and share them; the server cannot read them.": "Mit einem Konto speicherst du Ende-zu-Ende-verschlüsselte Dateien auf dem Server und teilst sie; der Server kann sie nicht lesen.",
        "Write this code down and keep it in a safe place. If you forget your password, it is the only way to get your files back. It is shown only once.": "Schreibe diesen Code auf und bewahre ihn sicher auf. Wenn du dein Passwort vergisst, ist er der einzige Weg zurück zu deinen Dateien. Er wird nur einmal angezeigt.",
        "You lose access; the owner keeps the file.": "Du verlierst den Zugriff; die Datei bleibt beim Besitzer.",
        "You need an invitation code from the server's admin. Choose a long password: nobody, not even the admin, can reset it without your recovery code.": "Du brauchst einen Einladungscode vom Admin des Servers. Wähle ein langes Passwort: Niemand, auch nicht der Admin, kann es ohne deinen Wiederherstellungscode zurücksetzen.",
        "Your password unlocks the keys on this device; it never leaves it.": "Dein Passwort entsperrt die Schlüssel auf diesem Gerät und verlässt es nie.",
        "Vault": "Tresor",
        "from": "von",
        "shared with": "geteilt mit",
        "Library": "Bibliothek",
        "Queue": "Warteschlange",
        "Visuals": "Visuals",
        "Settings": "Einstellungen",
        "Songs": "Titel",
        "Albums": "Alben",
        "Artists": "Interpreten",
        "Folders": "Ordner",
        "Playlists": "Playlists",
        "Search music": "Musik durchsuchen",
        "Play": "Abspielen",
        "Pause": "Pause",
        "Shuffle": "Zufällig",
        "Shuffle all": "Alle zufällig",
        "Play all": "Alle abspielen",
        "Play next": "Als Nächstes",
        "Add to queue": "Zur Warteschlange",
        "Add to playlist…": "Zur Playlist hinzufügen…",
        "Remove": "Entfernen",
        "Remove from queue": "Aus Warteschlange entfernen",
        "Clear": "Leeren",
        "No results": "Keine Treffer",
        "Receive AirPlay": "AirPlay empfangen",
        "This device appears as a speaker on iPhone, iPad and Mac; what they play is shown with visuals here.": "Dieses Gerät erscheint auf iPhone, iPad und Mac als Lautsprecher; was dort läuft, wird hier mit Visuals wiedergegeben.",
        "AirPlay is not available": "AirPlay ist nicht verfügbar",
        "Receiving from": "Empfängt von",
        "Ready, visible as": "Bereit, sichtbar als",
        "Starting…": "Wird gestartet…",
        "Speaker name": "Lautsprechername",
        "Password set (enter a new one to change it)": "Passwort gesetzt (zum Ändern ein neues eingeben)",
        "Password (optional)": "Passwort (optional)",
        "Set password": "Passwort setzen",
        "Remove password": "Passwort entfernen",
        "Only reachable in the local network. Without a password anyone in this network can play music on this device.": "Nur im lokalen Netzwerk erreichbar. Ohne Passwort kann jeder in diesem Netzwerk Musik auf diesem Gerät abspielen.",
        "Try a different title, artist or album.": "Versuche einen anderen Titel, Interpreten oder ein anderes Album.",
        "Collect songs with “Add to playlist…” in a song's menu.": "Sammle Titel über „Zur Playlist hinzufügen…“ im Menü eines Titels.",
        "Clear queue": "Warteschlange leeren",
        "Save as playlist": "Als Playlist speichern",
        "Unknown artist": "Unbekannter Interpret",
        "Unknown album": "Unbekanntes Album",
        "songs": "Titel",
        "song": "Titel",
        "albums": "Alben",
        "album": "Album",
        "Nothing playing": "Nichts wird abgespielt",
        "Pick something from your library": "Wähle etwas aus deiner Bibliothek",
        "Your library is empty": "Deine Bibliothek ist leer",
        "Add a music folder or connect to a Beatlumen server.": "Füge einen Musikordner hinzu oder verbinde dich mit einem Beatlumen-Server.",
        "Add music folder": "Musikordner hinzufügen",
        "Open files": "Dateien öffnen",
        "Connect server": "Server verbinden",
        "Scanning library…": "Bibliothek wird eingelesen…",
        "The queue is empty": "Die Warteschlange ist leer",
        "Songs you play or add appear here.": "Titel, die du abspielst oder hinzufügst, erscheinen hier.",
        "No playlists yet": "Noch keine Playlists",
        "New playlist": "Neue Playlist",
        "Playlist name": "Name der Playlist",
        "Create": "Erstellen",
        "Cancel": "Abbrechen",
        "Save": "Speichern",
        "Delete": "Löschen",
        "Rename": "Umbenennen",
        "OK": "OK",
        "Back": "Zurück",
        "More": "Mehr",
        "Effects": "Effekte",
        "Backgrounds": "Hintergründe",
        "Style": "Stil",
        "Intensity": "Intensität",
        "Calm": "Ruhig",
        "Wild": "Wild",
        "Adaptive": "Adaptiv",
        "Sync with the music": "Synchron zur Musik",
        "Later": "Später",
        "Earlier": "Früher",
        "Move the effects earlier if they come after the beat (e.g. with Bluetooth headphones).": "Effekte früher stellen, wenn sie nach dem Beat kommen (z. B. mit Bluetooth-Kopfhörern).",
        "Effects follow the energy of the song: calm in quiet parts, strong in drops.": "Effekte folgen der Energie des Songs: ruhig in leisen Passagen, kräftig bei Drops.",
        "Overlay": "Overlay",
        "Spectrum": "Spektrum",
        "Waveform": "Wellenform",
        "Off": "Aus",
        "Advanced editor": "Erweiterter Editor",
        "Customize every effect of the current style.": "Passe jeden Effekt des aktuellen Stils an.",
        "Built-in": "Integriert",
        "Mine": "Eigene",
        "Server": "Server",
        "Save style": "Stil speichern",
        "Save style as…": "Stil speichern unter…",
        "Style name": "Name des Stils",
        "Undo": "Rückgängig",
        "Redo": "Wiederholen",
        "Add effect": "Effekt hinzufügen",
        "Enabled": "Aktiv",
        "Duplicate": "Duplizieren",
        "Move up": "Nach oben",
        "Move down": "Nach unten",
        "Analysis": "Analyse",
        "Reset to defaults": "Standardwerte",
        "Energy": "Energie",
        "Beats (lows · mids · highs)": "Beats (Tiefen · Mitten · Höhen)",
        "Sections": "Abschnitte",
        "No analysis yet": "Noch keine Analyse",
        "Analysing…": "Wird analysiert…",
        "Add images": "Bilder hinzufügen",
        "Add folder": "Ordner hinzufügen",
        "Upload to server": "Auf Server hochladen",
        "Server images": "Server-Bilder",
        "Change background": "Hintergrund wechseln",
        "Manually": "Manuell",
        "When a song ends": "Wenn ein Titel endet",
        "On every song change": "Bei jedem Titelwechsel",
        "Use album cover when empty": "Albumcover verwenden, wenn leer",
        "No backgrounds yet. The album cover of the current song is used.": "Noch keine Hintergründe. Das Albumcover des aktuellen Titels wird verwendet.",
        "Appearance": "Darstellung",
        "Theme": "Design",
        "System": "System",
        "Light": "Hell",
        "Dark": "Dunkel",
        "Accent colour": "Akzentfarbe",
        "Language": "Sprache",
        "Music folders": "Musikordner",
        "Rescan": "Neu einlesen",
        "Server address": "Serveradresse",
        "Connect": "Verbinden",
        "The server's certificate is not trusted by this device. Only trust it if this fingerprint (SHA-256) matches the certificate of your server:": "Das Zertifikat des Servers ist auf diesem Gerät nicht vertrauenswürdig. Vertraue ihm nur, wenn dieser Fingerabdruck (SHA-256) mit dem Zertifikat deines Servers übereinstimmt:",
        "Trust this certificate": "Diesem Zertifikat vertrauen",
        "Warning: this address is outside your home network and the connection is not encrypted. Use https:// or a VPN.": "Achtung: Diese Adresse liegt außerhalb deines Heimnetzes und die Verbindung ist nicht verschlüsselt. Nutze https:// oder ein VPN.",
        "Disconnect": "Trennen",
        "Log in": "Anmelden",
        "Log out": "Abmelden",
        "Password": "Passwort",
        "Connected to": "Verbunden mit",
        "Connecting…": "Verbinde…",
        "Not connected": "Nicht verbunden",
        "Login required": "Anmeldung erforderlich",
        "Connection failed": "Verbindung fehlgeschlagen",
        "Only connections to this server are made. Outside your home network, use an https:// address or a VPN.": "Es wird nur zu diesem Server verbunden. Nutze außerhalb deines Heimnetzes eine https://-Adresse oder ein VPN.",
        "Performance": "Leistung",
        "Render resolution": "Render-Auflösung",
        "About": "Über",
        "Licenses": "Lizenzen",
        "Source code": "Quellcode",
        "Playback": "Wiedergabe",
        "Normalize volume": "Lautstärke angleichen",
        "Crossfade": "Überblendung",
        "The next song fades in while the current one fades out.": "Der nächste Titel wird eingeblendet, während der aktuelle ausklingt.",
        "Skip silence": "Stille überspringen",
        "Moves on to the next song as soon as the current one has ended, without silent gaps.": "Wechselt zum nächsten Titel, sobald der aktuelle zu Ende ist – ohne stille Pausen.",
        "Plays all songs at a similar loudness. Each song is measured once when it is first played.": "Spielt alle Titel ähnlich laut ab. Jeder Titel wird beim ersten Abspielen einmal vermessen.",
        "Copy link": "Link kopieren",
        "Choose folder": "Ordner wählen",
        "Use this folder": "Diesen Ordner verwenden",
        "Select folder": "Auswählen",
        "Allow access to music": "Zugriff auf Musik erlauben",
        "Beatlumen needs access to your music and pictures to show your library.": "Beatlumen braucht Zugriff auf deine Musik und Bilder, um deine Bibliothek anzuzeigen.",
        "Allow": "Erlauben",
        "Fullscreen": "Vollbild",
        "Exit fullscreen": "Vollbild beenden",
        "Volume": "Lautstärke",
        "Mute": "Stumm",
        "Repeat": "Wiederholen",
        "Next": "Weiter",
        "Previous": "Zurück",
        "Open": "Öffnen",
        "Show all": "Alle anzeigen",
        "tracks": "Titel"
    })
}
