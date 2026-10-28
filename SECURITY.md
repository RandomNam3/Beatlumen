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
  implementation.
* **No connections except the configured ones.** The apps contact only
  the server the user entered; the server connects only to remote sources
  its admin configures and to the push services the users' devices
  registered: no update checks, telemetry or metadata services.
* **Untrusted input is bounded.** Every parser limits sizes before
  allocating (sample tables, image dimensions up to 32 megapixels, JSON
  nesting, preset effects and particles). Panics in the server are caught
  per request.
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

* Optional password; logins issue random tokens (stored as SHA-256 digests,
  file mode 0600) that expire after 180 days. Password checks are
  constant-time, serialized and rate limited per address and globally.
* Requests: bodies are read as they arrive with per-route limits, slow
  clients are disconnected, connections are limited per address,
  ambiguous request framing (`Content-Length` + `Transfer-Encoding`) is
  rejected.
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

## Container

* The image (`container/Containerfile`) holds only the static server
  binary; it runs as an unprivileged user (10001) and is meant to run with
  a read-only root file system, no capabilities and `no-new-privileges`
  (as in `container/compose.yml`).
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

## AirPlay receiver

* Off by default; when switched on, the app listens in the local network
  for AirPlay senders (RTSP over TCP and three UDP ports for the stream)
  and announces itself through mDNS (Android: the system's NsdManager).
* An optional password (HTTP digest, as AirPlay senders use it) keeps
  others in the same network from playing on the device. Without one,
  anyone in the network can stream to it, as with any AirPlay speaker.
* Everything read from the network is bounded: request headers (16 KB),
  bodies (4 MB, cover art), at most eight control connections, at most
  eight seconds of buffered audio; UDP packets are only accepted from the
  sender of the current stream. A new sender replaces the current one.
* The audio is decrypted with the per-stream AES key that the sender wraps
  with the RSA key of AirPlay 1 receivers. That key is public (published
  in 2011 and used by all open-source receivers), so it protects nothing
  against an attacker in the network; it only serves compatibility. The
  RSA code is not constant time for that reason, as it only ever uses
  this public key.
* Received audio is played and analysed live, never stored. Cover art is
  kept as a single file in the app's storage while a stream plays.

## Known limitations

* Tokens appear in media URLs (`?token=`) because `<audio>`/`<img>` cannot
  send headers; reverse proxies may log them.
