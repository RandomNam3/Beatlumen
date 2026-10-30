// Beatlumen server administration. Plain DOM, no external code; texts in
// English and German (browser language). Talks to /api/admin/* with the
// session cookie and the X-Beatlumen-Admin header.
"use strict";
(() => {
  const DE = (navigator.language || "").toLowerCase().startsWith("de");
  const GERMAN = {
    "Admin": "Verwaltung",
    "Log in": "Anmelden",
    "Admin password": "Admin-Passwort",
    "Wrong password": "Falsches Passwort",
    "Set the admin password": "Admin-Passwort festlegen",
    "Choose the password for managing this server. Use at least 12 characters.": "Wähle das Passwort für die Verwaltung dieses Servers. Mindestens 12 Zeichen.",
    "New password": "Neues Passwort",
    "Repeat the password": "Passwort wiederholen",
    "The passwords differ": "Die Passwörter stimmen nicht überein",
    "Use at least 12 characters": "Mindestens 12 Zeichen verwenden",
    "Set password": "Passwort festlegen",
    "No admin password yet": "Noch kein Admin-Passwort",
    "Open the setup link from the server log (valid for a day after the first start), or create a new one on the server:": "Öffne den Einrichtungslink aus dem Server-Log (einen Tag nach dem ersten Start gültig) oder erstelle auf dem Server einen neuen:",
    "In a container:": "In einem Container:",
    "Forgot the password? Create a setup link the same way.": "Passwort vergessen? Erstelle auf dieselbe Weise einen Einrichtungslink.",
    "Overview": "Übersicht",
    "Library": "Bibliothek",
    "Accounts": "Konten",
    "Shared links": "Geteilte Links",
    "Song texts": "Songtexte",
    "Extensions": "Erweiterungen",
    "Network": "Netzwerk",
    "Security": "Sicherheit",
    "Log": "Protokoll",
    "Log out": "Abmelden",
    "Some changes take effect after a restart.": "Einige Änderungen werden erst nach einem Neustart wirksam.",
    "Restart now": "Jetzt neu starten",
    "Restart the server by hand to apply them.": "Starte den Server von Hand neu, um sie zu übernehmen.",
    "Restarting…": "Startet neu …",
    "Saved": "Gespeichert",
    "Saved. Restart to apply.": "Gespeichert. Neustart nötig.",
    "Save": "Speichern",
    "Saving…": "Speichert …",
    "Tracks": "Titel",
    "Albums": "Alben",
    "Folders": "Ordner",
    "Scanning…": "Durchsucht …",
    "Up to date": "Aktuell",
    "Rescan": "Neu durchsuchen",
    "Rescan started": "Durchsuchen gestartet",
    "Users": "Benutzer",
    "Public links": "Öffentliche Links",
    "App logins": "App-Anmeldungen",
    "Devices online": "Geräte online",
    "Speech recognition": "Spracherkennung",
    "Off": "Aus",
    "Automatic": "Automatisch",
    "Timed songs": "Synchronisierte Titel",
    "In the queue": "In der Warteschlange",
    "Working on a song": "Arbeitet an einem Titel",
    "Downloads": "Downloads",
    "Storage": "Speicher",
    "System": "System",
    "Version": "Version",
    "Running for": "Läuft seit",
    "Data folder": "Datenordner",
    "Listen address": "Adresse",
    "Web client": "Web-Client",
    "Integrity": "Integrität",
    "covers": "Cover",
    "analysis": "Analysen",
    "images": "Bilder",
    "uploads": "Hochgeladen",
    "lyrics": "Songtexte",
    "vault": "Verschlüsselte Dateien",
    "models": "Sprachmodelle",
    "imports": "Importe",
    "The server has no password: everyone in the network can use the library.": "Der Server hat kein Passwort: Jeder im Netzwerk kann die Bibliothek nutzen.",
    "The server password is short.": "Das Server-Passwort ist kurz.",
    "No music folders are set up.": "Es sind keine Musikordner eingerichtet.",
    "The speech model file is missing.": "Die Datei des Sprachmodells fehlt.",
    "Files of the web client were changed: the server does not serve them.": "Dateien des Web-Clients wurden verändert: Der Server liefert sie nicht aus.",
    "Fix": "Beheben",
    "Server name": "Servername",
    "Shown in the apps.": "Wird in den Apps angezeigt.",
    "Music folders": "Musikordner",
    "Folders on the server; subfolders are included.": "Ordner auf dem Server; Unterordner gehören dazu.",
    "Background image folders": "Ordner mit Hintergrundbildern",
    "Rescan interval": "Abstand zwischen Durchsuchungen",
    "0 = only when asked.": "0 = nur auf Anfrage.",
    "Scan threads": "Threads beim Durchsuchen",
    "0 = automatic.": "0 = automatisch.",
    "Longest song analysed": "Längster analysierter Titel",
    "0 = no limit.": "0 = keine Grenze.",
    "New accounts": "Neue Konten",
    "With an invitation": "Mit Einladung",
    "Storage per account": "Speicher pro Konto",
    "0 = unlimited.": "0 = unbegrenzt.",
    "Extra host names": "Weitere Hostnamen",
    "Domains the server is reached by (e.g. of a reverse proxy). IP addresses and local names always work.": "Domains, über die der Server erreicht wird (z. B. eines Reverse-Proxys). IP-Adressen und lokale Namen gehen immer.",
    "Public HTTPS address": "Öffentliche HTTPS-Adresse",
    "As users reach the server, e.g. https://music.example.org. Turns on passkeys.": "So wie Benutzer den Server erreichen, z. B. https://music.example.org. Schaltet Passkeys ein.",
    "Android app certificates": "Android-App-Zertifikate",
    "SHA-256 fingerprints of other builds of the app that may use passkeys.": "SHA-256-Fingerabdrücke anderer App-Builds, die Passkeys nutzen dürfen.",
    "Push services in the local network": "Push-Dienste im lokalen Netzwerk",
    "Allow UnifiedPush servers in private networks and over plain http.": "UnifiedPush-Server in privaten Netzen und über unverschlüsseltes http erlauben.",
    "Source code link": "Link zum Quellcode",
    "Required by the AGPL when you run a modified version for others.": "Pflicht nach der AGPL, wenn du eine veränderte Version für andere betreibst.",
    "Source extensions: a name and the address the extension listens on.": "Quellen-Erweiterungen: ein Name und die Adresse der Erweiterung.",
    "Name": "Name",
    "Address": "Adresse",
    "Add": "Hinzufügen",
    "Remove": "Entfernen",
    "Times the song texts of all songs in the background, on half of the processor cores. Songs already done by the same or a better model are skipped.": "Synchronisiert die Songtexte aller Songs im Hintergrund, mit der Hälfte der Prozessorkerne. Songs, die schon mit demselben oder einem besseren Modell erkannt wurden, werden übersprungen.",
    "Time the whole library": "Ganze Bibliothek synchronisieren",
    "Detailed log": "Ausführliches Protokoll",
    "Logs every request.": "Protokolliert jede Anfrage.",
    "Set by %1": "Festgelegt durch %1",
    "Takes effect after a restart.": "Wirkt nach einem Neustart.",
    "seconds": "Sekunden",
    "minutes": "Minuten",
    "Invitations": "Einladungen",
    "New accounts need an invitation code. Each code works once.": "Neue Konten brauchen einen Einladungscode. Jeder Code gilt einmal.",
    "Create invitation": "Einladung erstellen",
    "Valid for": "Gültig für",
    "1 day": "1 Tag",
    "7 days": "7 Tage",
    "30 days": "30 Tage",
    "Invitation code": "Einladungscode",
    "Give this code to the new user. It is shown only now.": "Gib diesen Code der neuen Person. Er wird nur jetzt angezeigt.",
    "Copy": "Kopieren",
    "Copied": "Kopiert",
    "Close": "Schließen",
    "Open invitations": "Offene Einladungen",
    "No open invitations.": "Keine offenen Einladungen.",
    "Expires %1": "Läuft ab: %1",
    "Revoke": "Widerrufen",
    "Revoked": "Widerrufen",
    "No accounts yet.": "Noch keine Konten.",
    "Created": "Erstellt",
    "Files": "Dateien",
    "Passkeys": "Passkeys",
    "Delete": "Löschen",
    "Delete account": "Konto löschen",
    "This deletes the account %1 with all its encrypted files and links. It cannot be undone. Type the name to confirm:": "Damit wird das Konto %1 mit allen verschlüsselten Dateien und Links gelöscht. Das lässt sich nicht rückgängig machen. Gib zur Bestätigung den Namen ein:",
    "Cancel": "Abbrechen",
    "Deleted": "Gelöscht",
    "Links to encrypted files that anyone with the link can open. The server cannot read the files.": "Links zu verschlüsselten Dateien, die jeder mit dem Link öffnen kann. Der Server kann die Dateien nicht lesen.",
    "No public links.": "Keine öffentlichen Links.",
    "Owner": "Besitzer",
    "Expires": "Läuft ab",
    "Never": "Nie",
    "Password": "Passwort",
    "Yes": "Ja",
    "No": "Nein",
    "With a speech model the server times plain song texts by recognising the sung words, and writes down what is sung when a song has no text. Everything runs on this server.": "Mit einem Sprachmodell synchronisiert der Server einfache Songtexte, indem er die gesungenen Wörter erkennt, und schreibt auf, was gesungen wird, wenn ein Titel keinen Text hat. Alles läuft auf diesem Server.",
    "Models": "Modelle",
    "Models are downloaded from %1 when picked and checked against their checksum.": "Modelle werden beim Auswählen von %1 heruntergeladen und mit ihrer Prüfsumme geprüft.",
    "In use": "In Verwendung",
    "Downloaded": "Heruntergeladen",
    "Use": "Verwenden",
    "Download and use": "Herunterladen und verwenden",
    "Checking…": "Wird geprüft …",
    "Failed: %1": "Fehlgeschlagen: %1",
    "Try again": "Erneut versuchen",
    "Turn off": "Ausschalten",
    "Speech recognition is off.": "Spracherkennung ist aus.",
    "%1 of memory": "%1 Arbeitsspeicher",
    "fastest, rough": "am schnellsten, grob",
    "fast, small": "schnell, klein",
    "fast": "schnell",
    "good for songs, recommended": "gut für Songs, empfohlen",
    "good for songs": "gut für Songs",
    "better, slow": "besser, langsam",
    "best, slow": "am besten, langsam",
    "Status": "Status",
    "Not timed": "Nicht synchronisiert",
    "Try failed songs again": "Fehlgeschlagene Titel erneut versuchen",
    "%1 songs will be tried again": "%1 Titel werden erneut versucht",
    "The model is loaded": "Das Modell ist geladen",
    "The model loads with the next song": "Das Modell wird mit dem nächsten Titel geladen",
    "Extensions run as separate programs; the server only connects to the addresses set here.": "Erweiterungen laufen als eigene Programme; der Server verbindet sich nur mit den hier eingetragenen Adressen.",
    "Change the admin password": "Admin-Passwort ändern",
    "Current password": "Aktuelles Passwort",
    "Change password": "Passwort ändern",
    "Password changed": "Passwort geändert",
    "The admin password is set by a file on the server.": "Das Admin-Passwort wird durch eine Datei auf dem Server festgelegt.",
    "Server password": "Server-Passwort",
    "Apps and the web client log in with it.": "Damit melden sich die Apps und der Web-Client an.",
    "A password is set.": "Ein Passwort ist gesetzt.",
    "No password is set: everyone who reaches the server can use the library.": "Kein Passwort gesetzt: Jeder, der den Server erreicht, kann die Bibliothek nutzen.",
    "Changing it logs out every app.": "Eine Änderung meldet alle Apps ab.",
    "Remove password": "Passwort entfernen",
    "Remove the server password? Everyone who reaches the server can then use the library.": "Server-Passwort entfernen? Dann kann jeder, der den Server erreicht, die Bibliothek nutzen.",
    "Server password set": "Server-Passwort gesetzt",
    "Server password removed": "Server-Passwort entfernt",
    "End all app logins": "Alle App-Anmeldungen beenden",
    "%1 apps are logged in with the server password.": "%1 Apps sind mit dem Server-Passwort angemeldet.",
    "All apps must log in again. Continue?": "Alle Apps müssen sich neu anmelden. Fortfahren?",
    "Logins ended": "Anmeldungen beendet",
    "Filter": "Filter",
    "Refresh": "Aktualisieren",
    "Something went wrong": "Etwas ist schiefgegangen",
    "Confirm": "Bestätigen",
    "Accounts with storage are end-to-end encrypted: the admin sees names and sizes, never contents.": "Konten mit Speicher sind Ende-zu-Ende verschlüsselt: Die Verwaltung sieht Namen und Größen, nie Inhalte.",
    "Settings": "Einstellungen",
    "Not reachable": "Nicht erreichbar",
    "Music sources": "Musikquellen",
    "Songs on other servers and storage become part of the library: the server lists them, analyses them and plays them for the apps. The files stay where they are.": "Songs auf anderen Servern und Speichern werden Teil der Bibliothek: Der Server listet sie, analysiert sie und spielt sie für die Apps ab. Die Dateien bleiben, wo sie sind.",
    "Sources": "Quellen",
    "Add source": "Quelle hinzufügen",
    "Edit source": "Quelle bearbeiten",
    "Kind": "Art",
    "Status": "Status",
    "Songs": "Songs",
    "Sync now": "Jetzt abgleichen",
    "Edit": "Bearbeiten",
    "Remove": "Entfernen",
    "Removed": "Entfernt",
    "Remove source": "Quelle entfernen",
    "Remove %1? Its songs leave the library; the files stay on the source.": "%1 entfernen? Die Songs verschwinden aus der Bibliothek; die Dateien bleiben auf der Quelle.",
    "No sources yet.": "Noch keine Quellen.",
    "Listing… %1": "Wird gelistet … %1",
    "Listed %1": "Gelistet %1",
    "Waiting": "Wartet",
    "Address": "Adresse",
    "User name": "Benutzername",
    "Password": "Passwort",
    "Unchanged": "Unverändert",
    "Folder": "Ordner",
    "Only songs below this folder; empty: all.": "Nur Songs unterhalb dieses Ordners; leer: alle.",
    "Server key": "Server-Schlüssel",
    "SHA-256 fingerprint of the SSH host key. Empty: the key seen at the first connection is kept.": "SHA-256-Fingerabdruck des SSH-Host-Schlüssels. Leer: Der Schlüssel der ersten Verbindung wird behalten.",
    "Turned on": "Eingeschaltet",
    "Media servers in the network": "Medienserver im Netzwerk",
    "Search": "Suchen",
    "Searching…": "Suche läuft …",
    "No media servers found.": "Keine Medienserver gefunden.",
    "Use": "Verwenden",
    "Connect": "Verbinden",
    "Connecting…": "Verbinde …",
    "Cancel": "Abbrechen",
    "Added": "Hinzugefügt",
    "Log in with Nextcloud": "Mit Nextcloud anmelden",
    "Open the Nextcloud page and allow access. This page notices when you are done.": "Öffne die Nextcloud-Seite und erlaube den Zugriff. Diese Seite merkt, wenn du fertig bist.",
    "Open Nextcloud": "Nextcloud öffnen",
    "Waiting for Nextcloud…": "Warte auf Nextcloud …",
    "Navidrome, Gonic, Airsonic and other Subsonic servers. The password is never sent; an API key works as \"apikey:<key>\".": "Navidrome, Gonic, Airsonic und andere Subsonic-Server. Das Passwort wird nie gesendet; ein API-Schlüssel geht als „apikey:<Schlüssel>“.",
    "The music libraries of a Jellyfin server.": "Die Musikbibliotheken eines Jellyfin-Servers.",
    "A folder in Nextcloud. You log in on the Nextcloud page; the server gets an app password of its own.": "Ein Ordner in Nextcloud. Du meldest dich auf der Nextcloud-Seite an; der Server bekommt ein eigenes App-Passwort.",
    "A folder on a WebDAV share (ownCloud, NAS, …). Tags are read from the files once and kept.": "Ein Ordner auf einer WebDAV-Freigabe (ownCloud, NAS, …). Tags werden einmal aus den Dateien gelesen und gemerkt.",
    "A folder on a computer reachable with SSH. Password or private key (OpenSSH format).": "Ein Ordner auf einem per SSH erreichbaren Rechner. Passwort oder privater Schlüssel (OpenSSH-Format).",
    "A media server in the local network (MiniDLNA, Plex, Serviio, …).": "Ein Medienserver im lokalen Netzwerk (MiniDLNA, Plex, Serviio, …).",
    "Private key": "Privater Schlüssel",
    "Compare this key with the server's (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub), then connect again to trust it:": "Vergleiche diesen Schlüssel mit dem des Servers (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub) und verbinde dann erneut, um ihm zu vertrauen:",
    "Unknown server key": "Unbekannter Server-Schlüssel",
    "Instead of a password (OpenSSH format, without passphrase).": "Statt eines Passworts (OpenSSH-Format, ohne Passphrase).",
    "DLNA media server": "DLNA-Medienserver",
    "Web folder": "Web-Ordner",
    "A folder on a web server with a directory listing, or a link to one audio file. Only links below this address are followed.": "Ein Ordner auf einem Webserver mit Verzeichnisliste oder ein Link auf eine Audiodatei. Nur Links unterhalb dieser Adresse werden verfolgt.",
  };
  const t = (s, ...a) => {
    let r = (DE && GERMAN[s]) || s;
    a.forEach((v, i) => { r = r.replace("%" + (i + 1), v); });
    return r;
  };
  document.documentElement.lang = DE ? "de" : "en";

  // ---- helpers ----------------------------------------------------------
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") el.className = v;
      // The style object, not the attribute (the page's CSP has no inline styles).
      else if (k === "style") el.style.cssText = v;
      else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else if (k === "value") el.value = v;
      else if (k === "checked") el.checked = !!v;
      else el.setAttribute(k, v === true ? "" : String(v));
    }
    for (const k of kids.flat()) {
      if (k === null || k === undefined || k === false) continue;
      el.append(k instanceof Node ? k : document.createTextNode(String(k)));
    }
    return el;
  }
  const app = document.getElementById("app");
  // replaceChildren() would turn null into the text "null".
  const put = (el, ...kids) => el.replaceChildren(...kids.flat().filter((k) => k !== null && k !== undefined && k !== false));
  // Where a fixed setting comes from, in the page's language.
  const origin = (o) => !DE ? o : o.replace("environment variable", "Umgebungsvariable").replace("command line", "Kommandozeile").replace("configuration file", "Konfigurationsdatei");
  function bytes(n) {
    if (!n) return "0 B";
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1000 && i < u.length - 1) { n /= 1000; i++; }
    return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + " " + u[i];
  }
  const fmt = new Intl.NumberFormat(DE ? "de" : "en");
  const date = (s) => s ? new Date(s * 1000).toLocaleString(DE ? "de" : "en", { dateStyle: "medium", timeStyle: "short" }) : "–";
  function duration(s) {
    const d = Math.floor(s / 86400), hh = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60);
    return d ? `${d} d ${hh} h` : hh ? `${hh} h ${m} min` : `${m} min`;
  }
  let toastTimer = 0;
  function toast(text, error) {
    document.querySelector(".toast")?.remove();
    const el = h("div", { class: "toast" + (error ? " error" : ""), role: "status" }, text);
    document.body.append(el);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.remove(), 3500);
  }
  class ApiError extends Error {
    constructor(status, msg) { super(msg); this.status = status; }
  }
  async function api(method, path, body) {
    const opts = { method, credentials: "same-origin", cache: "no-store", headers: { "X-Beatlumen-Admin": "1" } };
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    const r = await fetch("/api/admin/" + path, opts);
    if (r.status === 401 && !["login", "state", "setup"].includes(path)) {
      showLogin();
      throw new ApiError(401, t("Log in"));
    }
    const text = await r.text();
    let v = null;
    try { v = text ? JSON.parse(text) : null; } catch (_) { v = null; }
    if (!r.ok) throw new ApiError(r.status, (v && v.error) || r.statusText || t("Something went wrong"));
    return v;
  }
  function fail(e) { toast(e.message || t("Something went wrong"), true); }
  function confirmDialog(title, text, okLabel, needText) {
    return new Promise((resolve) => {
      const input = needText ? h("input", { type: "text", autocomplete: "off", "aria-label": title }) : null;
      const ok = h("button", { class: "btn primary danger" }, okLabel || t("Confirm"));
      const d = h("dialog", {},
        h("h3", {}, title), h("p", {}, text), input,
        h("div", { class: "actions" }, h("button", { class: "btn", onclick: () => d.close("cancel") }, t("Cancel")), ok));
      ok.addEventListener("click", () => {
        if (needText && input.value.trim() !== needText) { input.focus(); return; }
        d.close("ok");
      });
      d.addEventListener("close", () => { d.remove(); resolve(d.returnValue === "ok"); });
      document.body.append(d);
      d.showModal();
      (input || ok).focus();
    });
  }

  // "ggml-small-q5_1.bin" -> "small (q5_1)"
  const modelName = (path) => path.split(/[\\/]/).pop().replace(/^ggml-/, "").replace(/\.bin$/, "")
    .replace(/-(q\d_\d)$/, " ($1)").replace("large-v3-turbo", "large v3 turbo");

  // ---- state ------------------------------------------------------------
  let info = { name: "Beatlumen Server", version: "" };
  let timer = 0;
  let restartPending = false;
  let canRestart = false;

  function brand(sub) {
    return h("div", { class: "brand" }, h("div", { class: "logo", "aria-hidden": "true" }),
      h("div", {}, h("h1", {}, info.name), h("small", {}, sub || t("Admin"))));
  }

  // ---- login and setup ---------------------------------------------------
  function passwordInput(id, auto) {
    return h("input", { type: "password", id, autocomplete: auto, required: true, minlength: 12, maxlength: 1024 });
  }
  function showLogin() {
    clearInterval(timer);
    const pw = h("input", { type: "password", id: "pw", autocomplete: "current-password", required: true });
    const status = h("div", { class: "status" });
    const btn = h("button", { class: "btn primary wide", type: "submit" }, t("Log in"));
    const form = h("form", { onsubmit: async (e) => {
      e.preventDefault();
      btn.disabled = true;
      status.textContent = "";
      try {
        await api("POST", "login", { password: pw.value });
        start();
      } catch (err) {
        status.className = "status error";
        status.textContent = err.status === 401 ? t("Wrong password") : err.message;
        btn.disabled = false;
        pw.select();
      }
    } }, h("label", { for: "pw" }, t("Admin password")), pw, status, btn);
    put(app, h("div", { class: "center" }, h("div", { class: "box" }, brand(), form)));
    pw.focus();
  }
  function showSetup(token) {
    clearInterval(timer);
    if (!token) {
      put(app, h("div", { class: "center" }, h("div", { class: "box" }, brand(),
        h("h2", {}, t("No admin password yet")),
        h("p", { class: "hint" }, t("Open the setup link from the server log (valid for a day after the first start), or create a new one on the server:")),
        h("pre", { class: "secret mono" }, "beatlumen-server admin-link"),
        h("p", { class: "hint" }, t("In a container:")),
        h("pre", { class: "secret mono" }, "docker exec beatlumen /beatlumen-server admin-link"))));
      return;
    }
    const a = passwordInput("pw1", "new-password"), b = passwordInput("pw2", "new-password");
    const status = h("div", { class: "status" });
    const btn = h("button", { class: "btn primary wide", type: "submit" }, t("Set password"));
    const form = h("form", { onsubmit: async (e) => {
      e.preventDefault();
      status.className = "status error";
      if ([...a.value].length < 12) { status.textContent = t("Use at least 12 characters"); return; }
      if (a.value !== b.value) { status.textContent = t("The passwords differ"); return; }
      btn.disabled = true;
      try {
        await api("POST", "setup", { token, password: a.value });
        history.replaceState(null, "", location.pathname + "#overview");
        start();
      } catch (err) {
        status.textContent = err.message;
        btn.disabled = false;
      }
    } },
    h("h2", {}, t("Set the admin password")),
    h("p", { class: "hint" }, t("Choose the password for managing this server. Use at least 12 characters.")),
    h("label", { for: "pw1" }, t("New password")), a,
    h("label", { for: "pw2" }, t("Repeat the password")), b, status, btn);
    put(app, h("div", { class: "center" }, h("div", { class: "box" }, brand(), form)));
    a.focus();
  }

  // ---- settings fields ----------------------------------------------------
  const DEFS = {
    name: { label: "Server name", type: "text", hint: "Shown in the apps." },
    music: { label: "Music folders", type: "list", hint: "Folders on the server; subfolders are included.", placeholder: "/music" },
    images: { label: "Background image folders", type: "list", placeholder: "/images" },
    rescanInterval: { label: "Rescan interval", type: "number", unit: "seconds", hint: "0 = only when asked." },
    scanThreads: { label: "Scan threads", type: "number", hint: "0 = automatic." },
    analysisMaxMinutes: { label: "Longest song analysed", type: "number", unit: "minutes", hint: "0 = no limit." },
    registration: { label: "New accounts", type: "select", options: [["invite", "With an invitation"], ["off", "Off"]] },
    vaultQuotaMb: { label: "Storage per account", type: "number", unit: "MiB", hint: "0 = unlimited." },
    allowedHosts: { label: "Extra host names", type: "list", hint: "Domains the server is reached by (e.g. of a reverse proxy). IP addresses and local names always work.", placeholder: "music.example.org" },
    publicUrl: { label: "Public HTTPS address", type: "text", hint: "As users reach the server, e.g. https://music.example.org. Turns on passkeys.", placeholder: "https://", nullable: true },
    androidCerts: { label: "Android app certificates", type: "list", hint: "SHA-256 fingerprints of other builds of the app that may use passkeys." },
    pushAllowPrivate: { label: "Push services in the local network", type: "switch", hint: "Allow UnifiedPush servers in private networks and over plain http." },
    sourceUrl: { label: "Source code link", type: "text", hint: "Required by the AGPL when you run a modified version for others." },
    extensions: { label: "Extensions", type: "map", hint: "Source extensions: a name and the address the extension listens on." },
    autoSyncLyrics: { label: "Time the whole library", type: "switch", hint: "Times the song texts of all songs in the background, on half of the processor cores. Songs already done by the same or a better model are skipped." },
    verbose: { label: "Detailed log", type: "switch", hint: "Logs every request." },
  };

  function listEditor(values, placeholder, disabled) {
    const wrap = h("div", { class: "list-edit" });
    const add = (v) => {
      const input = h("input", { type: "text", value: v, placeholder: placeholder || "", disabled });
      const row = h("div", { class: "row" }, input,
        h("button", { class: "btn small", type: "button", hidden: disabled, onclick: () => row.remove(), "aria-label": t("Remove") }, "×"));
      wrap.insertBefore(row, addBtn);
    };
    const addBtn = h("button", { class: "btn small add", type: "button", hidden: disabled, onclick: () => add("") }, "+ " + t("Add"));
    wrap.append(addBtn);
    (values || []).forEach(add);
    if ((!values || !values.length) && !disabled) add("");
    wrap.read = () => [...wrap.querySelectorAll("input")].map((i) => i.value.trim()).filter(Boolean);
    return wrap;
  }
  function mapEditor(obj, disabled) {
    const wrap = h("div", { class: "list-edit" });
    const add = (k, v) => {
      const id = h("input", { type: "text", value: k, placeholder: t("Name"), disabled, style: "max-width:10rem" });
      const url = h("input", { type: "url", value: v, placeholder: "http://127.0.0.1:7790", disabled });
      const row = h("div", { class: "row" }, id, url,
        h("button", { class: "btn small", type: "button", hidden: disabled, onclick: () => row.remove(), "aria-label": t("Remove") }, "×"));
      wrap.insertBefore(row, addBtn);
    };
    const addBtn = h("button", { class: "btn small add", type: "button", hidden: disabled, onclick: () => add("", "") }, "+ " + t("Add"));
    wrap.append(addBtn);
    Object.entries(obj || {}).forEach(([k, v]) => add(k, v));
    wrap.read = () => {
      const o = {};
      for (const row of wrap.querySelectorAll(".row")) {
        const [a, b] = row.querySelectorAll("input");
        if (a.value.trim() && b.value.trim()) o[a.value.trim().toLowerCase()] = b.value.trim();
      }
      return o;
    };
    return wrap;
  }
  // A form for some settings; `all` is the list from GET settings.
  function settingsForm(all, keys, after) {
    const fields = [];
    const section = h("div", { class: "section" });
    for (const key of keys) {
      const s = all.find((x) => x.key === key);
      const d = DEFS[key];
      if (!s || !d) continue;
      const locked = !!s.lockedBy;
      const id = "set-" + key;
      let control, read;
      if (d.type === "text") {
        control = h("input", { type: "text", id, value: s.value ?? "", placeholder: d.placeholder, disabled: locked });
        read = () => d.nullable && !control.value.trim() ? null : control.value.trim();
      } else if (d.type === "number") {
        control = h("div", { class: "row" }, h("input", { type: "number", id, min: 0, value: s.value, disabled: locked, style: "max-width:10rem" }),
          d.unit ? h("span", { class: "hint" }, t(d.unit)) : null);
        read = () => Math.max(0, parseInt(control.querySelector("input").value || "0", 10));
      } else if (d.type === "switch") {
        const cb = h("input", { type: "checkbox", id, checked: !!s.value, disabled: locked, role: "switch" });
        control = h("label", { class: "switch" }, cb);
        read = () => cb.checked;
      } else if (d.type === "select") {
        control = h("select", { id, disabled: locked }, d.options.map(([v, l]) => h("option", { value: v, selected: s.value === v }, t(l))));
        read = () => control.value;
      } else if (d.type === "list") {
        control = listEditor(s.value, d.placeholder, locked);
        read = () => control.read();
      } else if (d.type === "map") {
        control = mapEditor(s.value, locked);
        read = () => control.read();
      }
      fields.push({ key, read, initial: JSON.stringify(s.value ?? null), locked });
      section.append(h("div", { class: "field" },
        h("div", {}, h("label", { for: id }, t(d.label)),
          d.hint ? h("p", { class: "hint" }, t(d.hint)) : null,
          locked ? h("p", { class: "locked" }, t("Set by %1", origin(s.lockedBy))) : null,
          !s.live && !locked ? h("p", { class: "hint faint" }, t("Takes effect after a restart.")) : null),
        h("div", {}, control)));
    }
    const btn = h("button", { class: "btn primary", type: "button" }, t("Save"));
    btn.addEventListener("click", async () => {
      const values = {};
      for (const f of fields) {
        if (f.locked) continue;
        const v = f.read();
        if (JSON.stringify(v) !== f.initial) values[f.key] = v;
      }
      if (!Object.keys(values).length) { toast(t("Saved")); return; }
      btn.disabled = true;
      btn.textContent = t("Saving…");
      try {
        const r = await api("PUT", "settings", { values });
        restartPending = r.restartPending;
        toast(r.restartPending ? t("Saved. Restart to apply.") : t("Saved"));
        for (const f of fields) if (f.key in values) f.initial = JSON.stringify(values[f.key]);
        renderBanner();
        after && after();
      } catch (e) { fail(e); }
      btn.disabled = false;
      btn.textContent = t("Save");
    });
    if (fields.some((f) => !f.locked)) section.append(h("div", { class: "actions" }, btn));
    return section;
  }

  // ---- pages ---------------------------------------------------------------
  const PAGES = [
    ["overview", "Overview", "◎"],
    ["library", "Library", "♫"],
    ["sources", "Music sources", "☁"],
    ["users", "Accounts", "☺"],
    ["links", "Shared links", "⇗"],
    ["speech", "Song texts", "✎"],
    ["extensions", "Extensions", "⧉"],
    ["network", "Network", "⌁"],
    ["security", "Security", "⚿"],
    ["log", "Log", "☰"],
  ];
  let main, bannerSlot;

  function renderBanner() {
    if (!bannerSlot) return;
    put(bannerSlot, );
    if (!restartPending) return;
    const btn = h("button", { class: "btn primary small", onclick: restart }, t("Restart now"));
    bannerSlot.append(h("div", { class: "banner warn" },
      h("span", {}, t("Some changes take effect after a restart."), " ", canRestart ? "" : t("Restart the server by hand to apply them.")),
      canRestart ? btn : null));
  }
  async function restart() {
    try { await api("POST", "restart"); } catch (e) { fail(e); return; }
    clearInterval(timer);
    put(main, h("div", { class: "banner" }, t("Restarting…")));
    const t0 = Date.now();
    await new Promise((r) => setTimeout(r, 1500));
    for (;;) {
      try {
        const s = await api("GET", "state");
        if (s) { location.reload(); return; }
      } catch (_) { /* still starting */ }
      if (Date.now() - t0 > 120000) { put(main, h("div", { class: "banner error" }, t("Not reachable"))); return; }
      await new Promise((r) => setTimeout(r, 1000));
    }
  }

  function shell() {
    const nav = h("nav", { "aria-label": t("Settings") }, brand());
    for (const [id, label, icon] of PAGES) {
      nav.append(h("button", { "data-page": id, onclick: () => go(id) }, h("span", { class: "icon", "aria-hidden": "true" }, icon), t(label)));
    }
    nav.append(h("div", { class: "foot" }, h("button", { onclick: async () => {
      try { await api("POST", "logout"); } catch (_) { /* ignore */ }
      showLogin();
    } }, h("span", { class: "icon", "aria-hidden": "true" }, "⎋"), t("Log out"))));
    bannerSlot = h("div");
    main = h("main", { id: "main" });
    put(app, h("div", { class: "shell" }, nav, h("div", {}, h("div", { style: "padding:1rem 2rem 0" }, bannerSlot), main)));
  }

  function go(id) {
    if (!PAGES.some((p) => p[0] === id)) id = "overview";
    if (location.hash !== "#" + id) history.replaceState(null, "", "#" + id);
    for (const b of document.querySelectorAll("nav button[data-page]")) {
      if (b.dataset.page === id) b.setAttribute("aria-current", "page"); else b.removeAttribute("aria-current");
    }
    clearInterval(timer);
    const page = PAGES.find((p) => p[0] === id);
    put(main, h("h2", {}, t(page[1])));
    ({ overview, library, sources, users, links, speech, extensions, network, security, log })[id]();
  }

  // Overview
  const WARNINGS = {
    noPassword: ["The server has no password: everyone in the network can use the library.", "security"],
    shortPassword: ["The server password is short.", "security"],
    noMusic: ["No music folders are set up.", "library"],
    speechModelMissing: ["The speech model file is missing.", "speech"],
    webModified: ["Files of the web client were changed: the server does not serve them.", null],
  };
  async function overview() {
    const body = h("div");
    main.append(body);
    const draw = async () => {
      let o;
      try { o = await api("GET", "overview"); } catch (e) { return; }
      restartPending = o.restartPending; canRestart = o.canRestart; renderBanner();
      const warn = (o.warnings || []).map((w) => {
        const [text, page] = WARNINGS[w] || [w, null];
        return h("div", { class: "banner warn" }, h("span", {}, t(text)), page ? h("button", { class: "btn small", onclick: () => go(page) }, t("Fix")) : null);
      });
      const lib = o.library;
      const sp = o.speech;
      const diskMax = Math.max(1, ...Object.values(o.disk));
      const speechStatus = !sp.model ? t("Off") : modelName(sp.model);
      put(body, ...warn, h("div", { class: "grid" },
        h("div", { class: "card" }, h("h3", {}, t("Library")),
          h("div", { class: "big" }, fmt.format(lib.tracks)), h("div", { class: "hint" }, t("Tracks")),
          h("dl", { class: "kv", style: "margin-top:.6rem" },
            h("dt", {}, t("Albums")), h("dd", {}, fmt.format(lib.albums)),
            h("dt", {}, t("Folders")), h("dd", {}, fmt.format(lib.folders))),
          h("div", { class: "spread", style: "margin-top:.75rem" },
            h("span", { class: "pill " + (lib.scanning ? "accent" : "on") }, lib.scanning ? t("Scanning…") : t("Up to date")),
            h("button", { class: "btn small", onclick: async () => { try { await api("POST", "rescan"); toast(t("Rescan started")); draw(); } catch (e) { fail(e); } } }, t("Rescan")))),
        h("div", { class: "card" }, h("h3", {}, t("Accounts")),
          h("dl", { class: "kv" },
            h("dt", {}, t("Users")), h("dd", {}, fmt.format(o.users)),
            h("dt", {}, t("Public links")), h("dd", {}, fmt.format(o.links)),
            h("dt", {}, t("App logins")), h("dd", {}, o.passwordRequired ? fmt.format(o.logins) : "–"),
            h("dt", {}, t("Devices online")), h("dd", {}, fmt.format(o.devices || 0)))),
        h("div", { class: "card" }, h("h3", {}, t("Speech recognition")),
          h("div", { class: "row" }, h("strong", {}, speechStatus), sp.auto ? h("span", { class: "pill on" }, t("Automatic")) : null),
          h("dl", { class: "kv", style: "margin-top:.6rem" },
            h("dt", {}, t("Timed songs")), h("dd", {}, fmt.format(sp.timed)),
            h("dt", {}, t("In the queue")), h("dd", {}, fmt.format(sp.queued))),
          sp.running ? h("div", { style: "margin-top:.6rem" }, h("div", { class: "hint" }, t("Working on a song")),
            h("div", { class: "bar" }, h("i", { style: `width:${Math.round(sp.running.progress * 100)}%` }))) : null,
          (o.downloads || []).map((d) => h("div", { style: "margin-top:.6rem" }, h("div", { class: "hint" }, t("Downloads") + ": " + d.id),
            h("div", { class: "bar" }, h("i", { style: `width:${d.total ? Math.round(d.done / d.total * 100) : 0}%` }))))),
        h("div", { class: "card" }, h("h3", {}, t("Storage")),
          Object.entries(o.disk).filter(([, v]) => v > 0).sort((a, b) => b[1] - a[1]).map(([k, v]) =>
            h("div", { class: "disk-row" }, h("span", {}, t(k)), h("div", { class: "bar" }, h("i", { style: `width:${Math.max(2, v / diskMax * 100)}%` })), h("span", {}, bytes(v))))),
        h("div", { class: "card" }, h("h3", {}, t("System")),
          h("dl", { class: "kv" },
            h("dt", {}, t("Version")), h("dd", {}, o.version),
            h("dt", {}, t("Running for")), h("dd", {}, duration(o.uptime)),
            h("dt", {}, t("Listen address")), h("dd", { class: "mono" }, o.bind),
            h("dt", {}, t("Data folder")), h("dd", { class: "mono" }, o.data),
            h("dt", {}, t("Web client")), h("dd", {}, o.webClient),
            h("dt", {}, t("Integrity")), h("dd", {}, o.webIntegrity)))));
    };
    await draw();
    timer = setInterval(draw, 4000);
  }

  async function withSettings(keys, extra) {
    let s;
    try { s = await api("GET", "settings"); } catch (e) { fail(e); return null; }
    restartPending = s.restartPending; renderBanner();
    main.append(settingsForm(s.settings, keys));
    if (extra) await extra(s);
    return s;
  }

  async function library() {
    await withSettings(["music", "images", "rescanInterval", "scanThreads", "analysisMaxMinutes"]);
    main.append(h("div", { class: "actions" }, h("button", { class: "btn", onclick: async () => {
      try { await api("POST", "rescan"); toast(t("Rescan started")); } catch (e) { fail(e); }
    } }, t("Rescan"))));
  }

  // Music sources
  const KINDS = [
    ["subsonic", "Subsonic / Navidrome", "Navidrome, Gonic, Airsonic and other Subsonic servers. The password is never sent; an API key works as \"apikey:<key>\".", "https://music.example.org"],
    ["jellyfin", "Jellyfin", "The music libraries of a Jellyfin server.", "https://jellyfin.example.org"],
    ["nextcloud", "Nextcloud", "A folder in Nextcloud. You log in on the Nextcloud page; the server gets an app password of its own.", "https://cloud.example.org"],
    ["webdav", "WebDAV", "A folder on a WebDAV share (ownCloud, NAS, …). Tags are read from the files once and kept.", "https://nas.example.org/dav"],
    ["sftp", "SFTP", "A folder on a computer reachable with SSH. Password or private key (OpenSSH format).", "sftp://nas.local"],
    ["web", "Web folder", "A folder on a web server with a directory listing, or a link to one audio file. Only links below this address are followed.", "https://example.org/music/"],
    ["dlna", "DLNA media server", "A media server in the local network (MiniDLNA, Plex, Serviio, …).", "http://192.168.1.10:8200/rootDesc.xml"],
  ];
  // Which fields each kind asks for.
  const KIND_FIELDS = {
    subsonic: ["url", "user", "secret"],
    jellyfin: ["url", "user", "secret"],
    nextcloud: ["url", "path"],
    webdav: ["url", "user", "secret", "path"],
    sftp: ["url", "user", "secret", "key", "path", "pin"],
    dlna: ["search", "url"],
    web: ["url", "user", "secret"],
  };
  const kindName = (k) => t((KINDS.find((x) => x[0] === k) || [k, k])[1]);

  async function sources() {
    main.append(h("p", { class: "hint" }, t("Songs on other servers and storage become part of the library: the server lists them, analyses them and plays them for the apps. The files stay where they are.")));
    const listBox = h("div", { class: "section" });
    const formBox = h("div", { class: "section", hidden: true });
    main.append(listBox, formBox);
    let polling = null;
    const status = (s) => {
      if (!s.enabled) return h("span", { class: "pill" }, t("Off"));
      if (s.syncing) return h("span", { class: "pill accent" }, t("Listing… %1", fmt.format(s.found)));
      if (s.error) return h("span", { class: "pill", style: "color:var(--danger)", title: s.error }, t("Failed: %1", s.error));
      if (s.synced) return h("span", { class: "pill on" }, t("Listed %1", date(s.synced)));
      return h("span", { class: "pill" }, t("Waiting"));
    };
    const draw = async () => {
      let list;
      try { list = await api("GET", "sources"); } catch (e) { return; }
      put(listBox, h("div", { class: "spread" }, h("h3", {}, t("Sources")),
        h("button", { class: "btn small primary", onclick: () => edit(null) }, "+ " + t("Add source"))),
      list.length ? h("div", { class: "table-wrap" }, h("table", {},
        h("thead", {}, h("tr", {}, h("th", {}, t("Name")), h("th", {}, t("Kind")), h("th", {}, t("Status")), h("th", { class: "num" }, t("Songs")), h("th"))),
        h("tbody", {}, list.map((s) => h("tr", {},
          h("td", {}, h("strong", {}, s.name), h("div", { class: "hint mono" }, s.url)),
          h("td", {}, kindName(s.kind)),
          h("td", {}, status(s)),
          h("td", { class: "num" }, fmt.format(s.songs)),
          h("td", { class: "num" }, h("div", { class: "row", style: "justify-content:flex-end" },
            h("button", { class: "btn small", disabled: !s.enabled || s.syncing, onclick: async () => {
              try { await api("POST", "sources/" + s.id + "/sync"); draw(); } catch (e) { fail(e); }
            } }, t("Sync now")),
            h("button", { class: "btn small", onclick: () => edit(s) }, t("Edit")),
            h("button", { class: "btn small danger", onclick: async () => {
              if (!await confirmDialog(t("Remove source"), t("Remove %1? Its songs leave the library; the files stay on the source.", s.name), t("Remove"))) return;
              try { await api("DELETE", "sources/" + s.id); toast(t("Removed")); draw(); } catch (e) { fail(e); }
            } }, t("Remove"))))))))) : h("p", { class: "empty" }, t("No sources yet.")));
      clearInterval(timer);
      timer = setInterval(draw, list.some((s) => s.syncing) ? 1500 : 8000);
    };
    const close = () => {
      clearInterval(polling);
      formBox.hidden = true;
      put(formBox);
    };
    const edit = (s) => {
      clearInterval(polling);
      formBox.hidden = false;
      const kind = h("select", { id: "src-kind", disabled: !!s }, KINDS.map(([v, l]) => h("option", { value: v, selected: s ? s.kind === v : v === "subsonic" }, t(l))));
      const name = h("input", { type: "text", id: "src-name", value: s ? s.name : "", maxlength: 64 });
      const url = h("input", { type: "url", id: "src-url", value: s ? s.url : "" });
      const user = h("input", { type: "text", id: "src-user", value: s ? s.user : "", autocomplete: "off" });
      const secret = h("input", { type: "password", id: "src-secret", autocomplete: "new-password", placeholder: s && s.hasSecret ? t("Unchanged") : "" });
      const key = h("textarea", { id: "src-key", rows: 4, autocomplete: "off", spellcheck: "false", class: "mono", placeholder: "-----BEGIN OPENSSH PRIVATE KEY-----" });
      const path = h("input", { type: "text", id: "src-path", value: s ? s.path : "", placeholder: "/" });
      const pin = h("input", { type: "text", id: "src-pin", value: s ? s.pin : "", class: "mono", placeholder: "SHA256:…" });
      const enabled = h("input", { type: "checkbox", id: "src-enabled", role: "switch", checked: s ? s.enabled : true });
      const found = h("div", { class: "list-edit" });
      const search = h("button", { class: "btn small", type: "button", id: "src-search", onclick: async () => {
        search.disabled = true;
        put(found, h("p", { class: "hint" }, t("Searching…")));
        try {
          const list = await api("GET", "sources/discover");
          put(found, list.length ? list.map((f) => h("div", { class: "spread" }, h("span", {}, h("strong", {}, f.name), h("div", { class: "hint mono" }, f.url)),
            h("button", { class: "btn small", type: "button", onclick: () => { url.value = f.url; if (!name.value.trim()) name.value = f.name; } }, t("Use")))) : h("p", { class: "hint" }, t("No media servers found.")));
        } catch (e) { put(found); fail(e); }
        search.disabled = false;
      } }, t("Search"));
      const kindHint = h("p", { class: "hint" });
      const field = (key, label, control, hint) => {
        const f = h("div", { class: "field", "data-key": key },
          h("div", {}, h("label", { for: control.id }, t(label)), hint ? h("p", { class: "hint" }, t(hint)) : null),
          h("div", {}, control));
        return f;
      };
      const fields = {
        search: field("search", "Media servers in the network", h("div", { id: "src-found" }, search, found)),
        url: field("url", "Address", url),
        user: field("user", "User name", user),
        secret: field("secret", "Password", secret),
        key: field("key", "Private key", key, "Instead of a password (OpenSSH format, without passphrase)."),
        path: field("path", "Folder", path, "Only songs below this folder; empty: all."),
        pin: field("pin", "Server key", pin, "SHA-256 fingerprint of the SSH host key. Empty: the key seen at the first connection is kept."),
      };
      const save = h("button", { class: "btn primary", type: "button" }, t("Connect"));
      const waitBox = h("div");
      const update = () => {
        const k = kind.value;
        const def = KINDS.find((x) => x[0] === k);
        kindHint.textContent = t(def[2]);
        url.placeholder = def[3];
        for (const [key, el] of Object.entries(fields)) el.hidden = !KIND_FIELDS[k].includes(key);
        save.textContent = k === "nextcloud" ? t("Log in with Nextcloud") : t("Connect");
      };
      kind.addEventListener("change", update);
      save.addEventListener("click", async () => {
        const k = kind.value;
        save.disabled = true;
        const label = save.textContent;
        save.textContent = t("Connecting…");
        try {
          if (k === "nextcloud") {
            const r = await api("POST", "sources/nextcloud", { url: url.value.trim(), name: name.value.trim(), path: path.value.trim() });
            const link = h("a", { class: "btn primary", href: r.loginUrl, target: "_blank", rel: "noopener noreferrer" }, t("Open Nextcloud"));
            put(waitBox, h("div", { class: "banner" }, h("span", {}, t("Open the Nextcloud page and allow access. This page notices when you are done.")), link),
              h("p", { class: "hint" }, t("Waiting for Nextcloud…")));
            polling = setInterval(async () => {
              try {
                const p = await api("POST", "sources/nextcloud/" + r.id);
                if (p.done) { close(); toast(t("Added")); draw(); }
              } catch (e) { clearInterval(polling); put(waitBox); fail(e); }
            }, 2000);
          } else {
            const body = { kind: k, name: name.value.trim(), url: url.value.trim(), user: user.value.trim(), secret: k === "sftp" && key.value.trim() ? key.value.trim() + "\n" : secret.value, path: path.value.trim(), pin: pin.value.trim(), enabled: enabled.checked };
            try {
              if (s) await api("PUT", "sources/" + s.id, body); else await api("POST", "sources", body);
            } catch (e) {
              // SFTP: a server key seen for the first time is shown to be trusted.
              const fp = k === "sftp" && !pin.value.trim() && /SHA256:[A-Za-z0-9+\/]+/.exec(e.message || "");
              if (fp) {
                pin.value = fp[0];
                put(waitBox, h("div", { class: "banner warn" }, h("span", {}, t("Compare this key with the server's (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub), then connect again to trust it:"), " ", h("code", {}, fp[0]))));
                throw new Error(t("Unknown server key"));
              }
              throw e;
            }
            close();
            toast(t(s ? "Saved" : "Added"));
            draw();
          }
        } catch (e) { fail(e); }
        save.disabled = false;
        save.textContent = label;
      });
      put(formBox, h("h3", {}, t(s ? "Edit source" : "Add source")), kindHint,
        field("kind", "Kind", kind),
        field("name", "Name", name),
        ...Object.values(fields),
        h("div", { class: "field" }, h("div", {}, h("label", { for: "src-enabled" }, t("Turned on"))), h("div", {}, h("label", { class: "switch" }, enabled))),
        waitBox,
        h("div", { class: "actions" }, h("button", { class: "btn", type: "button", onclick: close }, t("Cancel")), save));
      update();
      formBox.scrollIntoView({ behavior: "smooth", block: "start" });
      name.focus();
    };
    await draw();
  }

  async function users() {
    main.append(h("p", { class: "hint" }, t("Accounts with storage are end-to-end encrypted: the admin sees names and sizes, never contents.")));
    await withSettings(["registration", "vaultQuotaMb"]);
    const inv = h("div", { class: "section" });
    const accounts = h("div", { class: "section" });
    main.append(inv, accounts);
    const days = h("select", { "aria-label": t("Valid for") }, [[1, "1 day"], [7, "7 days"], [30, "30 days"]].map(([v, l]) => h("option", { value: v, selected: v === 7 }, t(l))));
    const drawInvites = async () => {
      let list = [];
      try { list = await api("GET", "invites"); } catch (e) { fail(e); }
      put(inv, h("h3", {}, t("Invitations")),
        h("p", { class: "hint" }, t("New accounts need an invitation code. Each code works once.")),
        h("div", { class: "row", style: "margin:.75rem 0" }, h("label", { class: "hint" }, t("Valid for")), days,
          h("button", { class: "btn primary small", onclick: createInvite }, t("Create invitation"))),
        list.length ? h("table", {}, h("tbody", {}, list.map((i) => h("tr", {},
          h("td", { class: "mono faint" }, i.id.slice(0, 8) + "…"), h("td", {}, t("Expires %1", date(i.expires))),
          h("td", { class: "num" }, h("button", { class: "btn small danger", onclick: async () => {
            try { await api("DELETE", "invites/" + i.id); toast(t("Revoked")); drawInvites(); } catch (e) { fail(e); }
          } }, t("Revoke"))))))) : h("p", { class: "empty" }, t("No open invitations.")));
    };
    const createInvite = async () => {
      try {
        const r = await api("POST", "invites", { days: parseInt(days.value, 10) });
        const code = h("div", { class: "secret", "aria-label": t("Invitation code") }, r.code);
        const d = h("dialog", {}, h("h3", {}, t("Invitation code")), h("p", { class: "hint" }, t("Give this code to the new user. It is shown only now.")), code,
          h("div", { class: "actions" },
            h("button", { class: "btn", onclick: async () => { try { await navigator.clipboard.writeText(r.code); toast(t("Copied")); } catch (_) { /* no clipboard */ } } }, t("Copy")),
            h("button", { class: "btn primary", onclick: () => d.close() }, t("Close"))));
        d.addEventListener("close", () => d.remove());
        document.body.append(d);
        d.showModal();
        drawInvites();
      } catch (e) { fail(e); }
    };
    const drawAccounts = async () => {
      let list = [];
      try { list = await api("GET", "users"); } catch (e) { fail(e); }
      put(accounts, h("h3", {}, t("Accounts")), list.length ? h("div", { class: "table-wrap" }, h("table", {},
        h("thead", {}, h("tr", {}, h("th", {}, t("Name")), h("th", {}, t("Created")), h("th", { class: "num" }, t("Files")), h("th", { class: "num" }, t("Storage")), h("th", { class: "num" }, t("Passkeys")), h("th"))),
        h("tbody", {}, list.map((u) => h("tr", {},
          h("td", {}, h("strong", {}, u.name)), h("td", {}, date(u.created)), h("td", { class: "num" }, fmt.format(u.files)),
          h("td", { class: "num" }, bytes(u.bytes)), h("td", { class: "num" }, fmt.format(u.passkeys)),
          h("td", { class: "num" }, h("button", { class: "btn small danger", onclick: async () => {
            if (!await confirmDialog(t("Delete account"), t("This deletes the account %1 with all its encrypted files and links. It cannot be undone. Type the name to confirm:", u.name), t("Delete"), u.name)) return;
            try { await api("DELETE", "users/" + encodeURIComponent(u.name)); toast(t("Deleted")); drawAccounts(); } catch (e) { fail(e); }
          } }, t("Delete")))))))) : h("p", { class: "empty" }, t("No accounts yet.")));
    };
    await Promise.all([drawInvites(), drawAccounts()]);
  }

  async function links() {
    main.append(h("p", { class: "hint" }, t("Links to encrypted files that anyone with the link can open. The server cannot read the files.")));
    const box = h("div", { class: "section" });
    main.append(box);
    const draw = async () => {
      let list = [];
      try { list = await api("GET", "links"); } catch (e) { fail(e); }
      put(box, list.length ? h("div", { class: "table-wrap" }, h("table", {},
        h("thead", {}, h("tr", {}, h("th", {}, t("Owner")), h("th", {}, t("Created")), h("th", {}, t("Expires")), h("th", {}, t("Password")), h("th"))),
        h("tbody", {}, list.map((l) => h("tr", {},
          h("td", {}, h("strong", {}, l.owner)), h("td", {}, date(l.created)), h("td", {}, l.expires ? date(l.expires) : t("Never")),
          h("td", {}, l.password ? t("Yes") : t("No")),
          h("td", { class: "num" }, h("button", { class: "btn small danger", onclick: async () => {
            try { await api("DELETE", "links/" + l.id); toast(t("Revoked")); draw(); } catch (e) { fail(e); }
          } }, t("Revoke")))))))) : h("p", { class: "empty" }, t("No public links.")));
    };
    await draw();
  }

  async function speech() {
    main.append(h("p", { class: "hint" }, t("With a speech model the server times plain song texts by recognising the sung words, and writes down what is sung when a song has no text. Everything runs on this server.")));
    const modelsBox = h("div", { class: "section" });
    const statusBox = h("div", { class: "section" });
    main.append(modelsBox);
    await withSettings(["autoSyncLyrics"]);
    main.append(statusBox);
    const pretty = (id) => id.replace(/-(q\d_\d)$/, " ($1)").replace("large-v3-turbo", "large v3 turbo");
    const draw = async () => {
      let s;
      try { s = await api("GET", "speech"); } catch (e) { return; }
      const locked = s.modelLockedBy;
      const busy = s.models.some((m) => m.state === "downloading" || m.state === "checking");
      const pick = async (id) => {
        try { await api("POST", "speech/model", { id }); draw(); } catch (e) { fail(e); }
      };
      const cards = s.models.map((m) => {
        let state = null, actions = [];
        if (m.state === "downloading") {
          const pct = m.total ? Math.round(m.done / m.total * 100) : 0;
          state = h("div", {}, h("div", { class: "bar" }, h("i", { style: `width:${pct}%` })), h("div", { class: "hint" }, `${pct} % · ${bytes(m.done)} / ${bytes(m.total)}`));
        } else if (m.state === "checking") {
          state = h("div", { class: "hint" }, t("Checking…"));
        } else if (m.state === "failed") {
          state = h("div", { class: "hint", style: "color:var(--danger)" }, t("Failed: %1", m.error));
          actions.push(h("button", { class: "btn small", disabled: locked, onclick: () => pick(m.id) }, t("Try again")));
        }
        if (m.current) {
          state = h("span", { class: "pill accent" }, t("In use"));
        } else if (!m.state || m.state === "failed") {
          if (m.downloaded) {
            actions.push(h("button", { class: "btn small primary", disabled: locked, onclick: () => pick(m.id) }, t("Use")));
            actions.push(h("button", { class: "btn small danger", onclick: async () => {
              try { await api("DELETE", "speech/models/" + m.id); draw(); } catch (e) { fail(e); }
            } }, t("Delete")));
          } else if (m.state !== "failed") {
            actions.push(h("button", { class: "btn small", disabled: locked, onclick: () => pick(m.id) }, t("Download and use")));
          }
        }
        return h("div", { class: "model" + (m.current ? " current" : "") },
          h("div", { class: "spread" }, h("h4", {}, pretty(m.id)), m.downloaded && !m.current ? h("span", { class: "pill" }, t("Downloaded")) : null),
          h("div", { class: "meta" }, t(m.note)),
          h("div", { class: "meta" }, bytes(m.size) + " · " + t("%1 of memory", bytes(m.memoryMb * 1e6))),
          state, h("div", { class: "actions" }, actions));
      });
      const current = s.summary.model;
      put(modelsBox, 
        h("div", { class: "spread" }, h("h3", {}, t("Models")),
          current && !locked ? h("button", { class: "btn small", onclick: () => pick(null) }, t("Turn off")) : null),
        locked ? h("p", { class: "locked" }, t("Set by %1", origin(locked))) : null,
        !current ? h("p", { class: "hint" }, t("Speech recognition is off.")) : h("p", { class: "hint mono", title: current }, current.split(/[\\/]/).pop()),
        h("div", { class: "models", style: "margin-top:.75rem" }, cards),
        h("p", { class: "hint", style: "margin-top:.75rem" }, t("Models are downloaded from %1 when picked and checked against their checksum.", new URL(s.source).host)));
      const sm = s.summary;
      put(statusBox, h("h3", {}, t("Status")),
        h("dl", { class: "kv", style: "margin:.5rem 0" },
          h("dt", {}, t("Timed songs")), h("dd", {}, fmt.format(sm.timed)),
          h("dt", {}, t("In the queue")), h("dd", {}, fmt.format(sm.queued)),
          h("dt", {}, t("Not timed")), h("dd", {}, fmt.format(sm.failed))),
        sm.model ? h("p", { class: "hint" }, sm.loaded ? t("The model is loaded") : t("The model loads with the next song")) : null,
        sm.running ? h("div", {}, h("div", { class: "hint" }, t("Working on a song")), h("div", { class: "bar" }, h("i", { style: `width:${Math.round(sm.running.progress * 100)}%` }))) : null,
        sm.failed ? h("div", { class: "actions" }, h("button", { class: "btn small", onclick: async () => {
          try { const r = await api("POST", "speech/retry"); toast(t("%1 songs will be tried again", r.retried)); draw(); } catch (e) { fail(e); }
        } }, t("Try failed songs again"))) : null);
      clearInterval(timer);
      timer = setInterval(draw, busy || sm.running || sm.queued ? 1500 : 6000);
    };
    await draw();
  }

  async function extensions() {
    main.append(h("p", { class: "hint" }, t("Extensions run as separate programs; the server only connects to the addresses set here.")));
    await withSettings(["extensions"]);
  }

  async function network() {
    await withSettings(["name", "allowedHosts", "publicUrl", "androidCerts", "pushAllowPrivate", "sourceUrl", "verbose"]);
  }

  async function security() {
    let st = {};
    try { st = await api("GET", "state"); } catch (e) { fail(e); }
    // Admin password.
    const cur = passwordInput("cur", "current-password"), a = passwordInput("np1", "new-password"), b = passwordInput("np2", "new-password");
    cur.removeAttribute("minlength");
    const status = h("div", { class: "status" });
    const adminBox = h("div", { class: "section" }, h("h3", {}, t("Change the admin password")));
    if (st.passwordFromFile) {
      adminBox.append(h("p", { class: "locked" }, t("The admin password is set by a file on the server.")));
    } else {
      adminBox.append(h("form", { onsubmit: async (e) => {
        e.preventDefault();
        status.className = "status error";
        if ([...a.value].length < 12) { status.textContent = t("Use at least 12 characters"); return; }
        if (a.value !== b.value) { status.textContent = t("The passwords differ"); return; }
        try {
          await api("PUT", "password", { current: cur.value, password: a.value });
          status.textContent = "";
          cur.value = a.value = b.value = "";
          toast(t("Password changed"));
        } catch (err) { status.textContent = err.message; }
      } },
      h("div", { class: "field" }, h("label", { for: "cur" }, t("Current password")), cur),
      h("div", { class: "field" }, h("label", { for: "np1" }, t("New password")), a),
      h("div", { class: "field" }, h("label", { for: "np2" }, t("Repeat the password")), b),
      status, h("div", { class: "actions" }, h("button", { class: "btn primary", type: "submit" }, t("Change password")))));
    }
    main.append(adminBox);
    // Server password.
    let s;
    try { s = await api("GET", "settings"); } catch (e) { fail(e); return; }
    let o = {};
    try { o = await api("GET", "overview"); } catch (_) { /* optional */ }
    const sp = s.serverPassword;
    const spBox = h("div", { class: "section" }, h("h3", {}, t("Server password")),
      h("p", { class: "hint" }, t("Apps and the web client log in with it.")),
      h("p", {}, sp.set ? h("span", { class: "pill on" }, t("A password is set.")) : h("span", { class: "pill warn" }, t("No password is set: everyone who reaches the server can use the library."))));
    if (sp.lockedBy) {
      spBox.append(h("p", { class: "locked" }, t("Set by %1", origin(sp.lockedBy))));
    } else {
      const p1 = passwordInput("sp1", "new-password"), p2 = passwordInput("sp2", "new-password");
      const st2 = h("div", { class: "status" });
      spBox.append(h("form", { onsubmit: async (e) => {
        e.preventDefault();
        st2.className = "status error";
        if ([...p1.value].length < 12) { st2.textContent = t("Use at least 12 characters"); return; }
        if (p1.value !== p2.value) { st2.textContent = t("The passwords differ"); return; }
        try { await api("PUT", "server-password", { password: p1.value }); toast(t("Server password set")); go("security"); } catch (err) { st2.textContent = err.message; }
      } },
      h("div", { class: "field" }, h("label", { for: "sp1" }, t("New password")), p1),
      h("div", { class: "field" }, h("label", { for: "sp2" }, t("Repeat the password")), p2),
      h("p", { class: "hint" }, t("Changing it logs out every app.")), st2,
      h("div", { class: "actions" },
        sp.set ? h("button", { class: "btn danger", type: "button", onclick: async () => {
          if (!await confirmDialog(t("Remove password"), t("Remove the server password? Everyone who reaches the server can then use the library."), t("Remove password"))) return;
          try { await api("PUT", "server-password", { password: null }); toast(t("Server password removed")); go("security"); } catch (err) { fail(err); }
        } }, t("Remove password")) : null,
        h("button", { class: "btn primary", type: "submit" }, t("Set password")))));
    }
    main.append(spBox);
    if (sp.set) {
      main.append(h("div", { class: "section" }, h("h3", {}, t("App logins")),
        h("p", { class: "hint" }, t("%1 apps are logged in with the server password.", fmt.format(o.logins || 0))),
        h("div", { class: "actions" }, h("button", { class: "btn danger", onclick: async () => {
          if (!await confirmDialog(t("End all app logins"), t("All apps must log in again. Continue?"), t("End all app logins"))) return;
          try { await api("POST", "logins/revoke"); toast(t("Logins ended")); } catch (e) { fail(e); }
        } }, t("End all app logins")))));
    }
  }

  async function log() {
    const filter = h("input", { type: "text", placeholder: t("Filter"), "aria-label": t("Filter"), style: "max-width:20rem" });
    const pre = h("pre", { class: "log", tabindex: 0 });
    let lines = [];
    const render = () => {
      const f = filter.value.trim().toLowerCase();
      const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 20;
      put(pre, ...lines.filter((l) => !f || l.toLowerCase().includes(f)).map((l) => {
        const level = (l.split(/\s+/)[2] || "").trim();
        return h("span", { class: level }, l + "\n");
      }));
      if (atBottom) pre.scrollTop = pre.scrollHeight;
    };
    const load = async () => {
      try { lines = await api("GET", "log"); render(); } catch (_) { /* shown by api() */ }
    };
    filter.addEventListener("input", render);
    main.append(h("div", { class: "spread", style: "margin-bottom:.75rem" }, filter, h("button", { class: "btn small", onclick: load }, t("Refresh"))), pre);
    await load();
    pre.scrollTop = pre.scrollHeight;
    timer = setInterval(load, 5000);
  }

  // ---- start ----------------------------------------------------------------
  async function start() {
    let s;
    try { s = await api("GET", "state"); } catch (e) {
      put(app, h("div", { class: "center" }, h("div", { class: "box" }, h("p", {}, e.message || t("Not reachable")))));
      return;
    }
    info = { name: s.name, version: s.version };
    document.title = s.name + " · " + t("Admin");
    const m = location.hash.match(/^#setup=([0-9a-f]{16,128})$/i);
    if (m) {
      showSetup(m[1]);
      return;
    }
    if (s.setup) { showSetup(null); return; }
    if (!s.loggedIn) { showLogin(); return; }
    shell();
    go(location.hash.slice(1) || "overview");
  }
  window.addEventListener("hashchange", () => {
    if (main && document.body.contains(main) && !location.hash.startsWith("#setup=")) go(location.hash.slice(1));
  });
  start();
})();
