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
* **No connections except the configured server.** The apps contact only
  the server the user entered; the server opens no outgoing connections.
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

## Known limitations

* Tokens appear in media URLs (`?token=`) because `<audio>`/`<img>` cannot
  send headers; reverse proxies may log them.
