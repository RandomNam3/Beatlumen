# beatlumen-server

Serves music libraries from local folders to Beatlumen clients (the native
apps and the browser web client) over HTTP: library index, audio streaming
with byte ranges, cover thumbnails, server-side track analysis, shared
playlists, shared effect presets and background images.

The server connects **only to remote sources its admin configures** (none by
default; planned: WebDAV/Nextcloud, SFTP, Jellyfin, Subsonic, internet
radio): there are no update checks, metadata or cover lookups, telemetry,
or anything similar. It uses
only the standard library and Beatlumen's own crates.

## Usage

```sh
beatlumen-server --music ~/Music --images ~/Pictures/Backgrounds --password 'choose-one'
```

Then open `http://<server>:7878/` in a browser (when the web client is
installed) or add the server in an Beatlumen app.

| Option | Default | Description |
|---|---|---|
| `--music DIR` | none | Music folder, scanned recursively (repeatable) |
| `--images DIR` | none | Background image folder (repeatable) |
| `--data DIR` | `./beatlumen-data` | Caches, playlists, presets, uploads, tokens |
| `--bind ADDR` | `0.0.0.0:7878` | Listen address (`127.0.0.1:7878` for local or reverse-proxy use) |
| `--password PW` | none | Clients must log in. `BEATLUMEN_PASSWORD=...` also works and keeps the password out of the process list |
| `--web-dir DIR` | see below | Static web client files |
| `--rescan-interval SECS` | `600` | Automatic incremental rescans, `0` = off |
| `--name NAME` | `Beatlumen Server` | Name shown to clients |
| `--analysis-max-minutes M` | `20` | Longest track analysed on the server, `0` = unlimited |
| `--scan-threads N` | automatic (up to 4) | Threads reading tags during scans |
| `--config FILE` | none | JSON configuration file |
| `--verbose` | off | Log every request and per-file tag problems |
| `--vault-pepper-file FILE` | `<data>/vault/pepper` | Secret (32+ bytes) mixed into the account verifiers; keep it outside the data folder, e.g. as a container secret |
| `--vault-quota-mb MB` | `10240` | Encrypted storage per account, `0` = unlimited |
| `--registration MODE` | `invite` | `invite`: new accounts with an invitation code; `off`: no new accounts |
| `--public-url URL` | none | The server's `https://` address as users reach it (reverse proxy); enables passkeys, with this host as WebAuthn relying party |
| `--android-cert SHA256` | official app | Signing certificate fingerprint of an Android app that may use passkeys for this server (repeatable); published in `/.well-known/assetlinks.json` |
| `--push-allow-private` | off | Allow push services (UnifiedPush) in private networks and over plain http, e.g. ntfy at home |
| `--health-check` | | Exit status 0 if a server with this configuration answers (container health checks) |
| `-h`, `--help` / `-V`, `--version` | | |

Every option can also be set through an environment variable (useful in
containers): `BEATLUMEN_MUSIC`, `BEATLUMEN_IMAGES` (folders separated like
`PATH`), `BEATLUMEN_DATA`, `BEATLUMEN_BIND`, `BEATLUMEN_PASSWORD`,
`BEATLUMEN_PASSWORD_FILE` (file with the password, for Docker/Podman
secrets), `BEATLUMEN_WEB_DIR`, `BEATLUMEN_NAME`, `BEATLUMEN_ALLOWED_HOSTS`
(comma separated), `BEATLUMEN_SOURCE_URL`, `BEATLUMEN_RESCAN_INTERVAL`,
`BEATLUMEN_ANALYSIS_MAX_MINUTES`, `BEATLUMEN_SCAN_THREADS`,
`BEATLUMEN_VAULT_PEPPER_FILE`, `BEATLUMEN_VAULT_QUOTA_MB`, `BEATLUMEN_REGISTRATION`,
`BEATLUMEN_PUSH_ALLOW_PRIVATE`, `BEATLUMEN_CA_FILE` (extra root certificates, PEM, for outgoing TLS),
`BEATLUMEN_PUBLIC_URL`, `BEATLUMEN_ANDROID_CERTS` (comma separated),
`BEATLUMEN_VERBOSE`. A container image, `compose.yml` and AppArmor/SELinux
profiles are in [`container/`](../../container/README.md).

Precedence: defaults < config file < environment variables < command line.
`--music` / `--images` given on the command line replace the lists from the
file. Relative paths in the file are resolved against the file's folder.

