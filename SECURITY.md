# Security

Beatlumen handles files and data from sources it cannot trust: music files
(tags, cover art), images, presets and everything a server sends. The
server itself may be reachable by other people on the network. This file
describes the measures taken and their limits.

## Reporting a vulnerability

Please report security problems privately through GitHub's
"Report a vulnerability" (Security tab of the repository) instead of a public
issue.

## Design principles

* **Few dependencies.** Decoders, the HTTP stack, JSON and image codecs are
  part of this repository (Rust, `#![forbid(unsafe_code)]` where possible)
  rather than large third-party libraries; Qt is the only external runtime.
  Cryptography is the exception: the end-to-end encryption uses the audited
  RustCrypto and dalek crates, pinned to exact versions, instead of an own
  implementation. The server also uses rustls for TLS and redb (pure Rust,
  no C code) as its embedded database, both pinned exactly.
* **No connections except the configured ones.** The apps contact only
  the server the user entered; the server connects only to remote sources
  its admin configures and to the push services the users' devices
  registered: no update checks, telemetry or metadata services.
* **Untrusted input is bounded.** Every parser limits sizes before
  allocating (sample tables, image dimensions up to 32 megapixels, JSON
  nesting, preset effects and particles). Panics in the server are caught
  per request. In the apps every edge of the core catches them: calls from
  the user interface (`bl-ffi`), worker jobs, the decoder thread, the
  library scan, speaker outputs and the network listeners. The piece of
  work ends with an error result (or silence, for audio), the app keeps
  running and tells the user once a minute at most.
* **No dynamic code.** The Android app runs QML in the interpreter
  (`QV4_FORCE_INTERPRETER`) and disables the regular expression JIT
  (`QT_ENABLE_REGEXP_JIT=0`), so no code is generated at run time. Both are
  set by `BeatlumenApplication.java` before any Qt library is loaded, since
  Qt's Android plugin already uses regular expressions before `main()`. Qt's QML engine still probes once at start
  whether executable memory could be allocated, which hardened systems such
  as GrapheneOS report as dynamic code loading;
  `apps/beatlumen/android/qt-patches/` removes that probe from a rebuilt
  Qt QML library.

## Server (`beatlumen-server`)

* Optional password; logins issue random tokens (stored as SHA-256 digests
  in the server's database, file mode 0600) that expire after 180 days. Password checks are
  constant-time, serialized and rate limited per address and globally.
* What it stores: its own data (account verifiers and wrapped keys, the
  metadata of encrypted files, sessions and login digests, the admin
  password's verifier, settings, playlists, favorites, history, labels,
  presets, push subscriptions, music sources with encrypted logins) is in
  one database file, `beatlumen.redb` (redb, pure Rust inside the server,
  file mode 0600), changed only in transactions. Encrypted file contents,
  song texts, uploads and caches are files in the data folder. The copy for
  backups (`backup/beatlumen.redb`) holds the same and is protected the
  same way; the account pepper stays out of both.
* Requests: bodies are read as they arrive with per-route limits, slow
  clients are disconnected, connections are limited per address,
  ambiguous request framing (`Content-Length` + `Transfer-Encoding`) is
  rejected.
* Bodies only with a login: without one a request may send 64 KiB. Larger
  bodies (4 MiB, image uploads 257 MiB) need the server password, an
  account or the admin login, which is checked from the request's head
  before any of the body is read.
* Connections: 256 at once, 32 per client address (IPv6 per /64), the
  loopback interface included. Only the trusted reverse proxies
  (`--trusted-proxy`, `BEATLUMEN_TRUSTED_PROXIES`, default: loopback) may
  name the client in `X-Forwarded-For`; each of their requests counts
  against that client while it runs, and the same address counts for
  failed logins, room codes and passwords of shared links. A few reserved
  connections let the local health check through when the limits are
  reached.
* Panics: a panic in a request, while a body streams (an Opus conversion)
  or in reading a request ends that connection only and gives its place
  back. Background threads (scanner, sources, rooms, labels, artist pages,
  song text timing, trash) log a panic and start again after a pause
  (1 s, doubling up to a minute).
* Device control and rooms: a command is at most 16 KiB, a device has at
  most 64 commands (256 KiB) it has not fetched yet, the event buffer of all
  logins holds at most 1000 events of 32 MiB, a login has at most 50
  devices and hosts at most 10 rooms, and room requests are at most
  16 KiB.
* Browsers: cross-site writes are refused (`Origin`, `Sec-Fetch-Site`);
  without a password only IP addresses, `localhost`, local network names
  and names given with `--allow-host` are accepted as `Host` (DNS
  rebinding). Responses carry a strict Content Security Policy,
  `nosniff`, `frame-ancestors 'none'` and no CORS headers.
* Files: track and image ids are opaque hashes; static files, presets and
  uploads are confined to their folders with sanitized names.
* **TLS through a reverse proxy.** The server itself speaks plain HTTP (home
  network, VPN); for access from elsewhere put an HTTPS reverse proxy in
  front of it (see the server README). The apps and the web app connect to
  `https://` addresses.

## Favorites and listening history

* Favorites of songs on the server are stored in plain text with the login
  on the server, like playlists. Favorites of local and encrypted files
  stay on the device.
* The listening history is kept on the device (`history.json`) and can be
  deleted in the settings.
* It only goes to the server after the user agreed in the settings. Then the
  server's admin could read it (in the server's database `beatlumen.redb`,
  `acct/<name>/history/`, or `shared/history/` for the shared server
  password). Only songs of the
  server's library and only plays from the moment of agreeing are sent;
  deleting the history in the app also deletes it on the server.

