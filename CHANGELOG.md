# Changelog

## 0.10.0 (beta)

The first beta: Beatlumen plays music from many places, on many devices,
and the server is managed in the browser.

### Server

- **Admin interface** at `/admin/`: library folders, music sources,
  accounts and invitations, shared links, speech recognition, extensions,
  network, security and the log. Setup link on the first start (or
  `beatlumen-server admin-link`), Argon2id password, strict sessions.
  Most settings apply at once; the server restarts itself for the others.
- **Speech models** are picked in the admin interface and downloaded in
  the background (resumable, checked against their SHA-256).
- **Music sources**: Subsonic/OpenSubsonic (Navidrome, Gonic, …),
  Jellyfin, WebDAV/Nextcloud (login flow with an app password), SFTP (own
  SSH client), DLNA media servers and web folders join the library; the
  server lists, analyses and streams them with seeking. Logins are stored
  encrypted with a key derived from the vault pepper.
- Playlist files (M3U, PLS, XSPF) in the music folders are read-only
  playlists. A hung network mount no longer stalls the scan.
- Song texts timed by speech recognition get word times (enhanced LRC).
- **Smaller streams**: on request the server converts songs to Opus while
  sending them (own encoder, 24–256 kbit/s); seeking starts a new stream.
- Devices: the apps of one account see, control and take over each
  other's playback; settings follow the account (end-to-end encrypted).

### Apps

- **Music sources without a server**: the same sources directly in the
  app (logins in the platform's protected store).
- **Internet radio**: stations (also from .pls/.m3u), Icecast/Shoutcast
  and HLS, the current song from the stream.
- **Live beat prediction** for radio, other apps, AirPlay and DLNA: the
  effects land on the coming beats; the delay of the speakers (e.g.
  Bluetooth) is measured by tapping along.
- **Receiving**: new DLNA control points must be allowed first; files sent
  over DLNA are analysed ahead; AirPlay 2 (experimental, off by default).
- **Sharing**: "Share" on Android and "Open with" for songs, playlists and
  links: play now, queue or keep in the encrypted files.
- **Playlists**: import M3U/M3U8/PLS/XSPF, export as M3U8.
- **Offline**: whole albums and playlists, a storage limit and an overview.
- **Streaming quality** per network: e.g. the originals on Wi-Fi and Opus
  at 96 kbit/s on mobile data.
- **Speech recognition on the device**: models downloaded in the app; plain
  song texts timed word by word, missing ones written down.
- **Interface**: settings in categories with search, a "More" entry on
  phones, a first start guide, devices page, music sources and radio tab.

### Notes for the beta

- AirPlay 2 has not been tried with Apple devices yet (no FairPlay, no PTP
  timing); AirPlay 1 keeps working and stays the default.
- SFTP supports password and unencrypted Ed25519 key logins with modern
  algorithms only (no very old servers).
