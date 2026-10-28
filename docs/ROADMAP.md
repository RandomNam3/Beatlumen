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
- User accounts (instead of the single shared password), with per-user
  playlists, presets, backgrounds and settings.
- Sharing music between users on the same server.
- Public share links (with optional expiry and password).

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
    music to Beatlumen, which plays and visualises it with full analysis.
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
system audio mode.
