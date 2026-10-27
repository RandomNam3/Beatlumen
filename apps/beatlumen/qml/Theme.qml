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
        "Disconnect": "Trennen",
        "Log in": "Anmelden",
        "Log out": "Abmelden",
        "Password": "Passwort",
        "Connected to": "Verbunden mit",
        "Connecting…": "Verbinde…",
        "Not connected": "Nicht verbunden",
        "Login required": "Anmeldung erforderlich",
        "Connection failed": "Verbindung fehlgeschlagen",
        "Only connections to this server are made. Use a VPN or reverse proxy with HTTPS outside your home network.": "Es wird nur zu diesem Server verbunden. Nutze außerhalb deines Heimnetzes ein VPN oder einen Reverse-Proxy mit HTTPS.",
        "Performance": "Leistung",
        "Render resolution": "Render-Auflösung",
        "About": "Über",
        "Licenses": "Lizenzen",
        "Choose folder": "Ordner wählen",
        "Use this folder": "Diesen Ordner verwenden",
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