## Labels and recommendations

* The server reads the audio of the songs in its music folders to work out
  tempo, key and energy (`sound.jsonl`); nothing leaves the server. It does not
  touch encrypted files, and it does not download songs of music sources for
  this. `--no-auto-labels` turns it off.
* Labels given to songs are stored per login in plain text, like playlists
  (those of local files on the device).
* Recommendations are worked out on the server from what it knows of the
  login; radio requests name the songs of the queue, only for that answer.
  Without a server everything happens on the device.

## Accounts and encrypted files

User files on the server are end-to-end encrypted; the design, threat
model and file format are in [`docs/ENCRYPTION.md`](docs/ENCRYPTION.md).
In short:

* Passwords never leave the device: Argon2id (64 MiB) derives an auth key
  for the server and a key that unwraps a random master key. The server
  stores only SHA-256 hashes of the auth keys mixed with a pepper kept
  outside the data folder. Apps refuse weaker Argon2 parameters, so a
  malicious server cannot make the password cheaper to guess.
* Files are encrypted on the device (XChaCha20-Poly1305, 64 KiB chunks
  bound to position and file, sizes padded to whole chunks, encrypted
  names) and signed by the owner (Ed25519, over a commitment to the file
  key). Sharing seals the file key to the recipient's X25519 key; the apps
  pin public keys and show safety numbers to compare in person.
* A forgotten password is replaced with the recovery code shown once at
  registration; without either the files cannot be decrypted by anyone.
* Accounts need an invitation from the admin (single use, expiring).
  Logins and password proofs are rate limited per account and address;
  password changes need the current password and end all other sessions.
