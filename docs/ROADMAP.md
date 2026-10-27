# Roadmap

Planned after the first release (requested by the project owner). The rule
that apps make no network connection except to the configured server stays in
force: provider access (YouTube, SoundCloud, Nextcloud, …) runs on the server
whenever one is connected, and directly from the app only when the user
explicitly enables it for app-only use.

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
- Storage connectors: Nextcloud (WebDAV) and other providers, to fetch music
  and other files from there or to work directly on the remote storage.

## Providers
- Download from YouTube, SoundCloud and similar, in the app and optionally on
  the server.
- Subscribe to people's feeds (channels/profiles), automatic download of new
  uploads.

## Offloading
- Everything works in the apps without a server.
- With a connected and logged-in server, heavy processes can run on the server
  instead (analysis is already supported; later: lyrics recognition,
  downloads, transcoding, library indexing).
