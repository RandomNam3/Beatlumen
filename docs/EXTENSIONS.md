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
| `BEATLUMEN_EXTENSION_CLEANUP_DAYS` / `--extension-cleanup-days` / `"extensionCleanupDays"` | Songs an extension downloaded are deleted after this many days without being played. Default 90, 0: never. Also in the admin area |
| `BEATLUMEN_EXTENSION_CLEANUP_KEEP_LISTED` / `"extensionCleanupKeepListed"` | Songs someone added on purpose, or in anyone's playlist or favorites, stay anyway. Default on. Also in the admin area |

Kept imports are written to `<data>/extensions/library/<id>/`, which the
server adds as one more music folder. Temporary imports (for the users'
encrypted files) go to `<data>/extensions/temporary/` and are deleted after
two hours.

### Songs nobody plays

A kept import is in the library of the accounts that brought it in, by playing
it or adding it, and the others do not see it there; the file exists once.
`POST /api/library/remove` takes a song out of an account's library, into its
trash for 29 days (`GET /api/library/hidden`, `POST /api/library/restore`). The
file goes 30 days after the last account took it out, if nobody played it since.
The admin's music folders are only hidden per account, never deleted. One limit:
a song someone else already has arrives at once, so adding it can reveal that.

Kept imports count as played when a client starts streaming them or
downloads them. Once an hour the server deletes those not played for
`extensionCleanupDays`. Only songs on its list of imports
(`<data>/extensions/imported.json`) and only inside
`<data>/extensions/library/` are deleted: the library's own songs are never
touched, not even with the same name. Songs someone added on purpose (`"pin"`
below) stay, like those in a playlist or among favorites, unless the admin turns
that off. Songs from before this list kept times get the full period from the
first start with it. A deleted song can simply be imported again.

## Protocol

Versions 1 and 2. Version 2 adds everything marked as such; an extension that
stays at version 1 keeps working and only searches.

The server sends `GET` requests with `Authorization: Bearer <token>` (when
configured) and expects JSON (UTF-8) unless noted. Errors: any non-200
status, optionally with `{"error": "message"}`. Strings are cut to 300
characters, control characters removed.

**Item ids** are chosen by the extension and opaque to the server, but must
match `[A-Za-z0-9_.:-]{1,128}` without `..` (they appear in URLs and file
names). Items with other ids are dropped.

### `GET /manifest`

```json
{"protocol": 2, "id": "youtube", "name": "YouTube", "version": "1.0.0",
 "capabilities": ["search", "shelves", "collections", "radio"]}
```

Fetched again every five minutes (30 seconds after a failure). Extensions
that do not answer are left out of the list the apps see. The apps show `name`
wherever they mention the extension; nothing about a particular service is
built into them.

`capabilities` (version 2) says which of the requests below the extension
answers: `search`, `shelves`, `collections`, `radio`. Unknown words are
ignored, and the server does not ask for what is not listed. Version 1 has
`"search": true` instead.

### `GET /search?q=<query>&limit=<1-50>`

```json
{"items": [{"id": "dQw4w9WgXcQ", "title": "…", "artist": "…", "album": "…",
            "duration": 212.0, "detail": "Lyric video · 2009", "image": true}]}
```

`detail` is a short line shown under the result; `image` says whether
`/image/<id>` has a picture. In version 2 an entry can also be a collection,
which is `"kind": "collection"` (a song is `"kind": "song"` or has no `kind`):

```json
{"kind": "collection", "id": "OLAK5uy_abc", "title": "…", "artist": "…",
 "detail": "Album · 2009", "count": 12, "image": true}
```

Entries of another `kind` are left out, so new kinds can come without breaking
older servers. The server keeps answers to the same search for ten minutes.

### `GET /shelves?limit=<1-50>` (version 2, `shelves`)

Rows of suggestions for the start page.

```json
{"shelves": [{"title": "…", "items": [song or collection, …]}]}
```

At most 20 rows, each with at most `limit` entries; empty rows are left out.
The server keeps the answer for 15 minutes.

### `GET /collection/<id>?limit=<1-50>&after=<position>` (version 2, `collections`)

The songs of a collection, a page at a time (`after` only for later pages).

```json
{"title": "…", "artist": "…", "detail": "…", "image": true,
 "items": [song, …], "next": "position of the next page"}
```

`next` is left out on the last page. It is opaque to the server: up to 2048
printable ASCII characters without spaces. The apps fetch up to 500 songs.
Only songs count here. The server keeps answers for 15 minutes.

### `GET /radio/<id>?limit=<1-50>` (version 2, `radio`)

Songs that fit after this song.

```json
{"items": [song, …]}
```

Asked each time the app's queue runs low, with the last song in the queue, so
the answer may differ every time. Not kept by the server.

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
| `GET /api/extensions` | `{"extensions": [{"id", "name", "search", "shelves", "collections", "radio"}]}` |
| `GET /api/extensions/<ext>/search?q=&limit=` | `{"items": [{"kind": "song", "id", "title", "artist", "album", "duration", "detail", "image": "/api/extensions/<ext>/image/<id>" or null, "track": library track id or null}, or {"kind": "collection", "id", "title", "artist", "detail", "count", "image"}]}` |
| `GET /api/extensions/<ext>/shelves?limit=` | `{"shelves": [{"title", "items": [as in the search]}]}`; 404 if not offered |
| `GET /api/extensions/<ext>/collections/<id>?limit=&after=` | `{"id", "title", "artist", "detail", "image", "items": [songs], "next"}`; 404 if not offered |
| `GET /api/extensions/<ext>/radio/<id>?limit=` | `{"items": [songs]}`; 404 if not offered |
| `GET /api/extensions/<ext>/image/<id>?size=32-1024` | JPEG |
| `POST /api/extensions/<ext>/import` `{"id", "keep": true, "pin": false}` | A job (below); `keep: false` imports temporarily for the user's encrypted files, `pin: true` says the user added the song on purpose. A song already imported answers `{"state": "done", "track"}` right away |
| `GET /api/extensions/jobs/<job>` | `{"job", "state": "running"/"done"/"failed", "bytes", "track", "error", "audio", "keep", "title", "artist", "album", "duration", "mime"}` |
| `GET /api/extensions/jobs/<job>/audio` | The imported file. While it is written: what is there so far and the rest as it arrives (chunked); `Range: bytes=N-` answers `206` with `Content-Range: bytes N-*/*` (length not known yet). When done: an ordinary file with ranges |

At most three imports run at once; the same song imported again while it
runs joins the running import. `track` is set once the library scan has
picked up a kept import.

## In the apps

With a server that has extensions, the search shows what they find next to
what the library has, and each extension can be chosen on its own. Its rows of
suggestions appear on the start page, its collections open as a page like an
album.

A result plays right away: the app streams the import while the server writes
it (in the browser, once it is finished). The first time, the app asks where
songs go — the library on the server (everyone using the server) or the user's
encrypted files (the app downloads the finished import and encrypts it; the
server deletes its temporary copy after two hours). The choice is remembered
and can be changed in the settings. When a library import is finished, playback
continues with the library track and its full analysis.

A result can also go straight into the library, without playing it: the button
on its row, its menu, or "Add all to library" on a collection. Those songs are
brought in a few at a time, and nothing is asked because the choice is clear.

A song added to the queue only waits there: the server fetches nothing until
its turn comes, so a long list or a radio costs nothing up front. A song that
cannot be fetched leaves the queue and the next one starts. Waiting songs do
not outlive the app: they are not in the queue after a restart.
