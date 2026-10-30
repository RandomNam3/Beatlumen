# Changelog

## 0.11.0 (beta)

Song texts to sing along with: every word (and long note) fills while it
is sung, who sings is shown, and the text can be translated.

### Song texts

- **Word timing from the audio**: words get their start and end from the
  singing (attention alignment refined by the audio's loudness), long
  notes are split into pieces, pauses stay pauses. The song text view
  fills each word or piece from left to right while it is sung.
- **Existing LRC files** with line times can now be timed word by word
  ("Time word by word"); the line times are kept as anchors.
- **Plain texts are timed more exactly**: lines are found by whole
  phrases, lines that were not heard are searched for between the found
  ones, and backing vocals do not mislead the matching (test song:
  77 % of the lines within 0.5 s, before 45 %).
- **More reliable recognition**: no-speech detection, the previous text
  as context, and passages that come out unreliable (repetition loops,
  unlikely words) are decoded again with more randomness.
- **Voices**: names before lines ("Anne-Marie:", "M:", "F:", "D:") and
  backing vocals in parentheses are shown in their own colours.
- **Where a text comes from** is shown above it (LRC file, music file,
  timed or written down by speech recognition, on the server or on this
  device, and with which model).
- **The whole library** at once: "Time all songs on this device" in the
  app; the server can do it for its library on its own (admin
  interface). Timed files name
  the speech model (`[re:Beatlumen speech timing 2, model …]`), so songs
  are done again only with a better model.
- **Fewer, better speech models**: all models were compared on real songs
  (writing the singing down without a text, timing a known text). tiny,
  small-q5_1 and medium-q5_0 did worse than others of their size or
  speed and are no longer offered (a downloaded one keeps working).
  Recommended: small; large v3 turbo is best (and slow without a graphics
  card), base-q5_1 is the small and fast one.
- **Translation** (apps on Linux, Windows and Android): each line in the
  app's language under the original (German or English, from English,
  German, Spanish, French, Italian, Dutch, Turkish and Russian). Own
  reader for the OPUS-MT models (about 300 MB each, downloaded on first
  use and checked against their SHA-256); translations stay on the device.

### Speed

- Speech recognition uses AVX2/FMA (x86) and NEON (ARM) when the processor
  has them; the attention works in blocks.
- **GPU**: the audio encoder of the speech model runs on the graphics card
  (Vulkan; Linux, Windows, Android) when there is one. The first window
  is also computed on the processor: the graphics card is only used when
  it gives the same result and is faster. Settings and the admin
  interface show where it runs. `BL_ASR_GPU=0` turns it off.

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
