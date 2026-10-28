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

## Third-party services

Evaluated October 2026. The visuals need the audio itself (beat and
spectrum analysis), so a service only fits fully if it hands out the audio
stream and allows analysing it.

- **Spotify: not as a music source.** The Web API gives metadata, library,
  playlists and remote control (Spotify Connect), never the audio; playback
  in third-party apps runs through DRM-protected players (Web Playback SDK,
  Premium only). The Developer Policy forbids analysing Spotify content and
  synchronising recordings with visual media (section III.6 and III.13), and
  the audio analysis endpoints are closed to new apps since November 2024.
  Since February 2026, Development Mode apps need a Premium account of the
  app owner and allow five users; extended quota is only for organisations
  with 250k monthly users. What would remain possible:
  - Import of Spotify playlists as lists of titles, matched against the
    own library (metadata only).
  - Remote control and "now playing" display for a Spotify Connect device,
    without visuals (owner needs Premium, at most five users per app key;
    with an open-source app each user can register their own app key).

  Beatlumen as a Spotify Connect device (listed in the account like a
  speaker, as smart speakers and assistants are): officially only through
  Spotify's embedded SDK, which is limited to companies with an NDA,
  certification and a distribution agreement. The unofficial route,
  librespot (Rust, MIT), would hand Beatlumen the decoded audio, but it
  needs Premium, breaks Spotify's terms of use (reverse engineering; any
  analysis, however limited, is still analysis), risks the user's account,
  and in October 2026 Spotify refuses its audio keys for some Premium
  accounts. Not planned.

  Practical combination: the official Spotify app plays, Beatlumen
  visualises the system audio (below) and can show title and cover through
  the user's own Web API key.
- **Self-hosted servers (best fit):** OpenSubsonic API (Navidrome, Airsonic,
  Gonic, …) and Jellyfin: full audio streams, open APIs, no DRM. Configured by
  the user, accessed through the Beatlumen server or, for app-only use, from
  the app.
- **Internet radio:** Icecast/Shoutcast streams and the Radio Browser
  directory; streams can be analysed live like local files.
- **Storage:** Nextcloud/WebDAV (see Server).
- **System audio (independent of services):** visualising whatever the
  device plays (PulseAudio/PipeWire monitor on Linux, WASAPI loopback on
  Windows, Android's playback capture with user consent where the playing
  app allows it), so any player can drive the visuals. Live input only: no
  look-ahead analysis, so effects react instead of anticipating beats.
