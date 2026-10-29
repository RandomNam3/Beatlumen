# Roadmap

Planned after the first release (requested by the project owner). The rule
that apps make no network connection except to sources the user configured
stays in force: remote sources are accessed through the Beatlumen server
whenever one is connected, and directly from the app only when the user
explicitly sets them up for app-only use.

## Lyrics
- Show lyrics for the current track (embedded tags, `.lrc`/`.txt` next to the file).
- Time-stamped lyrics (LRC, enhanced LRC with word timing), synced scrolling.
- Lyrics import from files.
- Speech recognition library (own implementation, no external services) that
  produces second-accurate synced lyrics with pause detection.
  - Server: optional automatic indexing of the whole library, or manual start
    per selected track.
  - App without server: manual start per track, computed locally.

## Server
- Done: user accounts with end-to-end encrypted files, sharing between
  users, passkeys (`docs/ENCRYPTION.md`); source extensions
  (`docs/EXTENSIONS.md`).
- Per-user playlists, presets, backgrounds and settings (today they are
  shared by everyone on the server).
- Public share links (with optional expiry and password).

## Devices: remote control and individual playback
With an active server connection, all apps and devices logged in to the
server see each other.

- Device list with name, type, online state and what is playing.
- Each device plays on its own by default (own queue, own position).
- Remote control of any device: play/pause, seek, skip, volume, edit the
  queue, and start music on a chosen device ("play on …").
- Transfer playback: move the queue and position from one device to
  another.
- Optional later: groups of devices that play in sync (multi-room).
- Security:
  - Commands run through the existing authenticated server connection; the
    devices open no ports for this.
  - Only devices of the same account (with user accounts: the same user or
    users it was shared with) can control each other.
  - Each device can turn off being remote-controlled.

## Receiving from other devices (cast target)
Beatlumen as a target that other devices and apps can send music to. It
plays and visualises what it receives. Every receiver is off by default,
only works in the local network, and new senders can be required to confirm
with a PIN.

- **DLNA/UPnP renderer:** covers many Android apps (BubbleUPnP, Fritz!App
  …), Windows "Play to" and TVs/receivers.
  - Done: media renderer built into the app on Linux, Windows and Android
    (`crates/bl-upnp`), with title, artist, cover, seeking, volume and the
    next track (gapless queueing by the control point).
  - Next: look-ahead analysis of the stream (it is a file the app could
    read ahead), PIN confirmation for new control points.
- **AirPlay (audio)** from iPhone, iPad and Mac.
  - Done: AirPlay 1 (RAOP) receiver built into the app on Linux, Windows
    and Android (`crates/bl-airplay`), with title, artist, cover, progress
    and sender volume. Browsers cannot accept network connections, so the
    web version cannot be a receiver.
  - Next: use the roughly two seconds the sender sends ahead for look-ahead
    analysis (effects that anticipate beats, as with files).
  - Later: AirPlay 2 (needs HomeKit-style pairing: SRP, Ed25519, X25519,
    ChaCha20-Poly1305), for multi-room groups with other AirPlay 2
    speakers. Senders still stream to AirPlay 1 receivers.
  - The protocol is re-implemented, not certified by Apple.
- **Share target:** "Share" on Android and "Open with" on desktop send files
  and links to this or another Beatlumen device (through the server).
- **Google Cast (Chromecast): not possible.** Cast senders check a
  Google-signed device certificate, which only certified devices have.
  Android apps can reach Beatlumen through DLNA or the share target instead.

## Music sources
Self-hosted and generic sources the user owns or controls. They deliver the
audio files themselves, so playback gets the full look-ahead analysis like
local files. Remote files are streamed with range requests; analysis results
are cached locally.

- **Jellyfin:** browse the music library (artists, albums, playlists) and
  stream the original files.
  - Login with the user's Jellyfin account; only the access token is stored.
  - Playback is reported back to Jellyfin, so play counts and "resume" stay
    correct.
- **Subsonic / OpenSubsonic:** servers with the Subsonic API (Navidrome,
  Gonic, Airsonic-Advanced, Ampache, …).
  - Browse artists, albums and playlists; stream the original files
    (no transcoding) for full analysis.
  - Token authentication (salted, no plain password on the wire); playback
    is reported ("scrobble") to the server.
