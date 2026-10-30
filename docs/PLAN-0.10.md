# Plan: everything on the roadmap, then the 0.10.0 beta

The features of [`ROADMAP.md`](ROADMAP.md) are built one after the other in
the phases below, plus an admin interface for the server. Each phase ends
with tests (unit, server, desktop UI tests, Windows CI, Android build), a
pull request into `main` that is merged when CI is green, and an update of
this file. After the last feature phase, the user interface is reworked so
the new pages fit together, and 0.10.0 is released as the final beta.

Principles stay as before: security first (nothing listens or connects
without the user turning it on, credentials only in protected stores,
strict CSP for web pages), own implementations instead of external
libraries where reasonable, everything works in the apps without a server,
German and English texts.

Legend: `[ ]` open, `[~]` in progress, `[x]` done.

## Phase 1: server settings and admin interface

Foundation for everything after it: settings that can be changed while the
server runs, and a web interface to manage them.

- [x] Settings store: settings changed in the admin interface are kept in
  `data/settings.json`. Order: defaults < `--config` file < admin settings
  < environment < command line; values set by the environment or command
  line are shown read-only with their origin. Settings are applied live
  where possible (name, rescan interval, registration, quotas, speech
  model, extensions, sources); the others (listen address, data folder)
  ask for a restart, which the server does itself (re-exec).
  The container examples stop passing defaults for settings the admin
  interface manages.
- Moved to phase 2: the event channel (the admin pages poll for now).
- [x] Admin login:
  - No admin yet: the server logs a one-time setup link (valid one hour);
    `beatlumen-server admin-link` creates a new one. The link sets the
    admin password (Argon2id, at least 12 characters).
    `BEATLUMEN_ADMIN_PASSWORD_FILE` for setups without a browser. Open:
    a passkey for the admin login.
  - Sessions: random 256-bit cookie (only its digest stored), `HttpOnly`,
    `SameSite=Strict`, `Secure` behind TLS; idle timeout 12 h, at most
    7 days; CSRF header and origin check; failed logins limited per
    address; password change ends all sessions.
- [x] Admin pages at `/admin` (plain HTML/JS/CSS like the share page, in
  the signed manifest with SRI, no external resources):
  - Overview: version, uptime, library size, running jobs (scan, analysis,
    speech recognition, downloads, imports), disk use, warnings.
  - Library: music and image folders, rescan, scan threads, analysis limits.
  - Users: accounts with storage use, invitation codes (create, revoke),
    registration, quota, delete account; server password and login tokens.
  - Shared links: public links of all accounts (expiry, downloads), revoke.
  - Song texts and speech recognition: model choice, background download,
    automatic timing, queue and failures, start for selected songs.
  - Extensions and (phase 4) sources.
  - Network: name, allowed host names, public URL, push options, extra
    CA certificates.
  - Log: recent server log lines (no secrets).
- [x] Speech models download themselves: the admin picks a model from a
  built-in list (tiny, base, small, medium, large-v3-turbo of whisper.cpp,
  with size and memory needs); the server downloads it in the background
  (resumable, progress in the overview, SHA-256 checked against the value
  in the list, then switched to without a restart). Own files stay
  possible. `bl-asr` learns quantised models (q8_0, q5_0, q5_1), which
  need far less memory.

## Phase 2: devices: remote control and transfer

- [x] Event channel `GET /api/events` (long polling, per login) for
  changes, commands and job progress.
- [x] Device registry on the server: every logged-in app is a device
  (name, type, platform, online state, what it plays, volume); visible only
  within the same account (or the server password login).
- [x] Commands through the event channel (no open ports on the devices):
  play/pause, seek, skip, volume, queue changes, "play on …". Each device
  can refuse remote control (setting).
- [x] Transfer playback: queue and position move to another device.
- [x] App: device page (list, control a device as if it were local),
  "Play on" in song menus and the player.
- [x] Groups that play in sync (multi-room, basic): clock sync with the
  server, start at a common time. Open: drift correction while playing.

## Phase 3: settings follow the account

- [x] App settings (appearance, effects, styles, player options) are
  stored with the user account (end-to-end encrypted like the files) and
  merged per setting (newest wins); device-specific settings (audio output,
  receivers, folders) stay on the device.

## Phase 4: music sources

