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