- **Nextcloud / WebDAV:** music folders on any WebDAV server (Nextcloud,
  ownCloud, NAS systems).
  - Uses the existing HTTP/TLS client, including certificate pinning.
  - Nextcloud: login flow v2 with an app password (revocable in Nextcloud).
  - Other files from there too: backgrounds, presets.
- **SFTP:** music folders on any SSH server.
  - Login with an SSH key (preferred) or password.
  - Host keys are checked: trust on first use with the fingerprint shown,
    like self-signed HTTPS certificates.
- **SSHFS (and other mounted network drives):** a mount is a normal folder
  for Beatlumen, so it works today. Planned: robustness for slow mounts
  (scanning with timeouts, periodic rescans where file change notifications
  do not work, no blocking of the UI).
- **DLNA / UPnP AV** (Fritz!Box, NAS systems, media servers on TVs and
  receivers):
  - Client: browse and play from DLNA media servers in the local network.
  - Renderer (optional, off by default): other devices and apps can send
    music to Beatlumen, which plays and visualises it (done, see above).
  - Discovery (SSDP) only in the local network and only while the user
    searches; servers can also be entered by address.
- **Internet radio:** Icecast/Shoutcast streams (MP3, AAC, Ogg, Opus) and
  HLS.
  - Station files (`.pls`, `.m3u`, `.xspf`) and stream URLs can be added as
    stations; stations can be saved as favourites.
  - Current title from the stream metadata (ICY).
  - Adjustable delay (a few seconds of buffer) gives the analysis look-ahead,
    so effects can anticipate beats like with files.
- **Web sources (HTTP/HTTPS):** direct links to audio files, and web pages or
  directory listings (e.g. Apache/nginx index pages) whose links point to
  audio files.
  - Links are read from the page's HTML; no scripts are run, and only links
    to audio files on the same host are followed.
  - Size and depth limits when reading pages.
- **M3U playlists:** import and export of M3U/M3U8 (with `#EXTINF`), also PLS.
  - Relative paths, local files and URLs.
  - M3U files inside library folders show up as playlists.
  - Entries only resolve to files inside the library folders or the
    playlist's own source (no access to arbitrary paths through a playlist).
- Common to all:
  - Credentials stored in the system keystore where available (Android
    Keystore, Secret Service, Windows Credential Manager).
  - Sources can be added in the app or on the server; with a server, it
    indexes and analyses them and the apps play through it.

## System audio
- Visualising whatever the device plays: PulseAudio/PipeWire monitor on
  Linux, WASAPI loopback on Windows, Android's playback capture with user
  consent where the playing app allows it. Any player can drive the visuals
  this way.
  - Done on Android 10+ ("Visualize other apps" in the library, in the
    settings and as a quick settings tile): the captured audio drives the
    visuals without being played again; with notification access the
    playing app's title, artist, cover and position are shown and play,
    pause, next, previous and seek control it.
  - Done on Linux (the default output's monitor through PulseAudio or
    PipeWire, MPRIS players for title, cover and controls) and Windows
    (WASAPI loopback, system media sessions).
- Live input only: effects react instead of anticipating beats. Beat
  prediction (tempo and phase tracking) and the output latency (e.g. of
  Bluetooth) make up for part of that.

## Offloading
- Everything works in the apps without a server.
- With a connected and logged-in server, heavy processes can run on the server
  instead (analysis is already supported; later: lyrics recognition,
  transcoding, library indexing, remote sources).

## Not planned: streaming services and download portals
No integration of specific providers (Spotify, Qobuz, Tidal, Deezer,
SoundCloud, YouTube/YouTube Music, Bandcamp, Last.fm, …), decided October
2026. Their terms forbid or do not allow what Beatlumen needs: access to the
audio for analysis, caching and pre-loading, and visuals that are
synchronised with the music. Some of them additionally restrict API access
to partners. Downloaders that circumvent protection measures are not part of
Beatlumen. Music from such services can still drive the visuals through the
system audio mode. A server admin can connect separate source extensions
(`docs/EXTENSIONS.md`) on their own responsibility; this repository
contains none.