```json
{
  "music": ["/srv/music", "/mnt/nas/music"],
  "images": "/srv/backgrounds",
  "data": "/var/lib/beatlumen",
  "bind": "127.0.0.1:7878",
  "password": "choose-one",
  "webDir": "/opt/beatlumen/web",
  "rescanInterval": 600,
  "name": "Living Room",
  "sourceUrl": "https://github.com/RandomNam3/Beatlumen",
  "analysisMaxMinutes": 20,
  "scanThreads": 0,
  "verbose": false
}
```

Logs go to stderr with UTC timestamps.

`sourceUrl` / `--source-url` is the link to the source code shown to users
(Settings → About). Beatlumen is licensed under the AGPL-3.0-or-later: if you
run a modified server for others, point it to the source of your version.

### Web client

The static web client is served from, in this order:

1. `--web-dir DIR`,
2. a `web/` folder next to the executable,
3. files embedded at build time: build with
   `BEATLUMEN_EMBED_WEB=/path/to/built/web cargo build --release -p beatlumen-server`
   (all files below that folder are embedded; without the variable nothing is
   embedded and the build does not need the web client),
4. a small built-in placeholder page.

A pre-compressed copy next to a file (`beatlumen.wasm.gz`, written by
`build-web.sh`) is sent to browsers that accept gzip. Embedded builds keep
only the compressed copy of such files and decompress it for the rare
client without gzip support.

`/` serves `index.html`; extension-less unknown paths fall back to
`index.html` (client-side routes). Paths containing `..`, `.`-prefixed
segments, backslashes, `:` or control characters are rejected.

### Data folder

```
beatlumen-data/
  library.json          cached library index (fast startup, incremental scans)
  tokens.json           SHA-256 digests of login tokens (mode 0600 on Unix)
  playlists.json        shared playlists
  presets/<name>.hfx    shared presets
  uploads/              uploaded background images
  cache/covers/         <track id>_<size>_<stamp>.jpg cover thumbnails
  cache/analysis/       <track id>_<config key>.ocan analysis blobs,
                        <track id>.loudness.json loudness measurements
  cache/images/         <image id>_<size>_<stamp>.jpg scaled backgrounds
```

Everything under `cache/` can be deleted at any time. Stale cache files of
removed or changed tracks are pruned after each scan. Writes of JSON files are
atomic (temporary file + rename).

## Library scanning

* Recursive walk of every music folder; symlinks are followed but directory
  loops are detected; hidden entries (`.name`) and NAS helper folders
  (`@eaDir`, `#recycle`, `$RECYCLE.BIN`) are skipped.
* Files with a supported extension (case-insensitive): mp3, mp2, flac, ogg,
  oga, opus, m4a, m4b, mp4, aac, wav, wave, aif, aiff, aifc, mka, webm.
* Incremental: only new files or files whose size or modification time
  changed are read again. Unreadable files are skipped and logged; files with
  unreadable tags are listed with their file name as title.
* Artists are split on `;`, ` / `, ` feat. `, ` ft. ` (case-insensitive) and
  NUL (ID3v2.4 multi values); the album artist is kept intact.
* Albums group tracks by (album artist or first artist, album name),
  case-insensitively. An album's `coverTrack` is its first track (by disc and
  track number) with a cover.
* Covers: the track's embedded picture (front cover preferred), else a folder
  image `cover|folder|front|album .jpg|.jpeg|.png` (case-insensitive), else the
  embedded picture of the directory's first track. Only the first track of a
  directory without folder image is checked for an embedded picture during
  scans.
* If a whole music folder is missing (e.g. an unmounted drive) its previous
  entries are kept instead of being dropped.
* Large initial scans publish intermediate results every 5 seconds.
* Performance (20 000 small WAV files, warm cache, release build): initial
  scan 0.9 s, unchanged rescan 0.2 s, `GET /api/library` (7.3 MB) 70 ms,
  conditional `GET` (304) under 1 ms, restart from the cached index 0.6 s.

### Ids

* Track id: 16 hex characters of the 64-bit FNV-1a hash of
  `<music folder index>/<path relative to the folder, '/' separated>`.
* Album id: the same hash of `album\0<album artist lower-cased>\0<album lower-cased>`.
* Image id: hash of `<image folder index>/<relative path>`, or
  `uploads/<file name>` for uploads.
* Playlist id: 16 random hex characters.

Ids stay stable across rescans and restarts as long as the folder order and
file paths do not change.

## HTTP API