Common framework first (browse, list, open as seekable stream with range
requests, metadata, cached analysis, credentials in the system keystore in
the apps or encrypted in the server data). Sources can be added in the app
(app-only use) or in the admin interface (the server indexes and analyses
them and the apps play through it).

- [x] M3U/M3U8/PLS/XSPF playlists: import, export, playlists inside
  library folders; entries resolve only inside the library or the
  playlist's own source.
- [x] Web sources: links to audio files and directory listings (no
  scripts, same host only, size and depth limits).
- [x] Internet radio: Icecast/Shoutcast (MP3, AAC, Ogg, Opus) and HLS,
  station files, favourites, ICY titles (the adjustable delay for
  look-ahead follows with the live beat prediction in phase 6).
- [x] Subsonic/OpenSubsonic (Navidrome, Gonic, …): token auth, original
  files, scrobbling.
- [x] Jellyfin: login, artists/albums/playlists, original files, playback
  reports.
- [x] WebDAV/Nextcloud: folders on WebDAV, Nextcloud login flow v2 with an
  app password, also backgrounds and styles.
- [x] SFTP: own SSH client (curve25519 key exchange, Ed25519/ECDSA/RSA host
  keys with trust on first use, ChaCha20-Poly1305/AES-GCM, key or password
  login) and SFTP v3.
- [x] DLNA/UPnP media servers: browse and play (discovery only while
  searching, addresses can be entered).
- [x] Mounted network folders (SSHFS, SMB, NFS): scans with time limits,
  periodic rescans, no blocking.
- [x] Admin interface: *Music sources* page (add, test the login, list in
  the background, sync now, turn off, remove), Nextcloud login flow, DLNA
  search; logins encrypted with a key derived from the vault pepper.
- [x] App-only use: the same sources in the apps without a server
  (Settings, *Music sources*; logins in the platform's protected store).

## Phase 5: receiving and sharing

- [x] DLNA renderer: look-ahead analysis of the received stream (files are
  read ahead over a second connection and analysed whole; live streams
  follow the audio); new control points must be allowed in the app
  (once or always) before they play.
- [ ] AirPlay receiver: look-ahead analysis with the audio the sender
  sends ahead.
- [x] Share target: "Share" on Android and "Open with" (songs, playlist
  files, links) ask to play now, queue, or keep the files in the
  encrypted storage (and so on every device of the account).
- [ ] AirPlay 2 receiver (basic): pairing (SRP, Ed25519, X25519,
  ChaCha20-Poly1305), buffered audio; senders keep AirPlay 1 as fallback.

## Phase 6: live audio

- [x] Beat prediction for live input (other apps, AirPlay, DLNA, radio):
  tempo from the autocorrelation of the onset envelope, phase from a
  comb; effects land on the predicted beats; sync offset measured by
  tapping along (e.g. Bluetooth latency).

## Phase 7: offline and song texts

- [x] Keep whole albums and playlists offline (⋮ menu of an album or
  playlist); storage limit with an overview by album in the settings.
- [x] Speech recognition in the app without a server: model download in
  the app (same list as the server), start per song, computed on the
  device.
- [x] Word timing from the recognition (enhanced LRC), on the server and
  in the app.

## Phase 8: offloading

- [ ] Smaller streams for mobile data: an own Opus encoder (CELT mode) on
  the server, used when the app asks for it (setting per network type).
- [ ] Remote sources indexed and analysed on the server (from phase 4).

## Phase 9: user interface rework

- [ ] Navigation for the new areas (sources, devices, radio): rail on wide
  screens, bottom bar with "More" on phones.
- [ ] Settings in categories with search; consistent dialogs and empty
  states; first start guide (server, folders, receivers).
- [ ] Layout review on phone, tablet and desktop (screenshots of every
  page, light and dark), translations complete, accessibility names.

## Phase 10: 0.10.0 beta

- [ ] Version 0.10.0 everywhere (Cargo, CMake, Android version code,
  container, web manifest), changelog, documentation.
- [ ] Security review of everything new; all tests on Linux, Windows and
  Android; builds: APK, AppImage, Windows installer and portable,
  server, container image, web client.
- [ ] Tag `v0.10.0` and a GitHub pre-release with the builds (published
  after the user's confirmation).
