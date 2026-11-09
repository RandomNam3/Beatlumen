# Beatlumen Server in a container

The server runs in a minimal image built `FROM scratch` (no base image, not
even a distroless one). It contains only what the server needs, about 14 MB:

| Path | Why |
|---|---|
| `/beatlumen-server` | The server: a static binary (no libraries) with the web client embedded, gzip-compressed |
| `/data/` | Empty data folder owned by the server's user, so a new volume on it is writable |
| `/usr/share/doc/beatlumen/` | License texts, which the AGPL requires to accompany the program |

There is no shell, no package manager and no user database: the server
runs as the numeric user `10001`. For its few HTTPS connections (speech
model downloads the admin starts, push services) the server carries the
Mozilla root certificates built in. It works with Docker and Podman
(rootful and rootless).

| File | Purpose |
|---|---|
| `Containerfile` | Image build (Docker and Podman) |
| `compose.yml` | Docker Compose / Podman Compose service |
| `example.env` | Settings for `compose.yml` (copy to `.env`) |
| `beatlumen-server.container` | Podman Quadlet unit (systemd service) |
| `apparmor/beatlumen-server` | AppArmor profile (Ubuntu, Debian, openSUSE, …) |
| `selinux/beatlumen_server.cil` | SELinux policy (Fedora, RHEL, CentOS Stream, …) |

## Build

Run from the repository root (the build context):

```sh
docker build -f container/Containerfile -t beatlumen-server .
podman build --format docker -f container/Containerfile -t beatlumen-server .
```

(`--format docker` keeps the image's health check; Podman's default OCI
format drops it.)

The **web client** (`http://<server>:7878/` in a browser) is embedded when
`dist/web` exists: build it with `./build-web.sh`, or unpack the
`beatlumen-web` artifact of a CI run to `dist/web`. Without it, the server
shows a placeholder page; the apps work either way.

