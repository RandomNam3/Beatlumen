# Changelog

## Unreleased

- **Source extensions can opt into playback feedback** (`playback`, protocol 2): actual
  player progress, pause, seek and end observations go back through the server,
  including songs already imported into the library. Imports and downloads do not
  count as plays. Existing extensions and older apps keep working.
- **"Where should the song stay?"** The question at the first song of an extension says
  plainly what each place means: the library on the server holds a normal, unencrypted file
  that only shows in your library and that the server analyses for visuals, song texts and
  karaoke (recommended); your encrypted files are encrypted on the device, so not even the
  server can read them. It said before only who gets the song, which read as if both were
  encrypted.

- **Signing in with an account:** a server that wants a login now offers *Sign in* with
  user name and password first, and *Create account* next to it; the server's shared
  password is the way without an account. Before, *Log in* asked only for the shared
  password, and the account was a separate step further down.
- **Invitation links:** the admin area shows a link next to each new invitation code
  (`https://server/#invite=CODE`, with the public address when it is set). Opened in the
  browser, or pasted into the app as the server address, it connects and opens account
  creation with the code filled in; the browser removes it from the address bar. The steps
  of account creation are spelled out, including the recovery code.

- **Web client plays sound:** browsers keep Web Audio switched off until the page is
  touched, and the app's switch-on listened to events that Qt's canvas never let through.
  It now listens before Qt does, on every touch, click and key, and again when playback
  starts. Without a secure page (`https://` or `localhost`) the browser offers no audio
  worklet; the console says so instead of failing silently.
- **Start page:** tapping a song in a suggestion row ("Für dich", "Zum Entdecken") did
  nothing (since 0.12.0); it plays again.
- **Songs from extensions play at the first tap:** a FLAC or MP3 song that is still being
  imported has no known length yet, and opening it failed with "io error: length not known
  yet" (QBZ Light delivers FLAC); a second tap worked once the import had finished. Such
  streams open now, and the player gets the real format from the server.
- **Cover as background with songs from extensions:** it asked the server for a library cover
  the song does not have yet and showed "Could not load background image" twice per song.
  It uses the extension's picture now, and a song without a cover gets the default
  background without a message.
- **Sign-in behind a reverse proxy:** a proxy that rewrites the `Host` header (nginx's
  default, or one that drops a non-standard port) made browser sign-ins and other writes
  fail with 403. A trusted proxy's `X-Forwarded-Host` and the public address now count as
  the server's own origin.

## 0.13.0 (beta)

Karaoke with points and phones as microphones, listening together, server data
in a database, protection against crafted files, backgrounds up to 16K (AVIF,
HEIC, animations), and song texts timed by the speech model's timing layer.

- **Karaoke page** (under *More*): pick a song, compute or import its notes, give each singer a
  microphone (and a voice in duets) and sing. The note bars, the text and each singer's pitch are
  drawn as the song plays, the points at the end. Microphones are only open during a round.
- **Phones as wireless microphones:** on the karaoke page a phone (or any other device logged in
  to the same server) sings into the device that plays. Its singing goes as Opus packets through the
  Beatlumen server, each stamped with the server's clock when it was recorded, so points go by when
  it was sung, not by when it arrived. No device opens a port, and the server keeps nothing.
- **Microphone delay measured:** the karaoke page plays a few clicks; the microphone hears them
  through the speakers (or the singer claps or sings "ta" on each), and the delay found is kept as
  the input's latency offset.
- **Singing in the song quieter or off** during karaoke: the singing is separated from the music
  once per song (`bl-sep`) and taken out on the way to the speakers, sample by sample at the song's
  position, also after seeking (formats that do not seek to the exact sample are matched up again).