All responses are JSON unless noted. Errors are `{"error": "message"}` with a
matching status code (400 invalid input, 401 not logged in, 403 forbidden,
404 not found, 405 method not allowed, 413 too large, 415 unsupported media,
422 undecodable, 429 too many failed logins, 500 internal error). Types are
those of `bl_net::api` (`ServerInfo`, `Library`, `Track`, `Album`,
`Playlist`, `ImageEntry`). `HEAD` works wherever `GET` does.

| Endpoint | Description |
|---|---|
| `GET /api/info` | `ServerInfo` (always public), including `sourceUrl` |
| `POST /api/login` | body `{"password": "..."}` → `{"token": "..."}`; 401 on a wrong password. Without a configured password it returns `{"token": ""}` |
| `POST /api/logout` | revokes the presented token → 204 |
| `GET /api/library` | `Library`. `ETag: "<revision>"`; `If-None-Match` → 304 |
| `POST /api/rescan` | starts a scan → 202 `{"scanning": true}` (`/api/info` reports `scanning`) |
| `GET /api/tracks/{id}/stream` | the raw file, `Range` supported, `Content-Type` from the extension, `Cache-Control: private, max-age=3600` |
| `GET /api/tracks/{id}/cover?size=N` | JPEG thumbnail (N clamped to 32..1024, default 300; never upscaled); 404 without cover; `ETag` |
| `GET /api/albums/{id}/cover?size=N` | cover of the album's `coverTrack` |
| `GET /api/tracks/{id}/analysis?cfg=...` | `application/octet-stream` blob of `bl_analysis::encode`. `cfg` is the percent-encoded JSON `{"analysisConfig":{..},"noveltyConfigs":{..}}` of a preset (optional: defaults) |
| `GET /api/tracks/{id}/loudness` | `{"integrated": LUFS or null, "peak": linear}`: integrated loudness after EBU R128, used by clients for volume normalization (measured on first request, then cached) |
| `GET /api/playlists` | all playlists **including their items** |
| `POST /api/playlists` | body `{"name", "kind": "track"\|"background", "items"?}` → 201 `Playlist` |
| `GET /api/playlists/{id}` | `Playlist` |
| `PUT /api/playlists/{id}` | partial update: `{"name"?, "items"?}` → updated `Playlist` |
| `DELETE /api/playlists/{id}` | 204 |
| `GET /api/images` | `[ImageEntry]` from the image folders and uploads (jpg, jpeg, png, bmp, gif, webp) |
| `GET /api/images/{id}` | original bytes (`Range` supported); `?size=N` → JPEG scaled to fit N×N (64..4096, cached) |
| `POST /api/images` | raw image body, optional `X-Filename` header (may be percent-encoded) → 201 `ImageEntry`. Max 50 MB, must be JPEG/PNG/BMP/GIF/WebP by magic bytes; the stored name is sanitized, made unique and gets the extension of the detected format |
| `DELETE /api/images/{id}` | uploads only (403 for images from configured folders) → 204 |
| `GET /api/presets` | `[{"name": "..."}]` |
| `GET /api/presets/{name}` | the raw `.hfx` JSON |
| `PUT /api/presets/{name}` | stores the body (a JSON object with an `effects` array, max 4 MB) → 201 created / 200 replaced |
| `DELETE /api/presets/{name}` | 204 |
| `GET /…` | static web client |
| `/api/vault/…` | accounts and end-to-end encrypted files, see below |

Preset names (percent-encoded in the URL) must be 1..100 characters without
`/`, `\`, `..`, control characters, `:*?"<>|`, leading `.`, trailing `.` or
space, and must not be Windows device names; invalid names give 400.

`modified` fields are Unix timestamps in seconds. Cover and image URLs are
not versioned; clients that need fresh images right after a change can append
a cache-busting parameter such as `&v=<track.modified>` (ignored by the
server).

### Analysis

The track is decoded completely, downmixed to mono and analysed with
`bl_analysis`. The result is cached per track and configuration key
(`AnalysisConfig::cache_key`) and recomputed when the track file is newer
than the cache. At most two analyses run at a time; concurrent requests for
the same track and configuration wait for the running one. Configuration
values are clamped to sane ranges, and for very long tracks the chroma
down-sampling is raised so that the self-similarity matrices stay small
(the blob records the value used). Tracks longer than
`--analysis-max-minutes` get 413 (a 10-minute track needs roughly 400 MB of
memory while being analysed).

### Authentication

