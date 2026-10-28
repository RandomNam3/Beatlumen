# beatlumen-server

Serves music libraries from local folders to Beatlumen clients (the native
apps and the browser web client) over HTTP: library index, audio streaming
with byte ranges, cover thumbnails, server-side track analysis, shared
playlists, shared effect presets and background images.

The server **never opens outgoing network connections**: there are no update
checks, metadata or cover lookups, telemetry, or anything similar. It uses
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
| `--health-check` | | Exit status 0 if a server with this configuration answers (container health checks) |
| `-h`, `--help` / `-V`, `--version` | | |

Every option can also be set through an environment variable (useful in
containers): `BEATLUMEN_MUSIC`, `BEATLUMEN_IMAGES` (folders separated like
`PATH`), `BEATLUMEN_DATA`, `BEATLUMEN_BIND`, `BEATLUMEN_PASSWORD`,
`BEATLUMEN_PASSWORD_FILE` (file with the password, for Docker/Podman
secrets), `BEATLUMEN_WEB_DIR`, `BEATLUMEN_NAME`, `BEATLUMEN_ALLOWED_HOSTS`
(comma separated), `BEATLUMEN_SOURCE_URL`, `BEATLUMEN_RESCAN_INTERVAL`,
`BEATLUMEN_ANALYSIS_MAX_MINUTES`, `BEATLUMEN_SCAN_THREADS`,
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