* Share notifications use UnifiedPush: the server encrypts each message
  for the device (RFC 8291) and signs it with its VAPID key; the push
  service relays ciphertext only, and the app drops messages that do not
  decrypt. No phone number, e-mail address or Google service is involved.
  The server connects only to the registered endpoints, never to private
  addresses unless the admin allows it (no requests into the server's
  network on a user's behalf).
* What a compromised server can still do: delete or withhold files, see
  who shares how many files of which size with whom and when, and serve a
  modified web client. Against the last one: the server checks its web
  client against a signed manifest and refuses modified files, the page
  pins its scripts and WebAssembly module by hash, and the native apps
  can verify the server's web client against the release signature
  (Settings → Server → Check).

## Parsers and loaders

Everything read from outside goes through parsers in this repository: music
files and their tags, images, song texts, playlists, karaoke files, JSON,
speech models, and what servers and other devices send. Damaged or hostile
input ends in an error, not in a crash or an endless loop:

* **Sizes before allocations.** Sizes and counts from a file are checked
  against fixed limits and against the data really there before memory is
  allocated. Ogg packets up to 64 MiB; after 32 MiB of pages with wrong
  checksums the reader gives up instead of checking every fake page
  header. JSON: 256 levels of nesting, up to 1,048,576 keys per object and
  16,777,216 items per array. Song texts:
  5,000 lines of up to 4 KiB, times within 100 hours. Karaoke tracks:
  10,000 lines, 100,000 notes, 9 voices. Animations: 10,000 frames and
  2^32 pixels per loop. Speech models (ggml) are read in growing pieces, so
  a header that claims gigabytes allocates at most about twice what the
  file holds; the tensors the network uses must have the shapes the
  model's hyperparameters give them (widths up to 4096), and so must those
  of a timing layer (`.head`). Models reach the apps from servers, so this
  protects the apps against a malicious server.
* **Linear time.** Repeated JSON keys are found through a hash index;
  nested tags and runs of `<` or `&` in TTML, XSPF and UPnP XML are read in
  one pass.
* **Checked arithmetic.** Values from files are added and multiplied with
  checked or saturating arithmetic. The crates that parse outside data
  (`bl-media`, `bl-json`, `bl-ogg`, `bl-sources`, `bl-upnp`, `bl-cast`,
  `bl-airplay`, `bl-pitch`, `bl-net`) keep overflow checks in release
  builds, so an overflow is a caught panic, not a wrong size. `bl-image`
  does not (its decoders became up to 11 % slower, scaling 74 %); it relies
  on its size checks and robustness tests.
* **No `unsafe` where it is not needed.** Crates without SIMD or system
  calls forbid it (`#![forbid(unsafe_code)]`); the SIMD code of the speech
  recogniser checks the bounds of every load.
* **Robustness tests.** Every parser gets damaged copies of sample inputs
  (bytes changed, cut short, spliced together, ranges repeated or dropped,
  tokens of the format put in) under `catch_unwind`; a panic or an input
  that takes seconds fails the test. `BL_FUZZ_ITERS=<n>` runs them longer
  (`crates/*/tests/*robustness.rs`).

## Container

* The image (`container/Containerfile`) holds only the static server
  binary (musl, no C library, no shell) and the license texts; it runs as
  an unprivileged user (10001) and is meant to run with a read-only root
  file system, no capabilities, `no-new-privileges`, no IPC sharing and a
  seccomp allowlist of the system calls it uses
  (`container/seccomp/beatlumen-server.json`: no executable memory, no new
  namespaces, IPv4/IPv6 sockets only), as in `container/compose.yml`. It
  needs no init process.
* The password and the account pepper come from files
  (`BEATLUMEN_PASSWORD_FILE`, `BEATLUMEN_VAULT_PEPPER_FILE`; Docker and
  Podman secrets) instead of the environment, which `docker inspect` shows.
* Optional AppArmor (`container/apparmor/`) and SELinux
  (`container/selinux/`) profiles confine it further; see
  `container/README.md`.

## Apps

* Text from tags, servers and presets is shown as plain text (no rich text,
  which could load remote images).
* The login token is stored in the settings file, readable by the user
  only; Android backups are disabled. Tokens are removed from log output.
* Redirects are only followed for reads on the same server, so the login
  token or a password never reaches another host.
* Answers are read into memory up to 64 MiB unless a request states its
  own limit (the server API 256 MiB, for the library of a large server);
  songs, offline copies and speech models stream into files.
* Android: only the launcher activity is exported; files picked with the
  system picker are copied into app storage under sanitized names with
  size limits.
* `https://` servers: certificates must be trusted by the system and match
  the host name (Android: the system's TLS; desktop: Qt's TLS backend). A
  self-signed certificate (common on home servers) is only accepted after
  the user compared and trusted its SHA-256 fingerprint; the trust applies
  to that one certificate and server address. Server images are loaded by
  the app's own HTTP client so the same rules apply to them.
* The app warns when a server outside the home network is used without
  encryption.
* **Pictures are decoded by the core only.** Covers (in music files, next to
  them, from the server, from AirPlay and DLNA senders and from other apps'
  media sessions), artist pictures and backgrounds reach the user interface
  through two image providers, `image://server` (fetched by the core's HTTP
  client, at most 32 MB) and `image://thumb` (local files, at most 512 MB),
  which decode with the core's own decoders (`bl-image`: Rust, limits for
  dimensions and memory before allocating) and hand over plain pixels. The
  packages contain none of Qt's image readers except SVG for the app's own
  icons; the views refuse any other picture address. Android's media
  notification gets decoded pixels (no `BitmapFactory`), the desktop's media
  controls a PNG made from them. Qt's built-in PNG reader only sees PNGs
  made by the core (thumbnails of encrypted images).
* **Listeners.** The live stream for DLNA speakers and Chromecast
  (random 128-bit address) checks the address first, in constant time,
  before reading headers; the request head is at most 16 KB and must
  arrive within 10 seconds; at most eight connections. The sign-in page's
  return to the app listens on 127.0.0.1 only, reads one request line of
  at most 256 KB within 10 seconds per connection and stops after
  15 minutes.
* **Files.** Settings, the library, covers and other state are written to a
  temporary file of their own (process id and counter in the name) and
  renamed over the old one. Offline copies are never changed in place: a
  new copy is written and renamed, and playback reads header and audio
  from the same open file. On desktops a lock in the data folder keeps a
  second instance from running; a second start hands its files to the
  running app over a local socket of the user and ends.