When a password is configured, every `/api/*` endpoint except `/api/info`
and `/api/login` requires `Authorization: Bearer <token>` or `?token=<token>`
(the query form is needed for media URLs used by `<audio>`/`<img>`). The
static web client itself is public so that it can show its login screen.

* Tokens are 128-bit random values from the OS (`/dev/urandom`,
  `BCryptGenRandom` on Windows). They expire after 180 days;
  `POST /api/logout` revokes one. Only their SHA-256 digests are stored
  (`tokens.json`, readable by the server user only).
* The password is compared in constant time. Changing the password
  invalidates all tokens.
* Password checks run one at a time. A failed login costs 300 ms, rising to
  3 s while many logins fail (from any address); after 10 failures from one
  address (IPv6: one /64) within 5 minutes logins from there are refused
  with 429. Behind a reverse proxy on the same machine the address is taken
  from `X-Forwarded-For` / `X-Real-IP`. Use a long password (12+ characters)
  if the server can be reached from the internet.

### Accounts and encrypted files

Users with an account store files on the server that only they and the
people they share them with can read (design and threat model:
[`docs/ENCRYPTION.md`](../../docs/ENCRYPTION.md)). Passwords, keys and file
contents never reach the server; the apps encrypt on the device.

The admin creates invitations on the command line; each code creates one
account and expires after 7 days (`--days N`). `--registration off` turns
registration off completely (also for existing codes):

```sh
beatlumen-server invite --data /var/lib/beatlumen
docker exec beatlumen /beatlumen-server invite      # container
beatlumen-server users --data /var/lib/beatlumen    # list accounts
```

A session token from `/api/vault/login` also opens the library of a
password-protected server. Binary values are base64 in JSON.