Build argument `BEATLUMEN_RELEASE_KEY` (hex public key, official builds)
makes the server accept only a web client with a valid release signature;
without it, the embedded web client is checked against its unsigned
manifest. Either way modified files are not served (see "Web client
integrity" in [`docs/ENCRYPTION.md`](../docs/ENCRYPTION.md)).

Build argument `RUST_IMAGE` selects the Rust build image (default
`docker.io/library/rust:1.99-slim-trixie`, Debian with the official Rust
toolchain; it is only used for building and is not part of the result:
the server is linked statically against musl there). When Docker Hub
limits anonymous pulls, use a mirror: `--build-arg
RUST_IMAGE=mirror.gcr.io/library/rust:1.99-slim-trixie`. The image builds for the
platform of the build host (x86-64, arm64, …); `docker buildx build
--platform linux/arm64` builds for another one.

## Run

### Docker Compose / Podman Compose

```sh
cd container
cp example.env .env            # music and image folders, port, name
printf '%s' 'a-long-password' > password.txt
head -c 32 /dev/urandom > pepper.bin   # secret for user accounts
chmod 0444 password.txt pepper.bin     # readable by the container user (see Secrets)
docker compose up -d --build   # or: podman compose up -d --build
docker compose logs -f
```

`podman-compose` 1.0.x (as in Debian and Ubuntu packages) leaves out
`ipc` and `pids_limit` and runs the health check through a shell the image
does not have, so it reports "unhealthy"; use `podman compose` with Docker
Compose as its provider, `podman run` below or the Quadlet unit.

### docker run / podman run

```sh
docker run -d --name beatlumen --restart unless-stopped \
  -p 7878:7878 \
  -v /srv/music:/music:ro,z \
  -v /srv/backgrounds:/images:ro,z \
  -v beatlumen-data:/data:Z \
  -e BEATLUMEN_PASSWORD_FILE=/run/secrets/password \
  -v "$PWD/password.txt":/run/secrets/password:ro,z \
  -e BEATLUMEN_VAULT_PEPPER_FILE=/run/secrets/pepper \
  -v "$PWD/pepper.bin":/run/secrets/pepper:ro,z \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --security-opt seccomp=container/seccomp/beatlumen-server.json \
  --ipc none \
  --health-cmd '["/beatlumen-server","--health-check"]' \
  beatlumen-server
```

`podman run` takes the same options. With Podman, the password is better
kept as a Podman secret:

```sh
printf '%s' 'a-long-password' | podman secret create beatlumen_password -
podman run ... --secret beatlumen_password,uid=10001,mode=0400,target=/secrets/beatlumen_password \
  -e BEATLUMEN_PASSWORD_FILE=/secrets/beatlumen_password ...
```

The target outside `/run/secrets` matters with `--read-only`: Podman mounts
`/run/secrets` itself and cannot add a secret's file to it in a read-only
container ("make mountpoint … read-only file system").

### Podman Quadlet (systemd)

Copy `beatlumen-server.container` to `~/.config/containers/systemd/`
(rootless) or `/etc/containers/systemd/` (root), adjust the folders, then
`systemctl --user daemon-reload && systemctl --user start beatlumen-server`.
For a rootless service that runs without a login session:
`loginctl enable-linger $USER`.

## Ports

| Port | Protocol | Direction | Purpose |
|---|---|---|---|
| `7878` | TCP | incoming | HTTP: API for the apps, audio streaming, web client |
| as configured | TCP, UDP 53 (DNS) | outgoing | Only to remote sources the admin configures (planned: WebDAV/Nextcloud, SFTP, Jellyfin, Subsonic, internet radio) |
| 443 (as registered) | TCP, UDP 53 (DNS) | outgoing | Push services (UnifiedPush) the users' devices registered, for share notifications |

`7878` is the only port to publish. Outgoing connections go only to
sources the admin configured: no update checks, telemetry or metadata
services. Without remote sources (the current state) the server opens none;
a network policy may then block all outgoing traffic of the container.
AirPlay is a feature of the apps, not of the server.

The server speaks plain HTTP. For access from the internet put a reverse
proxy with HTTPS in front of it (and set `BEATLUMEN_ALLOWED_HOSTS` to its
domain), or use a VPN. Publishing as `127.0.0.1:7878:7878` keeps the port
reachable for a proxy on the same host only.

### Behind a reverse proxy: trusted proxies

The server allows 32 connections at once per client address (IPv6: per
/64) and counts failed logins and room codes per address. Behind a reverse
proxy every request arrives from the proxy, so tell the server which
addresses are proxies with `BEATLUMEN_TRUSTED_PROXIES` (comma separated
addresses or ranges, e.g. `172.18.0.1` or `10.0.0.0/8`). Only their
`X-Forwarded-For` header names the client, and each of their requests counts
against that client while it runs; the header from anyone else is ignored,
so clients cannot pose as others. The default trusts only the loopback
interface, which inside a container is the container itself; `none` trusts
nobody.

Which address the proxy connects from depends on the network: with a
published port it is often the gateway of the container network (Docker:
`172.17.0.1` or the compose network's, e.g. `172.18.0.1`; Podman: `10.88.0.1`),
for a proxy container in the same network that container's address. The
server logs a warning with the address the first time a request with
`X-Forwarded-For` comes from one it does not trust; `BEATLUMEN_VERBOSE=true`
logs the client of every request. Do not trust a range that ordinary clients
can connect from.

## Folders (volumes)

| Path in the container | Mode | Content |
|---|---|---|
| `/music` | read-only | Music library, scanned recursively. Several folders: mount them below `/music` (e.g. `/music/nas`, `/music/local`) or set `BEATLUMEN_MUSIC` |
| `/images` | read-only | Background images |
| `/data` | read-write | The database `beatlumen.redb` (accounts, logins, settings, playlists, favorites, history, presets) with its copy for backups, the accounts' encrypted files, uploads, song texts, the library cache, analyses and covers (keep it: a backup of this volume and the pepper keeps everything, see [Backup](#backup)) |
| `/run/secrets/…`, `/secrets/…` | read-only | Password file and pepper (optional; Podman secrets go to `/secrets`) |

The container user is `10001:10001`. The music and image folders only need
to be readable for it (world-readable files are). `/data` is a named volume
in the examples; with a host folder instead, give the user write access:
`chown 10001:10001 /srv/beatlumen-data` (rootless Podman:
`podman unshare chown 10001:10001 /srv/beatlumen-data`).

On SELinux systems `:z` (shared) or `:Z` (private) relabels a host folder
for container use; do not use them on system folders such as `/home` or
`/usr`.

## Admin interface

The server is managed in the browser at `http://<server>:7878/admin/`:
library folders, music sources, accounts and invitations, shared links,
speech recognition (models are downloaded in the background when picked),
source extensions, network settings, the server password and the log.

On the first start the server logs a one-time link that sets the admin
password (valid for a day):

```sh
docker compose logs server | grep admin     # or: docker logs beatlumen
```

`beatlumen-server admin-link` creates a new link (valid for an hour), also
to reset a forgotten admin password:

```sh
docker exec beatlumen /beatlumen-server admin-link
```

The link contains `<server address>` when the server listens on all
addresses: open it with the address you reach the server by. Without a
browser, `BEATLUMEN_ADMIN_PASSWORD_FILE` (a secret like the server
password) sets the admin password.

Settings made there are kept in the database (`/data/beatlumen.redb`).
Settings given by environment variables or command line parameters stay
fixed and are shown read-only; `compose.yml` therefore passes only the values set in `.env`.
Most changes apply at once; folders, extensions, the public address and
the push options need a restart, which the admin interface does itself.

## Environment variables

| Variable | Default (image) | Meaning |
|---|---|---|
| `BEATLUMEN_MUSIC` | `/music` | Music folders, separated by `:` |
| `BEATLUMEN_IMAGES` | `/images` | Background image folders, separated by `:` |
| `BEATLUMEN_DATA` | `/data` | Data folder |
| `BEATLUMEN_BIND` | `0.0.0.0:7878` | Listen address inside the container |
| `BEATLUMEN_PASSWORD_FILE` | – | File with the login password (secrets); a trailing newline is ignored |
| `BEATLUMEN_PASSWORD` | – | Login password (prefer the file: environment variables show up in `docker inspect`) |
| `BEATLUMEN_NAME` | `Beatlumen Server` | Name shown in the apps |
| `BEATLUMEN_ALLOWED_HOSTS` | – | Host names besides IP addresses, `localhost` and `*.local`/`*.lan`/`*.home.arpa`/`*.internal`, comma separated (domain of a reverse proxy) |
| `BEATLUMEN_TRUSTED_PROXIES` | loopback | Reverse proxies (addresses or ranges, comma separated) whose `X-Forwarded-For` names the client for the limits per client; `none`: no proxy (see [trusted proxies](#behind-a-reverse-proxy-trusted-proxies)) |
| `BEATLUMEN_SOURCE_URL` | official repository | Source code link shown to users; required by the AGPL when you run a modified version for others |
| `BEATLUMEN_RESCAN_INTERVAL` | `600` | Seconds between library rescans, `0` = off |
| `BEATLUMEN_ANALYSIS_MAX_MINUTES` | `20` | Longest track analysed on the server, `0` = unlimited |
| `BEATLUMEN_SCAN_THREADS` | `0` (automatic) | Threads reading tags during scans |
| `BEATLUMEN_WEB_DIR` | embedded | Web client from a mounted folder instead of the embedded one (a `x.gz` next to a file is sent to browsers that accept gzip) |
| `BEATLUMEN_VAULT_PEPPER_FILE` | `/data/vault/pepper` | Secret (at least 32 bytes) mixed into the account verifiers. As a secret outside `/data`, a copy of the volume alone does not even allow testing password guesses. Losing it locks every account out (the files stay encrypted and are lost with it) |
| `BEATLUMEN_VAULT_QUOTA_MB` | `10240` | Encrypted storage per account in MiB, `0` = unlimited |
| `BEATLUMEN_REGISTRATION` | `invite` | `invite`: new accounts with an invitation code; `off`: no new accounts |
| `BEATLUMEN_ADMIN_PASSWORD_FILE` | – | File with the admin password (instead of the setup link) |
| `BEATLUMEN_SPEECH_MODEL` | – | Whisper model file for timing song texts; usually picked in the admin interface (see [Song texts](#song-texts-speech-recognition)) |
| `BEATLUMEN_MODEL_SOURCE` | huggingface.co (whisper.cpp) | Mirror of the speech model files offered in the admin interface |
| `BEATLUMEN_AUTO_SYNC_LYRICS` | `false` | `true`: time the song texts of the whole library in the background (needs the model) |
| `BEATLUMEN_AUTO_LABELS` | `true` | `false`: do not work out how the library's songs sound (labels, recommendations) |
| `BEATLUMEN_KARAOKE` | `false` | `true`: compute karaoke data on request (downloads the vocal separation model) |
| `BEATLUMEN_AUTO_KARAOKE` | `false` | `true`: compute the karaoke data of the whole library in the background |
| `BEATLUMEN_ROOM_GUESTS` | `true` | `false`: people without a login cannot join a room to listen together |
| `BEATLUMEN_OFFER_MODELS` | `true` | `false`: do not offer the speech models in `/data/models` to the apps |
| `BEATLUMEN_ONLINE_METADATA` | `false` | `true`: look the library's artists up in public databases for artist pages and recommendations; their names go to MusicBrainz, Wikimedia and ListenBrainz |
| `BEATLUMEN_PUSH_ALLOW_PRIVATE` | `false` | `true`: allow push services (UnifiedPush, e.g. ntfy) in private networks and over plain http |
| `BEATLUMEN_CA_FILE` | – | Extra root certificates (PEM) for outgoing TLS, e.g. a private CA of a push service at home |
| `BEATLUMEN_VERBOSE` | `false` | `true`: log every request and per-file tag problems |

Without a password the server accepts every client in the network. Set one
whenever others share the network, and always when the server is
reachable from the internet (at least 12 characters).

`compose.yml` reads these settings from `.env` (see `example.env`):
`MUSIC_DIR`, `IMAGES_DIR`, `BEATLUMEN_PORT` (host port) and the
`BEATLUMEN_*` values above.

## Command line parameters

Arguments after the image name are passed to the server and override the
environment variables:

| Parameter | Meaning |
|---|---|
| `--music DIR` | Music folder (repeatable; replaces `BEATLUMEN_MUSIC`) |
| `--images DIR` | Background image folder (repeatable) |
| `--data DIR` | Data folder |
| `--bind ADDR` | Listen address |
| `--password PW` | Password (visible in the process list: prefer the file) |
| `--web-dir DIR` | Web client folder |
| `--rescan-interval SECS` | Seconds between rescans |
| `--name NAME` | Server name |
| `--allow-host NAME` | Extra host name (repeatable) |
| `--trusted-proxy ADDR` | Reverse proxy address or range (repeatable; `none`: no proxy) |
| `--source-url URL` | Source code link |
| `--analysis-max-minutes M` | Longest track analysed |
| `--scan-threads N` | Tag reading threads |
| `--config FILE` | JSON configuration file (mount it read-only) |
| `--vault-pepper-file FILE` | Pepper of the account verifiers |
| `--vault-quota-mb MB` | Encrypted storage per account |
| `--registration MODE` | `invite` or `off` |
| `--speech-model FILE` | Whisper model file for timing song texts |
| `--model-source URL` | Mirror of the speech model files |
| `--auto-sync-lyrics` | Time the song texts of the whole library in the background |
| `--no-auto-labels` | Do not work out how the library's songs sound (labels, recommendations) |
| `--karaoke` | Compute karaoke data on request (downloads the vocal separation model, 36 MB, from zenodo.org) |
| `--auto-karaoke` | Also compute the karaoke data of the whole library in the background |
| `--no-room-guests` | People without a login cannot join a room to listen together |
| `--no-offer-models` | Do not offer the speech models in `/data/models` to the apps |
| `--online-metadata` | Look the library's artists up in public databases for artist pages and recommendations (off by default) |
| `--verbose` | Log every request |
| `--health-check` | Exit status 0 if the server answers (used by the health check) |
| `admin-link` (command) | Prints a one-time link that sets the admin password |
| `--help`, `--version` | |

Precedence: built-in defaults < `--config` file < environment variables <
command line parameters.

## Music sources

Songs on other servers and storage join the library (admin interface,
*Music sources*): Subsonic/OpenSubsonic servers (Navidrome, Gonic,
Airsonic, …), Jellyfin, Nextcloud (log in on the Nextcloud page; the
server gets an app password of its own), WebDAV shares and DLNA media
servers in the network, SFTP folders (own SSH client; the server key is
confirmed on first use) and web folders (plain directory listings; only
links below the given address are followed). The server lists their songs in the background (at
start, after a change and with every rescan), analyses them and plays them
for the apps with seeking; the files stay where they are. Plays are
reported back to Subsonic and Jellyfin.

The logins are kept in the database, encrypted with a key derived
from the vault pepper: with `BEATLUMEN_VAULT_PEPPER_FILE` outside the data
folder, a backup of `/data` alone does not reveal them. Listings are
cached in `/data/sources/`, so the library is complete right after a
restart. Playlist files (M3U, M3U8, PLS, XSPF) in the music folders show
up as read-only playlists; their entries resolve only to songs of the
library. DLNA discovery needs the container in the host network (or
enter the server's description address by hand).

## Song texts (speech recognition)

With a Whisper model the server times plain song texts (`.txt`, tags
without times, texts added in the apps) by recognising the sung words, and
writes down what is sung when a song has no text at all. The recognition
runs on the server itself (own implementation, no external service). In
the apps, the song text view then offers *Sync automatically* (or
*Recognize song text*); with *Time the whole library* the server works
through the whole library in the background on half of the processor
cores. The results are stored in `/data/lyrics`; texts added later replace
them.

The model is picked in the admin interface (*Song texts*): the server
downloads it in the background from the whisper.cpp model page
(`https://huggingface.co/ggerganov/whisper.cpp`, or the mirror set with
`BEATLUMEN_MODEL_SOURCE`) into `/data/models`, checks it against the
SHA-256 in its list and switches to it. *small (q5_1)* (190 MB, about
450 MB of memory) is a good choice for songs; quantised models (q4, q5,
q8) need far less memory than the full ones.

The apps signed in to the server can download the models in
`/data/models` from it (Settings, Song texts in the app), checked against
their SHA-256; `BEATLUMEN_OFFER_MODELS=false` turns that off. Own models
go there too: `ggml-<name>.bin` (the ggml format of whisper.cpp), the
timing layer made for it as `ggml-<name>.head` (a quantised
`ggml-<name>-q5_1.bin` also takes `ggml-<name>.head`) and, if wanted, a
description `ggml-<name>.json`:

```json
{"note": "trained on singing", "rank": 1, "memoryMb": 800}
```

Own models count as better than those of the list (among themselves by
`rank`), so songs timed with those are timed again. The admin interface
shows them under *Song texts*, where they are used or deleted.

Own model files work without the models folder as well: mount a folder
read-only and set the file:

```yaml
    environment:
      BEATLUMEN_SPEECH_MODEL: /models/ggml-small.bin
    volumes:
      - ./models:/models:ro,z
```

Songs longer than 15 minutes are timed only for their first 15 minutes.

## User accounts

Users with an account store end-to-end encrypted files on the server and
share them with each other; neither the admin nor an attacker on the server
can read them (see [`docs/ENCRYPTION.md`](../docs/ENCRYPTION.md)). Accounts
are created in the apps with an invitation code from the admin:

```sh
docker exec beatlumen /beatlumen-server invite            # one account, 7 days
docker exec beatlumen /beatlumen-server invite --days 1
```

(Compose: `docker compose exec server /beatlumen-server invite`.) The
admin interface lists the accounts; `beatlumen-server users` does so only
while the server is stopped, since a running server holds its database.
The pepper (`pepper.bin`) belongs in the backup next to the `/data` volume,
but stored separately from it.

## Health check

`GET /api/health` answers `{"status": "ok"}` (200) while the server serves
requests, its data folder takes writes and its database a write
transaction, else 503 with `problems` (e.g. a full disk or a read-only
`/data`). It needs no login and tells nothing
about the setup, so monitoring can use it too.

The image declares a health check (`/beatlumen-server --health-check`,
every 30 s), which asks that address on the loopback interface. It gets
through even when the limits per client are reached (a few connections
are kept for it). Docker uses it directly; Podman uses it when the image is built in Docker format
(`podman build --format docker`) or through `compose.yml` and the Quadlet
unit, which declare it themselves. `docker ps` / `podman ps` show the
state. Give it in exec form (`["/beatlumen-server","--health-check"]`):
the image has no shell, so a plain command line, which Podman runs
through `/bin/sh -c`, always fails.

## Security

The examples run the container with:

* **an unprivileged user** (`10001`) and **no capabilities**
  (`--cap-drop ALL`), **no privilege gain** (`no-new-privileges`);
* **a read-only root file system** (`--read-only`): only `/data` is
  writable, the music and image folders are mounted read-only;
* **a seccomp allowlist** (`container/seccomp/beatlumen-server.json`):
  only the about 90 system calls the server uses, every other one fails
  with "not implemented". It also allows no executable memory (no
  `mmap`/`mprotect` with `PROT_EXEC`), no new namespaces (`clone` for
  threads only), sockets for IPv4 and IPv6 only and a handful of `ioctl`
  and `prctl` requests;
* **no IPC namespace sharing** (`--ipc none`): no shared memory or message
  queues;
* **no init process**: the server stops on SIGTERM itself (within a
  second or two of `docker stop`) and starts no other processes;
* a process and memory limit (compose: `pids_limit`, `mem_limit`).

The image itself holds the static server binary (linked against musl, no
C library, no shell, no package manager) and the license texts: about
9 MB, or about 14 MB with the web client.

### The seccomp profile

The list comes from tracing the static binary (`strace -f`) while it
scanned a library in all supported formats, streamed and converted songs,
timed song texts with a speech model, served models, playlists,
favorites, history, recommendations and its health check, plus the
command line tools (`--health-check`, `admin-link`, `invite`, `users`).
Calls needed on rare paths (removing folders, outgoing connections and
DNS, UDP discovery, signals) and their `*at` forms for arm64 were added;
`flock` is the database's whole-file lock (`beatlumen.redb`): next to its
byte-range locks (`fcntl`) for older redb versions, and in their place on
file systems without byte-range locks (some network and FUSE ones);
`fstatfs`, `openat2` and `close_range` are for the container runtime
between applying the profile and starting the server.

When a new version needs another call, the kernel log shows it while the
profile's `defaultAction` is set to `SCMP_ACT_LOG` (`journalctl -k |
grep type=1326`).

### AppArmor (Ubuntu, Debian, openSUSE, …)

`apparmor/beatlumen-server` is stricter than the runtimes' default profile:
TCP and UDP sockets only (listening, remote sources, DNS; no raw or packet
sockets), no capabilities, read access to `/music`, `/images`,
`/run/secrets`, `/secrets`, name resolution files and the image's documentation, write
access to `/data` only, no mounts, no ptrace.

```sh
sudo cp container/apparmor/beatlumen-server /etc/apparmor.d/
sudo apparmor_parser -r -W /etc/apparmor.d/beatlumen-server
docker run --security-opt apparmor=beatlumen-server ...
podman run --security-opt apparmor=beatlumen-server ...
```

In `compose.yml`, enable the line `- apparmor=beatlumen-server` under
`security_opt`. Denials show up in the kernel log
(`journalctl -k | grep apparmor`). The profile needs AppArmor 3 or newer
(Ubuntu 22.04, Debian 12 and later).

### SELinux (Fedora, RHEL, CentOS Stream, …)

`selinux/beatlumen_server.cil` defines the type `beatlumen_server.process`
from the udica container templates: like the default container type, but
without any Linux capability and without access to files that are not
labeled for containers.

```sh
sudo dnf install udica container-selinux
sudo semodule -i container/selinux/beatlumen_server.cil \
    /usr/share/udica/templates/{base_container.cil,net_container.cil}
podman run --security-opt label=type:beatlumen_server.process \
    -v /srv/music:/music:ro,z ... beatlumen-server
```

In `compose.yml`, enable the line `- label=type:beatlumen_server.process`;
in the Quadlet unit, `SecurityLabelType=beatlumen_server.process`. Denials:
`sudo ausearch -m avc -ts recent`. Remove with
`sudo semodule -r beatlumen_server`.

## Backup

Everything the server keeps is in the `/data` volume, plus the pepper when
it is a secret of its own (`pepper.bin`, keep it apart from the volume
backup). Most of `/data` are plain files that can be copied at any time.
The database `beatlumen.redb` is the exception: a file copy taken while
the server writes to it can be torn. Two ways to a consistent backup:

* **While the server runs:** within an hour of a change the server writes
  a consistent copy of its database to `/data/backup/beatlumen.redb`
  (through a read transaction, then renamed into place). Back up the volume
  as usual (restic, borg, rsync, a snapshot); in a restore use
  `backup/beatlumen.redb` as `beatlumen.redb`. It is at most an hour behind.
* **The very last state:** stop the container, copy the volume (or just
  `beatlumen.redb`) out of it, start it again. `docker cp` / `podman cp`
  also read from a stopped container, so no other image is needed:

  ```sh
  docker stop beatlumen                      # Compose: docker compose stop server
  docker cp beatlumen:/data ./beatlumen-backup
  docker start beatlumen
  ```

  (Compose names the container `<project>-server-1`, see
  `docker compose ps`; Quadlet: `beatlumen-server`.)

To restore, stop the server, put the files back into the volume (owned by
`10001:10001`), and replace `beatlumen.redb` with `backup/beatlumen.redb`
if the backup was taken while the server ran. The server repairs a
database that was not closed properly when it opens it, and refuses one
written by a newer server version.

## Updating

```sh
git pull
docker compose up -d --build     # or: podman compose up -d --build
```

The data volume is kept; the library is rescanned on start.

**From 0.12.0:** the server keeps its data in a database since 0.13.0 and
does not take over the 0.12.0 files. Accounts with their encrypted files,
logins, playlists, favorites, the listening history, presets, music
sources, the admin password and the settings of the admin interface are set
up again (see the CHANGELOG). Music, images, song texts and caches stay.