- **Points for the text:** each sung line is scored against the known line by the small CTC head
  on the speech model's encoder, on its own thread one line after the other (about 0.1 s a line
  with the base model on four cores). It uses the picked speech model when it has a head (also
  one of the server's own models), else the smallest one in the models folder that has one;
  without any there are no text points and the other points count for all 10 000.
- **Points for the pitch:** each singer's live pitch against the notes of their voice, the octave
  does not count. The tolerance depends on the difficulty (2, 1 or half a semitone), golden notes
  count double, rap notes count when the singer is heard, and a long note held on pitch earns a
  bonus. Consonants and breaths within a note are no misses (up to 40 % of it). Each line gets a
  rating as it ends.
- **Karaoke data per song:** the melody of the singing as one note per syllable of the timed song
  text, with the key, from the vocals separated from the music (`crates/bl-karaoke`). The server
  computes it on request or for the whole library in the background (`--karaoke`,
  `--auto-karaoke`; turning it on downloads the vocal separation model, 36 MB; new songs right
  after a scan; also in the hardened container) and keeps it like song texts (`GET/PUT/DELETE
  /api/tracks/<id>/karaoke`). The app computes it itself where the server does not, keeps it in a
  song's offline copy, and imports and exports UltraStar text files (duets included). Computed
  notes are dropped when the song text changes; imported ones stay. For a song text with line
  times only, each line's syllables are put on the time it is sung.
- **Artist and album information stays:** an artist page asked for once is shown at once
  from then on, also after 30 days: it is asked for anew in the background when it is opened,
  in the language of whoever opened it. Before, looking through the library replaced an old
  page with the genres alone, so the page had to load again, and the server showed nothing
  until it had asked anew. A picture that did not change is not fetched again, and a page stays
  when the services find nobody the next time. Albums, playlists and rows of suggestions from
  extensions are kept on the server's disk: they appear at once, also after a restart, and are
  asked for anew in the background (albums after an hour, suggestions after 15 minutes). Their
  pictures stay until nobody looked at them for 30 days instead of 7.
- **Smooth transitions:** pages, tabs (e.g. Effects → Backgrounds), settings categories and
  library sections come in with a short fade and slide (about 0.2 s), pushed pages slide in from
  the side, menus and dialogs grow and fade, bottom sheets slide up, the page panel fades over the
  visual, and the rows of the start page fade in when it first fills. All share one set of
  durations and curves and only move opacity and transforms, so nothing is laid out again.
  "Reduce motion" turns them off; the photosensitive mode and the system's battery saver keep
  the fading and leave out sliding and growing (shorter on the battery saver). While an
  animation runs, phones briefly draw at the full frame rate even where frames are otherwise
  rationed (paused, a page over the visual).
- **Podman secrets with a read-only container:** the Quadlet unit's password and pepper did
  not start (Podman mounts `/run/secrets` itself and cannot add files to it when the container
  is read-only). The unit and the README put them into `/secrets` now.
- **Server container, stricter and smaller:** the image holds only the static server (musl, no C
  library, no shell) and the license texts, about 9 MB; it is built without Alpine (the Debian Rust
  image compiles it). A seccomp profile allows only the system calls the server uses: no executable
  memory, no new namespaces, IPv4/IPv6 sockets only (`container/seccomp/`, used by `compose.yml`
  and the Quadlet unit, also `--ipc none`). The server stops on SIGTERM itself, so the container
  needs no init process and `docker stop` / `podman stop` take a second or two instead of the
  timeout. `GET /api/health` (no login) reports whether the server works and its data folder takes
  writes; `--health-check` and the image's health check use it. The Quadlet unit's health check
  works now (it ran through a shell the image does not have).
- **Background folders stay in sync:** a folder added to the backgrounds is followed. New
  images in it join the list, deleted ones leave it, and a changed image is shown anew. The app
  looks on return (Android), when the list is opened, and every 20 seconds while it is on screen.
  Images taken off the list stay off. The synced folders are listed below the images; removing
  one takes its images off the list, and the files stay where they are.
- **No background twice:** an image already in the list is not added again, also when it is a
  copy with another name or comes both from a folder and on its own. Files are compared by
  content, and only when their sizes match. A picked duplicate becomes the current background
  instead, with a note.
- **Selecting backgrounds:** a long press no longer removes an image at once. It starts a
  selection with check marks, "Select all" and "Remove from the list", which asks first and
  never deletes a file. The select button above the images does the same; it replaces the button
  that emptied the whole list without asking.
- **Server: large bodies only with a login.** Without a login a request may send 64 KiB.
  Larger bodies (4 MiB, image uploads 257 MiB) need the server password, an account or the
  admin login, and that is checked from the request's head before any of the body is read:
  before, anyone could make the server take in a 257 MiB upload before it answered 401. Clients
  that send `Expect: 100-continue` get the answer before they send the body.
- **Server behind a reverse proxy:** a new setting names the trusted proxies
  (`--trusted-proxy`, `BEATLUMEN_TRUSTED_PROXIES`, Admin area → Network; default: the same
  machine, `none` for no proxy). Only their `X-Forwarded-For` names the client, and each of
  their requests counts against that client while it runs. The loopback interface is no longer
  exempt from the 32 connections per client, which let everything behind a local proxy through
  without a limit. The server's own health check still gets through when the limits are
  reached, and it warns once when another address sends `X-Forwarded-For`. Wrong room codes
  and admin logins count IPv6 clients per /64 now, as server logins did.
- **Server keeps its places after internal errors:** a panic while reading a request or while
  a body streamed (such as the Opus conversion for mobile data) ended the connection without
  giving its place back, so enough of them made the server refuse everyone. Places are given
  back now in every case, and a panicking handler answers 500. Background work (scanner,
  sources, rooms, labels, artist pages, song text timing, the trash) logs a panic and starts
  again after a pause instead of stopping for good; a failed extension import reports its error
  instead of running forever.
- **Device control and rooms with limits:** a command may be 16 KiB (it was up to 4 MiB, with
  1000 of them buffered), a device has at most 64 commands (256 KiB) waiting that it has not
  fetched, all events together at most 32 MiB, a login at most 50 devices (the one offline the
  longest makes room) and 10 rooms. Room requests are at most 16 KiB and read before the rooms
  are locked.
- **Apps read at most 64 MiB of an answer** unless the request says otherwise (it was 1 GiB):
  256 MiB for the server API, whose library of a large server comes in one piece, and 32 MiB
  for the cover and song text of an offline copy. Songs and models stream into files as before.
- **Apps hardened against crafted files and network data:**
  - Every picture from outside is decoded by Beatlumen's own image decoders, which have size
    and memory limits: covers in music files and next to them, covers from the server, from
    AirPlay and DLNA senders and from other apps' playback, artist pictures and backgrounds.
    The apps no longer ship Qt's JPEG, GIF, WebP and other image readers (only SVG, for the
    app's own icons). Android's media notification and the desktop's media controls get
    pictures made from the decoded pixels instead of the original files. A damaged picture
    shows the placeholder.
  - A bug that makes the core panic no longer ends the app: calls from the interface, worker
    jobs, the decoder, the library scan and the network listeners catch it. The work in
    question ends with an error (a song is skipped, a cover stays empty), audio is silent for
    a moment, and the app says once that an internal error happened.
  - The live stream for DLNA speakers and Chromecast checks its secret address before
    reading anything else and takes at most eight connections; requests must arrive within
    10 seconds and stay short. The same for the sign-in page's return to the app, which also
    stops listening after 15 minutes. The DLNA receiver takes at most 16 connections, each
    request within 15 seconds, and reads bodies as they arrive. AirPlay connections must send
    their first request within 10 seconds, and a crash in one no longer uses up a place.
  - Settings, the library and covers are written to a temporary file of their own and then
    renamed, so two writers never mix. Changing the song text of an offline copy writes a new
    copy instead of overwriting its header while the song plays; a cut-off write could
    formerly make the copy look damaged, and damaged copies are removed.
  - On desktops only one Beatlumen runs per data folder: starting it again (for example by
    opening a song from the file manager) hands the files to the running app and brings its
    window to the front.
  - Rust 1.82 or newer is needed to build.
- **Server data in a database; after an update from 0.12.0 it is set up again:** the
  server keeps its own data in one database file in the data folder, `beatlumen.redb`
  (redb, pure Rust and built into the server; no database server). Every change is one
  transaction: it is stored whole or not at all, also when the server or the computer stops
  in the middle, and deleting an account takes everything of it in one step. A play, a
  favorite or a login writes only its own entry, not a whole file. The 0.12.0 files
  (`tokens.json`, `admin.json`, `settings.json`, `playlists.json`, `favorites.json`,
  `history.jsonl`, `labels.json`, `hidden.json`, `sources.json`, `presets/`,
  `vault/users/`, `vault/files/*.json`, `vault/userdata/`, ...) are **not** taken over, and
  the server says so in its log while they are there. After the update set up again:
  the **admin password** (the server logs a new setup link) and the **settings** made in the
  admin interface; **accounts** and **their encrypted files** (shared ones and public links
  included), the **logins** of the apps with the server password, **playlists**,
  **favorites**, the **listening history**, given **labels**, **presets** and **music
  sources**. Settings from the environment or the command line apply as before. What stays:
  the music, the images, song texts (imported and timed ones), the library index, the sound
  analysis, artist information, covers and other caches, speech models and the pepper. The
  stored bytes of 0.12.0's encrypted files (`vault/blobs/`) belong to nobody any more and are
  deleted 30 days after the first start of the new server; to keep them, back up the data
  folder before the update. The old files themselves are left alone and can be deleted.
- **Backups of a running server:** within an hour of a change the server writes a consistent
  copy of its database to `backup/beatlumen.redb` in the data folder, so a backup of the
  data folder taken while it runs holds a complete database (at most an hour old). For a
  copy of the very last state stop the server first. See the container README.
- **Health check:** `/api/health` and `--health-check` also report a database that does not
  take a write transaction.
- **Background images up to 16K:** JPEG, PNG, WebP, GIF and BMP, and now TIFF and animated
  PNG too, in every size from 720p to 16K (15360 × 8640). Large pictures are scaled while they
  are read, so they never exist at full size: a 16K image needs about 16 MB of working memory
  instead of 530 MB, on a tablet as on a PC. Each picture is decoded for the screen (with room
  for the camera's zoom) and the graphics card's limit, and once more in finer detail when the
  window becomes much larger. Formerly pictures were cut to 2560 pixels and 8K files failed.
- **Animated backgrounds:** GIF, animated WebP and animated PNG play behind the visual at
  screen size (up to 1600 × 900 pixels on phones and tablets, 1440p elsewhere), and small
  enough that all frames fit 256 MB (512 MB on computers, down to 960 × 540 for very long
  ones): after the first loop nothing is decoded any more. Frames are decoded ahead on their
  own thread; nothing runs while the app is hidden.
- **Backgrounds from the server:** animations come as they are, still images scaled to the
  screen (up to 8192 pixels instead of 4096). The image folders list TIFF and APNG files,
  files up to 512 MB are read and uploads may be up to 256 MB. AVIF is scaled there like the
  other formats; HEIC files go to the apps as they are, which decode them themselves.
- **AVIF and HEIC backgrounds:** AVIF pictures (also tiled ones and ones with transparency, 8
  to 12 bits, rotated or cropped; HDR pictures are toned down for the screen) are decoded by an
  AV1 decoder of Beatlumen's own, written from the open AV1 specification, on every platform and
  on the server. A 16K AVIF is read with about 60 MB of memory in a few seconds; animated AVIF
  shows its first picture. HEIC pictures (as phones take them) are decoded by the operating
  system where it can, because HEVC is covered by patents: on Android 9 and newer, on Windows
  with the HEIF and HEVC extensions from the Microsoft Store, and on Linux when the system has
  libheif. Beatlumen brings no HEVC decoder of its own.
- **Listen together:** make a room on the Devices page and let others in with its code or
  link. Everyone hears the same song of the server at the same moment; the host decides what
  plays. A song starts when everybody has it (or after 8 seconds, the slow ones catch up), and
  while it plays every device keeps itself in step, in steps too small to be heard. People
  without a login can join as guests: they get this room's songs and nothing else, and only
  while it lasts. The host can turn guests off for a room, the admin for the server (Admin
  area, `--no-room-guests`). The host hears what happens in a new room at once, too: before, that
  took the host's own next device report.
- **Devices playing together** ask for the server's time more carefully (corrected by the
  round trip) and now stay in step while they play.
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
- **Statistics:** songs without an album tag no longer show up as blank rows among the top albums,
  and the genres are written as on the start page: a tag "Genre:" with nothing after it is none.
  A song without a genre tag counts for the first two genres of its artist, when they were looked
  up on the internet.
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
- **Song texts timed faster and closer with a timing layer:** a speech model can come with
  a small timing layer made for it (a `.head` file of a few megabytes next to the model).
  With it, a plain song text is timed from what the model hears in the whole song, without
  writing the singing down first: on 79 songs timed by hand 93 % of the lines within half a
  second instead of 83 %, word starts 0.03 s off instead of 0.1 s, and a 3-minute song in
  about 18 s instead of 43 s on 4 processor cores. A text that does not fit the song is
  timed the old way. Songs without a text are written down as before, and the layer times
  the words: of the lines whose first word was recognised, 94 % start within half a second
  instead of 84 %. The models offered for download have no such layer yet.
- **Speech models from your own server:** the server offers the speech models in its models
  folder to the apps signed in to it, also own ones: a `ggml-<name>.bin` with the timing layer
  made for it (`ggml-<name>.head`) and, if wanted, a `ggml-<name>.json` with a note, a rank and
  the memory it needs. The apps list them next to the public ones (Settings, Song texts),
  download them through the server connection and check each file against the checksum the
  server lists; a public model the server has comes from the server too, without the internet.
  Own models count as better than the public ones, so songs timed with those are timed again.
  The admin page uses and deletes own models as well; offering is on by default and turned off
  there, with `--no-offer-models` or with `BEATLUMEN_OFFER_MODELS=false`.
- **Damaged and hostile files end in an error, not a crash:** song texts (LRC, TTML),
  playlists, karaoke files (UltraStar, JSON), JSON from servers and devices, Ogg streams,
  animations, speech models and their timing layers (`.head`), UPnP answers and AirPlay
  messages. Sizes in a file are checked against what the file really holds before memory is
  taken, so a speech model from a server can no longer make an app reserve gigabytes for a
  small file. New limits: Ogg packets up to 64 MiB, JSON objects up to about a million keys
  and arrays up to 16 million items, song texts up to 5,000 lines of 4 KiB, karaoke files up
  to 10,000 lines and 100,000 notes, animations up to 10,000 frames (longer ones loop
  earlier). JSON objects with many keys and long runs of `<` or `&` in TTML, XSPF and UPnP
  answers no longer take quadratic time (400,000 keys: 76 ms instead of minutes). Song
  texts with times like `[00:01.abé]` and timing layers whose sizes do not fit each other no
  longer crash the app. The parsers of outside data (except images) keep overflow checks
  in release builds, at no measurable cost.

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
