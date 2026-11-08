# Changelog

## Unreleased

- **Android, no more "not responding":** with any accessibility service on (a password manager
  like Bitwarden, KDE Connect's remote mouse), Android's main thread spent minutes waiting for
  Qt's answers about the app's controls. The keyboard did not come up, Android offered to close
  the app, and sometimes it crashed. The app now only talks to such services when a screen
  reader or similar assistant runs (Settings has a switch for others).
- **Less work per frame on Android:** at most 60 frames a second and 30 behind a page, and the
  app's state is put together far less often. With a long queue that was a large part of the load.
- **Swiping:** sideways through the pages and the tabs of Visuals, back out of a detail page.
- **Tablets:** Visuals uses the width of the screen in two columns, the spectrum is wider and the
  shade behind the title reaches the window's edges. The keyboard no longer opens by itself with
  the search page on touch screens, and a genre tag like "Genre: Rock" is just "Rock".
- **Back in a folder** goes one folder up instead of closing the page.
- **Dialogs in German:** their buttons said "Cancel", "OK", "Close" and "Save" in English.
- **A page's own menu on touch screens** (⋮ of a playlist, album or artist) is headed by its name.
  With a single song in it, it looked like the menu of that song.
- **Covers that did not show:** some taggers write a stray byte or the picture's description
  before the picture itself. Its start is found now, as other players do, and such covers kept
  from before are read again.
- **Names like "FrÃ©dÃ©ric":** many taggers write UTF-8 but mark it as Latin-1. Such text is read
  as UTF-8 now, and songs already in the library with such names are read again at the next scan.
- **An MP3 with junk before its music** (about 19 KB in a real file) is recognised and plays; the
  check looked only at the first 16 KB. Songs that could not be read before are read again at the
  next scan.
- **Titles that start with a number:** a tagged title with an artist stays as it is even when the
  file is named the same. "12 Millionen" by DAME no longer turns into "Millionen".
- **Now playing:** the favorite button fades out with the controls, and the spectrum moves with
  the title when the controls come and go (it jumped behind at 20 steps a second).
- **To the edges on Android:** the app reaches under the status bar and the notch, and the window
  keeps its size when the bars come and go. On Android 16 the bottom of the app no longer hangs
  off the screen once the bars come back, and the keyboard lifts the app by its real height.
  Dialogs move up above the keyboard instead of disappearing behind it. The room of the bars is
  kept once: the top had it twice, and a strip under the player bar showed the visual through.
- **Mixes:** tapping one shows its songs first; the button on the cover starts it right away, and
  back leads to the start page.
- **Start page:** its rows are only made anew when something in them changed (they kept jumping
  back to their start), and the covers of the top cards sit straight in their cards.
- **Far less power on Android:** the visual is drawn at about a million pixels and fewer behind a
  page, instead of the whole screen. On a Galaxy Tab S8 Ultra that gives a steady 60 frames a second
  instead of 46 with the graphics chip at its limit, and about half the current from the battery.
  Settings, Appearance, can turn this off. Also: fewer frames when paused, behind a page or with the
  system's battery saver, a look at the state 30 times a second instead of 60, and audio in the
  system's power saving mode.
- **Music is stored once:** the same music file in several accounts' encrypted files takes up
  the space of one, and it stays encrypted. The server can see that two accounts hold the
  same file (`docs/ENCRYPTION.md`); other files are never put together.
- **Trash:** what you delete stays in the trash for 29 days (Library, Trash) and can come back.
  A file goes when nobody needs it any more, 30 days after the last account let go of it, so
  you cannot tell whether someone else still has it.
- **Remove from library:** a song disappears for your account only. A song of an extension is
  only in the libraries of those who added it, and goes 30 days after it left the last one.
  Songs from the server's own folders are never deleted and can always come back.
- **More from server extensions (protocol 2, `docs/EXTENSIONS.md`):** what an extension
  offers is in its manifest. Rows of suggestions on the start page, collections that open
  like an album, a radio for a song, and search results next to the library's. Extensions
  of protocol 1 keep working and only search.
- **Add to the library without playing:** a button on every result of an extension, in its
  menu, and "Add all to library" for an album. Added like this, a song stays.
- **Songs of extensions in the queue:** they have a menu now, can be added to the queue and
  only get imported when their turn comes. A song that cannot be fetched leaves the queue.
- **Songs nobody plays:** songs an extension downloaded by playing them are deleted after 90
  days without being played, to save storage. Those added on purpose or in anyone's playlist
  or favorites stay, and the library's own songs are never touched. All of it can be changed
  in the admin area (0 days: never). Songs imported earlier get the full 90 days from the
  first start.
- **Status in Discord (Linux, Windows):** shows the song you are listening to in the
  Discord program on the same computer (Settings, Playback; off by default). Needs the
  Application ID of a Discord application of your own. Only title, artist, album,
  start and end time and that application's picture go out, and only to the Discord
  program: no cover links, no account token. Encrypted songs and what other devices
  send here (AirPlay, DLNA, other programs) never show up, and pausing removes the status.
- **Now playing beside pages:** in wide windows the visual stays at the side, sharp and
  whole, while a page is open (can be switched off, Settings, Playback). With the queue
  open, the visual fills what the queue leaves free.
- **Library:** in wide windows its sections sit at the side, with filters for the songs
  and what you pinned there (albums, artists and labels, from their menu).
- **Start page:** a dense grid of what you last played and your favorites, and chips for
  moods and genres at the top.
- **Several songs at once:** Ctrl and Shift with a mouse, a long press with a finger. A bar
  then plays them, adds them to the queue or opens their menu.
- **Recent searches** before you type, kept on this device and easy to clear.
- **The queue leaves the visual in sight:** it opens as a panel at the side, on a narrow
  window as a sheet over the lower part, instead of as a page over everything.
- **Song sheet on phones:** the menu of a song is now a sheet from below, with the song
  on top, big rows and the way to its album and artist.
- **Current line in the player:** with a timed song text, the line being sung appears
  under the title instead of the artist (Settings, Playback; on by default).
- **Swiping on the mini player** goes to the next or the previous song.

## 0.12.0 (beta)

A start page with recommendations and statistics, an equalizer and bit-perfect
playback, artist pages and smart playlists, and a more compact interface.

### Sound

- **Equalizer:** ten bands from 31 Hz to 16 kHz, a level control, a few starting
  points and the curve that results (Settings, Playback). Boosts lower the level
  by themselves so that nothing clips. It applies at once, also on speakers in
  the network, and stays on this device. Switched off nothing is computed.
- **Bit-perfect playback (Linux, Windows, Android 14 and newer):** hands the music on
  exactly as it is in the file, at its own rate (Settings, Playback; off by default).
  Songs of the same format stay gapless, another rate means a short gap. Equalizer,
  normalizing, crossfade and skipping silence are off meanwhile. The ways there:
  - **Through PipeWire (Linux),** the default: other programs stay audible. The sound
    server runs at each song's rate. Exact as long as nothing else makes sound.
  - **A sound card alone (Linux):** the sound server is asked to let go of the card, and
    after 30 s of pause it gets it back. Other programs have no sound on it meanwhile.
    Exact whatever else is going on.
  - **A sound card alone (Windows):** WASAPI in exclusive mode, on every output except
    Bluetooth, also given back after 30 s of pause. Windows has to allow exclusive use
    of the device; if it does not, or another program holds it, playback stops with that
    message instead of going through the mixer.
  - **A USB sound card (Android 14 and newer):** the system's bit-perfect mixer, where
    the device has one. A track that leaves the device stops instead of playing on.
- **Signal path:** says what really happens to the music, from what opened and not
  from the switch: the file, the output and whatever changes the samples, for
  example a card that cannot do 44.1 kHz, a volume below 100 % or another program
  making sound. Through PipeWire it looks at the graph and the card every few seconds.
  "Bit-perfect" only appears when all of it holds. Lossy files never get that word. Below
  Android 14 the settings say the direct path is not available; in the browser there is none.

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

### Smart playlists

- **Rules instead of songs:** genre, mood, decade, tempo, key, language, record label,
  your own labels, artist, year, rating, favorites, how often and when played, when
  added; all rules or at least one, each also as "is not", with an order and a limit
  (Library, Playlists). Worked out again whenever you open it, so new songs that fit
  join by themselves. The rules follow your account like other settings.

### Song texts

- **TTML files** next to a song or added by hand work like LRC ones, with word times,
  voices and backing voices.
- **The most finely timed text wins:** word times beat line times beat plain text, no
  matter whether they are in a file next to the song or in its tags. Extensions may
  be written in any case (`.LRC`).
- **A warning when a text does not fit:** when its lines go on well after the song
  ends, or it says it was timed for another length.
- **Plain texts are timed closer:** a line whose first words the model did
  not recognise no longer starts at its first recognised word, up to two
  seconds late; it starts about where its first word is sung (on the test
  songs about 85 % of the lines within half a second instead of 79 %, with
  and without a GPU). Lines also no longer start up to a third of a second
  too early when the timing picks up after a longer stretch.
- **Skipped choruses:** when the model leaves out a repeat of a chorus, its
  lines are placed where the recognition leaves time for them, and lines
  that were not heard spread over a long gap instead of bunching up at its
  start.

### Song metadata

- **More from the tags:** ISRC, MusicBrainz ids, tempo, key (also Camelot and Open
  Key), mood, record label, language, original year, composer and rating are read
  from all tag formats, and from Navidrome and Jellyfin where they tell them. Tempo,
  key, mood, language and label become labels, so devices without a server and
  songs of other sources get them too. What the server hears wins over the tags.
- **Genres meet:** "Hip-Hop", "hiphop" and "hip hop" are one genre now, and tags like
  "favorites" or "seen live" are not genres. Clear ratings count for your taste.
- **Analysis version:** when the sound analysis improves, the server works songs
  out again by itself instead of keeping old results until the file changes.
- The library is read once more after the update to find all this.
- **Artist information from the internet (off by default):** the setup asks once, and
  the settings can change it at any time. With it on, artist pages show a biography
  (Wikipedia), a picture (Wikimedia Commons), where they come from, members, links and
  similar artists (ListenBrainz), and the artists' genres (MusicBrainz) feed
  recommendations and labels. Names of artists and your IP address go to those
  services, nothing else; off, none of them is asked. An artist is only shown when the
  match is certain, and marked when several share the name. Looked up in the background
  at the pace the services ask for and kept for 30 days; "Forget what was found" deletes it.
- **On the server** the same has its own switch in the admin area (`onlineMetadata`,
  `--online-metadata`, off by default) for its whole library and for everybody on it.

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

### Sizes and texts

- **Compact with a mouse:** on a computer, rows, buttons, dropdowns, text fields, the
  player bar, the navigation and the space between them are smaller. Dropdowns are as
  wide as their longest entry. Phones and the web app keep the sizes for fingers.
- **Shorter texts in the menus:** settings, setup and signal path say what matters in a
  line. Where more needs saying, for example what goes out to online services, it is
  behind a small info button.
- **Wide windows:** settings and visuals options no longer stretch across the window.

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

### Apps

- **Windows now uses Qt 6.11.3** (before: 6.10.3), like Linux, Android and
  the web client. Qt's security issue CVE-2026-114366 is in the Qt WebSockets
  module, which Beatlumen neither uses nor ships, so the apps were not
  affected.

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