| Endpoint | Description |
|---|---|
| `POST /api/vault/prelogin` | `{"user"}` → `{"salt", "params": {"m","t","p"}}` for Argon2id; made-up but stable values for unknown names |
| `POST /api/vault/register` | `{"invite", "user", "auth", "recoveryAuth", "bundle"}` → 201 `{"token", "user"}`; 403 invalid invitation, 409 name taken |
| `POST /api/vault/login` | `{"user", "auth"}` → `{"token", "user", "bundle"}`; 401 wrong, 429 locked |
| `POST /api/vault/recover` | `{"user", "recoveryAuth"}` → like login |
| `POST /api/vault/logout` | ends the session → 204 |
| `GET /api/vault/account` | `{"user", "bundle"}` (salt, params, wrapped master key ×2, wrapped identity, public key) |
| `PUT /api/vault/account/keys` | `{"proof": {"auth"} or {"recoveryAuth"}, "password"?: {"salt","params","auth","masterByPassword"}, "recovery"?: {"recoveryAuth","masterByRecovery"}}` → `{"token"?}`; a new password ends all other sessions |
| `DELETE /api/vault/account` | `{"proof"}` → 204; deletes the account and its files |
| `GET /api/vault/users` | `[{"name", "public"}]` (public keys for sharing) |
| `GET /api/vault/users/{name}` | `{"name", "public"}` |
| `GET /api/vault/files` | `{"files": [entry], "used", "quota"}`: own files and files shared with the user |
| `POST /api/vault/files` | `{"header", "meta", "signature", "key"}` → 201 entry; 507 over quota |
| `PUT /api/vault/files/{id}/data?offset=N` | appends up to 4 MB of the encrypted file at `N` (must equal `received`) → `{"received", "complete"}` |
| `GET /api/vault/files/{id}` | entry: `id, owner, size, received, complete, header, meta, signature, key` (the caller's sealed key), `recipients` (owner only) |
| `GET /api/vault/files/{id}/data` | the encrypted file, `Range` supported |
| `PUT /api/vault/files/{id}/meta` | `{"meta", "signature"}` → 204 (owner) |
| `POST /api/vault/files/{id}/keys` | `{"user", "key"}` → 204: share (owner) |
| `DELETE /api/vault/files/{id}/keys/{user}` | revoke (owner) or leave (recipient) → 204 |
| `DELETE /api/vault/files/{id}` | 204 (owner) |
| `POST /api/vault/passkeys/options` | `{"purpose": "login"}` (public) or `"register"` (signed in) → `{"challengeId", "publicKey"}`: WebAuthn options in their JSON form, with the PRF extension |
| `POST /api/vault/passkeys/login` | `{"challengeId", "credentialId", "clientDataJSON", "authenticatorData", "signature"}` (base64url) → `{"token", "user", "bundle", "credentialId", "masterByPasskey"}` |
| `GET /api/vault/passkeys` | `{"passkeys": [{"id", "name", "created", "lastUsed"}], "passwordOff", "available"}` |
| `POST /api/vault/passkeys` | `{"challengeId", "clientDataJSON", "attestationObject", "name", "masterByPasskey", "proof"}` → 201 `{"id"}` |
| `DELETE /api/vault/passkeys/{id}` | `{"proof"}` → 204; 409 for the last passkey while the password is off |
| `GET /api/vault/push` | `{"vapidKey", "subscriptions": [{"id", "name", "created"}]}` |
| `POST /api/vault/push` | `{"endpoint", "p256dh", "auth", "name"}` (a UnifiedPush/Web Push subscription) → 201 `{"id"}`; the same endpoint again updates it |
| `DELETE /api/vault/push/{id}` | 204 |

**Passkeys** need `--public-url` (an `https://` address; `http://localhost`
for testing). Sign-in requires user verification; the server checks the
signature (ES256, EdDSA, RS256) and the signature counter, and returns
the master key wrapped for that passkey, which only the passkey's PRF
output unwraps. `"proof": {"session": true}` is accepted for ten minutes
after a passkey sign-in. `PUT /api/vault/account/keys` with
`"passwordOff": true` turns the password off (at least one passkey);
setting a new password turns it on again.

**Push notifications** (UnifiedPush): when a file is shared with a user,
the server sends `{"type": "share", "from", "file"}` to each of the user's
registered endpoints, encrypted for the device (RFC 8291) and signed with
the server's VAPID key (RFC 8292, `data/vault/vapid.key`). The push
service sees only ciphertext; no phone number or e-mail address is used.
Endpoints must use https and public addresses unless
`--push-allow-private` is set; endpoints that answer 404/410 are removed.

The server stores only hashes of the auth keys mixed with the pepper, and
checks them in constant time. Failed logins, recoveries and password proofs
cost 300 ms; after 10 failures for one account or 20 from one address
within 15 minutes further attempts get 429. Sessions last 90 days.

## Security notes

* **No TLS is built in.** Passwords, tokens and music travel in plain text.
  On a trusted home LAN this may be acceptable; for anything else use a VPN
  (e.g. WireGuard, Tailscale) or a reverse proxy with HTTPS, with the server
  bound to localhost (`--bind 127.0.0.1:7878`) and the proxy's domain
  allowed (`--allow-host music.example.org`). The apps and the web app then
  connect to `https://music.example.org`; with a self-signed certificate
  the apps show its SHA-256 fingerprint and connect after you compared and
  trusted it. Caddy:

  ```
  music.example.org {
      reverse_proxy 127.0.0.1:7878
  }
  ```

  nginx:

  ```
  location / {
      proxy_pass http://127.0.0.1:7878;
      proxy_set_header Host $host;
      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
      proxy_buffering off;
      client_max_body_size 50m;
  }
  ```

* Without `--password` anyone who can reach the port can read the library,
  edit playlists and presets and upload images (the server logs a warning
  when listening on a non-loopback address without a password). Web pages
  cannot use the server through the visitor's browser: requests for unknown
  host names (DNS rebinding) and cross-site writes are refused.
* Limits against overload: request bodies 4 MiB (image uploads 50 MiB),
  read as they arrive and at a minimum rate; 10 s for request headers;
  256 connections, 32 per client address (local reverse proxies excepted);
  images up to 32 megapixels.
* Files are only addressed by ids; request paths never reach the file system
  except for the static web client, whose path segments are validated.
* Responses carry `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and `Cross-Origin-Resource-Policy:
  same-origin`; HTML additionally gets a strict `Content-Security-Policy`,
  `Cross-Origin-Opener-Policy: same-origin`,
  `Cross-Origin-Embedder-Policy: require-corp` and `X-Frame-Options: DENY`.
* Keep the data folder private: it contains token digests, a salted password
  check value and uploads.
* Request bodies are buffered in memory by the HTTP layer (up to 51 MB per
  connection, at most 256 connections).

## Building and testing

```sh
cargo build --release -p beatlumen-server
cargo build --release -p beatlumen-server --target x86_64-pc-windows-gnu
cargo test --release -p beatlumen-server
# 20 000 track benchmark:
cargo test --release -p beatlumen-server --test server -- --ignored --nocapture
```

The integration tests start servers on `127.0.0.1:0` with temporary folders
and generate audio fixtures with `ffmpeg` (tests needing them pass with a note
when ffmpeg is missing).
