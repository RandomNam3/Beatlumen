# Source extensions

A source extension finds music somewhere the server cannot look itself (a
video platform, a shop, an archive) and hands it to the server. Extensions
are **separate programs**: any language, any licence, own release cycle.
The server talks to them over HTTP; the apps only ever talk to the server.
They are not linked into the server, so the server stays AGPL-3.0 and an
extension can be closed source.

```
app  ──HTTPS──▶  beatlumen-server  ──HTTP (token)──▶  extension  ──▶  the outside world
                 │ search proxy, picture proxy
                 │ import: cuts, tags, cover, library
```

The server only connects to the extensions the admin configured. An
extension should listen on localhost or a private container network only.

## Configuration (server)

| Variable / option | Meaning |
| --- | --- |
| `BEATLUMEN_EXTENSIONS=youtube=http://127.0.0.1:7790,other=http://…` / `--extension youtube=http://…` / `"extensions": {"youtube": "http://…"}` | The extensions: id (1-32 of `a-z 0-9 -`, not `jobs`) and base URL |
| `BEATLUMEN_EXTENSION_TOKEN_FILE` / `--extension-token-file` / `"extensionTokenFile"` | A file with one line, at least 16 characters: sent as `Authorization: Bearer <token>` to every extension |

Kept imports are written to `<data>/extensions/library/<id>/`, which the
server adds as one more music folder. Temporary imports (for the users'
encrypted files) go to `<data>/extensions/temporary/` and are deleted after
two hours.

## Protocol (version 1)

The server sends `GET` requests with `Authorization: Bearer <token>` (when
configured) and expects JSON (UTF-8) unless noted. Errors: any non-200
status, optionally with `{"error": "message"}`. Strings are cut to 300
characters, control characters removed.

**Item ids** are chosen by the extension and opaque to the server, but must
match `[A-Za-z0-9_.:-]{1,128}` without `..` (they appear in URLs and file
names). Items with other ids are dropped.

### `GET /manifest`

```json
{"protocol": 1, "id": "youtube", "name": "YouTube", "version": "1.0.0", "search": true}
```

Fetched again every five minutes (30 seconds after a failure). Extensions
that do not answer are left out of the list the apps see.

### `GET /search?q=<query>&limit=<1-50>`

```json
{"items": [{"id": "dQw4w9WgXcQ", "title": "…", "artist": "…", "album": "…",
            "duration": 212.0, "detail": "Lyric video · 2009", "image": true}]}
```

`detail` is a short line shown under the result; `image` says whether
`/image/<id>` has a picture.

### `GET /image/<id>`

The item's picture (JPEG or PNG, at most 4 MiB). With `?full=1` the server
asks for the largest picture (embedded as cover); without it a thumbnail is
enough. The server decodes and re-encodes pictures, so only clean JPEG data
reaches the apps.

### `GET /item/<id>`

```json
{"id": "dQw4w9WgXcQ", "title": "…", "artist": "…", "album": "…", "year": "2009",
 "duration": 205.3, "format": "webm-opus", "cuts": [[0.0, 6.1], [211.2, 213.0]],
 "image": true, "source": "https://youtu.be/dQw4w9WgXcQ"}
```

* `format`: `webm-opus` (Opus in WebM, e.g. as video platforms deliver
  it), `ogg-opus`, `mp3`, `m4a` or `flac`.
* `cuts`: seconds of the delivered audio to leave out (intros, outros,
  talking). The server applies them to `webm-opus` by remuxing into
  Ogg/Opus whole packets at a time, without re-encoding; other formats are
  stored as delivered, so an extension delivering them must cut itself.
* `duration`: length after the cuts (shown before the import finishes).
* `source`: stored as the `SOURCE` tag.

### `GET /audio/<id>`

The audio in `format`, as fast as it can be fetched (the server streams it
to the listener while it is still arriving, so it does not need to be
complete before the first byte). A length is not required (chunked is
fine).

## Server API (for the apps)

Every endpoint needs a logged-in user (the server password or an account).

| Request | Answer |
| --- | --- |
| `GET /api/extensions` | `{"extensions": [{"id", "name", "search"}]}` |
| `GET /api/extensions/<ext>/search?q=&limit=` | `{"items": [{"id", "title", "artist", "album", "duration", "detail", "image": "/api/extensions/<ext>/image/<id>" or null, "track": library track id or null}]}` |
| `GET /api/extensions/<ext>/image/<id>?size=32-1024` | JPEG |
| `POST /api/extensions/<ext>/import` `{"id", "keep": true}` | A job (below); `keep: false` imports temporarily for the user's encrypted files. A song already imported answers `{"state": "done", "track"}` right away |
| `GET /api/extensions/jobs/<job>` | `{"job", "state": "running"/"done"/"failed", "bytes", "track", "error", "audio", "keep", "title", "artist", "album", "duration", "mime"}` |
| `GET /api/extensions/jobs/<job>/audio` | The imported file. While it is written: what is there so far and the rest as it arrives (chunked); `Range: bytes=N-` answers `206` with `Content-Range: bytes N-*/*` (length not known yet). When done: an ordinary file with ranges |

At most three imports run at once; the same song imported again while it
runs joins the running import. `track` is set once the library scan has
picked up a kept import.