## AirPlay receiver

* Off by default; when switched on, the app listens in the local network
  for AirPlay senders (RTSP over TCP and three UDP ports for the stream)
  and announces itself through mDNS (Android: the system's NsdManager).
* An optional password (HTTP digest, as AirPlay senders use it) keeps
  others in the same network from playing on the device. Without one,
  anyone in the network can stream to it, as with any AirPlay speaker.
* Everything read from the network is bounded: request headers (16 KB),
  bodies (4 MB, cover art, read as they arrive), at most eight control
  connections, which must send their first request within 10 seconds, at
  most eight seconds of buffered audio; UDP packets are only accepted from
  the sender of the current stream. A new sender replaces the current one.
  A panic while handling a connection ends only that connection.
* The audio is decrypted with the per-stream AES key that the sender wraps
  with the RSA key of AirPlay 1 receivers. That key is public (published
  in 2011 and used by all open-source receivers), so it protects nothing
  against an attacker in the network; it only serves compatibility. The
  RSA code is not constant time for that reason, as it only ever uses
  this public key.
* Received audio is played and analysed live, never stored. Cover art is
  kept as a single file in the app's storage while a stream plays.

## DLNA receiver

* Off by default; when switched on, the app answers SSDP searches and
  announces itself in the local network, and serves its UPnP descriptions
  and control endpoints over HTTP.
* UPnP has no authentication: while it is on, any device in the network
  can make the app fetch and play a stream URL and control its playback.
  It reads, lists and sends none of the user's files.
* Requests, SOAP bodies and DIDL metadata are size limited (bodies up to
  1 MB, read as they arrive); at most 16 connections, each request must
  arrive within 15 seconds; event subscriptions are bounded in number and
  only call back to the address that subscribed (no requests to third
  hosts on someone's behalf). The sender's cover address is fetched and
  decoded by the core like every other picture.

## Other apps' playback (Android)

* Only after the user starts it, each time: Android asks for consent to
  record the screen (MediaProjection); the app uses only the playback
  capture of other apps' audio (no microphone, no screen content). A
  notification shows while it runs and stops it.
* The audio is analysed live for the visuals and never stored or sent.
  Apps that opt out of being recorded are not recorded.
* Titles, covers and controls of the playing app come from Android's
  media sessions, which need notification access. The listener reads no
  notifications; it only exists to make the sessions available.

## Microphones (karaoke)

* An input is only recorded while the user has it open; listing the
  inputs records nothing. Android asks for the microphone permission when
  the first one is opened (not at start), and shows its own microphone
  indicator while recording; Windows applies its microphone privacy
  setting. Leaving the microphone check in the settings closes the inputs
  it opened, quitting the app closes all.
* The audio stays on the device: the core keeps the last few seconds of
  each input in memory (for levels and scoring the singing) and neither
  stores it nor sends it anywhere. The karaoke page opens the inputs its
  singers use when a round starts and closes them when it ends or the
  page is left.
* Phones as wireless microphones: only when the user starts it on the
  karaoke page. The phone's singing goes as Opus packets to the Beatlumen
  server, which hands them only to a device of the same login (an account,
  or everyone with the server password) and keeps the last three seconds in
  memory, never on disk. No device opens a port.
* The system's sound libraries are used as they are (libpulse on Linux,
  loaded at run time; WASAPI on Windows; AudioRecord on Android).
* The only stored data is a latency offset per input (by its system id) in
  the settings.

## Source extensions (server)

* Separate programs the admin configures (`docs/EXTENSIONS.md`); the
  server sends them a shared token and connects to nothing else on their
  behalf: no URLs from an extension are fetched, pictures and audio come
  from the extension itself.
* What an extension sends is bounded and checked: JSON (1 MiB) and text
  lengths, item ids (a safe character set, used in URLs and file names),
  pictures (4 MiB, decoded and re-encoded as JPEG before they reach apps
  or files), at most three imports at once. File names come from
  sanitised titles inside the extension's own folder.
* Only logged-in users can search and import. Imports go into a library
  folder of their own; temporary ones are deleted after two hours.

## Known limitations

* Tokens appear in media URLs (`?token=`) because `<audio>`/`<img>` cannot
  send headers; reverse proxies may log them.
* The browser build decodes pictures with the browser (and Qt inside the
  WebAssembly sandbox), and a panic there stops the page (WebAssembly is
  built with `panic=abort`).
