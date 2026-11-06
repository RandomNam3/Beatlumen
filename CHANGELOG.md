# Changelog

## Unreleased

### Sound

- **Equalizer:** ten bands from 31 Hz to 16 kHz, a level control, a few starting
  points and the curve that results (Settings, Playback). Boosts lower the level
  by themselves so that nothing clips. It applies at once, also on speakers in
  the network, and stays on this device. Switched off nothing is computed.

### Start page and favorites

- **Start page:** the app opens on a start page with what you last played,
  what is new in your library, your favorites and where you left off.
  Started with songs or in fullscreen it still opens on the visual.
- **Favorites:** a heart on every song (list, player, now playing, song
  menu, Ctrl+L) and an automatic playlist. Favorites among the server's
  songs follow your login to your other devices, also when set offline.
- **Listening history:** kept on the device. A song counts after 30 s or
  half of its length, less counts as skipped. Sending it to the server is
  off until you agree (Settings); only the server's songs and only from
  then on, local and encrypted files never leave the device. Can be deleted
  at any time.

### Labels and recommendations

- **Labels:** the server works out tempo, key, energy and a rough mood of the
  songs in its music folders in the background (one song at a time on half of
  one core; off with `--no-auto-labels` or in the admin page), and labels come
  from genre tags and years too. Your own labels are given in the song menu,
  per login. In the library under *Labels*.
- **Recommendations from the server:** *For you*, *Because you listened to…*,
  *New for you*, *Not heard for a while* on the start page, each suggestion
  with its reason, from your favorites, playlists and, if you agreed to send
  it, your listening history. Without a server the app does the same with
  tags and the history of the device.
- **Your mixes:** personal playlists per genre or artist you listen to, a
  discovery mix and forgotten favorites, the same for a day.
- **Radio:** from a song, album, artist, label, or *My radio* from your taste;
  goes on by itself, close to where it started, with artists taking turns.
- **Endless playback:** when the queue runs out, similar songs follow (a
  switch on the queue page).

### Statistics

- **Statistics page:** listening time, songs played and skipped, days with
  music, days in a row, top songs, artists, albums and genres, listening time
  over the period, by time of day and by day of the week (each also as a
  table), for 7 days, 30 days, a year or all time, and the library at a glance.
  With the agreement to keep the history on the server also across all devices
  (`GET /api/stats`).

### Navigation

- **Slimmer menu:** Home, Search, Library and Visuals; statistics, devices,
  settings and help under *More*. The queue, the output and the visual are in
  the player bar (or click the cover).
- **Search has its own page:** artists, albums and songs, and labels to browse
  before you type.
- **Settings by category:** at the side on wide windows, the search looks
  through all of them.
- **Help:** the keyboard shortcuts at a glance.
- **Phones held sideways:** the navigation fits, the player bar keeps the
  title and hides the volume slider and the time instead.

### Visuals and epilepsy mode

- **Endless animations without pictures:** Flow, Tunnel, Stars, Aurora and
  Waves, following the music, or a new one per song. The effects work on them
  like on pictures.
- **More displays:** frequency wave, mirrored bars and a circle next to
  spectrum and waveform, as a strip below the title or filling the visual.
- **Epilepsy mode:** brightness can only change slowly at every point of the
  picture (at most 2.5 flashes per second are possible, WCAG 2.3.1 allows
  three), saturated red is held back, flash, glitch and hue effects and hard
  cuts are off, transitions only blend. This works for every preset. It lowers
  the risk but cannot rule it out.

### Setup and accessibility

- **Setup at the first start:** language and epilepsy mode first, how it looks,
  where the music comes from, the listening history (with a server), and a
  short tour of the app. Every step can be skipped, and the whole can be left
  at any time; under More → Help you can start it again. Who has used the app
  before gets a hint on the start page instead.
- **New options:** reduce motion, high contrast, keep the controls visible and
  the size of everything, under Settings → Display.

### Accessibility, checked in the running app

- **Keyboard:** Tab reaches everything, also while the controls are hidden
  (they come back), with a clear outline around the focus; Space and the arrow
  keys go to the control reached by keyboard instead of pausing or seeking.
- **Screen readers:** song rows, switches, chips and the small player have
  roles and names, the position slider is no longer named in English only, and
  a new song is announced.
- **Readable:** the faintest text reaches 4.5:1 in both themes, accent colours
  are adjusted for text, the title over the visual has a scrim.
- **Reduce motion** also ends sliding and fading in all pages.
- **Small windows:** phones held sideways get the small player instead of big
  controls that covered the title; the window can be made lower.

## 0.11.1 (beta)

Song texts are timed more reliably, and can be timed and corrected by hand.

### Song texts

- **Tap along and correct by hand:** in the song text menu. Paste a text
  and tap where each line starts while the song plays (Space or the big
  button, Backspace goes back a line), or move single lines by a tenth of
  a second, set them to the current position and edit their text. Undo,
  and saved like an added song text (on the server for its songs); word
  times move with their line.
- **Existing LRC files are checked against the song:** an LRC made for
  another release (a longer intro) is moved to where the lines are sung
  before its words are timed (it was 3 s off for one test song, and every
  word ended up squeezed at its line's end).
- **Plain texts are matched more reliably:** lines are anchored by phrases,
  not single common words; backing vocals and repeated choruses no longer
  pull lines to the wrong place; lines that were not heard are searched for
  between the found ones. The song text is no longer given to the model as
  context (it made the model copy the text and skip verses).
- **Syllables:** long words fill syllable by syllable (hyphenation patterns
  for English, German, French, Spanish, Italian, Dutch and Portuguese).
- **Written-down texts:** long lines are split where the singer breathes,
  get capitals, and spelled-out words of the title are spelled like it
  ("F-R-I-N-D-S" becomes "F-R-I-E-N-D-S"). `*...*` sound notes are dropped.
- **Tidier song text view:** translation and one menu with the other
  actions instead of loose buttons.

### Other

- Release workflow: the APK signature check works with the newer
  `apksigner` on the build machines.
- Built natively but not used by default, because they measured worse on
  the test songs (see `docs/SPEECH-NOTES.md`): separating the singing from
  the music (`bl-sep`) and word alignment with wav2vec2 models
  (`bl-align`).

## 0.11.0 (beta)

The first published beta. Song texts to sing along with: every word (and
long note) fills while it is sung, who sings is shown, and the text can be
translated. It also contains everything listed under 0.10.0 in
[CHANGELOG.md](https://github.com/RandomNam3/Beatlumen/blob/main/CHANGELOG.md), which
was not published on its own.

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

## 0.10.0 (beta, not published on its own)

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
