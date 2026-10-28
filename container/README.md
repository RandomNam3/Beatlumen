# Beatlumen Server in a container

The server runs in a minimal image built `FROM scratch` (no base image, not
even a distroless one). It contains only what the server needs, about 14 MB:

| Path | Why |
|---|---|
| `/beatlumen-server` | The server: a static binary (no libraries) with the web client embedded, gzip-compressed |
| `/data/` | Empty data folder owned by the server's user, so a new volume on it is writable |
| `/usr/share/doc/beatlumen/` | License texts, which the AGPL requires to accompany the program |

There is no shell, no package manager and no user database: the server
runs as the numeric user `10001`. CA certificates will be added once the
server connects to HTTPS remote sources itself (see `docs/ROADMAP.md`);
until then they would be unused. It works with Docker and Podman (rootful and
rootless).

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
podman build -f container/Containerfile -t beatlumen-server .
```

The **web client** (`http://<server>:7878/` in a browser) is embedded when
`dist/web` exists: build it with `./build-web.sh`, or unpack the
`beatlumen-web` artifact of a CI run to `dist/web`. Without it, the server
shows a placeholder page; the apps work either way.

Build argument `RUST_IMAGE` selects the Rust build image (default
`docker.io/library/rust:1.99-alpine3.22`; it is only used for building and
is not part of the result). When Docker Hub limits anonymous pulls, use a
mirror: `--build-arg RUST_IMAGE=mirror.gcr.io/library/rust:1.99-alpine3.22`. The image builds for the
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

### docker run / podman run

```sh
docker run -d --name beatlumen --init --restart unless-stopped \
  -p 7878:7878 \
  -v /srv/music:/music:ro,z \
  -v /srv/backgrounds:/images:ro,z \
  -v beatlumen-data:/data:Z \
  -e BEATLUMEN_PASSWORD_FILE=/run/secrets/password \
  -v "$PWD/password.txt":/run/secrets/password:ro,z \
  -e BEATLUMEN_VAULT_PEPPER_FILE=/run/secrets/pepper \
  -v "$PWD/pepper.bin":/run/secrets/pepper:ro,z \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  beatlumen-server
```

`podman run` takes the same options. With Podman, the password is better
kept as a Podman secret:

```sh
printf '%s' 'a-long-password' | podman secret create beatlumen_password -
podman run ... --secret beatlumen_password,uid=10001,mode=0400 \
  -e BEATLUMEN_PASSWORD_FILE=/run/secrets/beatlumen_password ...
```

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

`7878` is the only port to publish. Outgoing connections go only to
sources the admin configured: no update checks, telemetry or metadata
services. Without remote sources (the current state) the server opens none;
a network policy may then block all outgoing traffic of the container.
AirPlay is a feature of the apps, not of the server.

The server speaks plain HTTP. For access from the internet put a reverse
proxy with HTTPS in front of it (and set `BEATLUMEN_ALLOWED_HOSTS` to its
domain), or use a VPN. Publishing as `127.0.0.1:7878:7878` keeps the port
reachable for a proxy on the same host only.

## Folders (volumes)

| Path in the container | Mode | Content |
|---|---|---|
| `/music` | read-only | Music library, scanned recursively. Several folders: mount them below `/music` (e.g. `/music/nas`, `/music/local`) or set `BEATLUMEN_MUSIC` |
| `/images` | read-only | Background images |
| `/data` | read-write | Library cache, analyses, covers, playlists, presets, uploads, login tokens, accounts and their encrypted files (keep it: a backup of this volume and the pepper keeps everything) |
| `/run/secrets/…` | read-only | Password file and pepper (optional) |

The container user is `10001:10001`. The music and image folders only need
to be readable for it (world-readable files are). `/data` is a named volume
in the examples; with a host folder instead, give the user write access:
`chown 10001:10001 /srv/beatlumen-data` (rootless Podman:
`podman unshare chown 10001:10001 /srv/beatlumen-data`).

On SELinux systems `:z` (shared) or `:Z` (private) relabels a host folder
for container use; do not use them on system folders such as `/home` or
`/usr`.

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
| `BEATLUMEN_SOURCE_URL` | official repository | Source code link shown to users; required by the AGPL when you run a modified version for others |
| `BEATLUMEN_RESCAN_INTERVAL` | `600` | Seconds between library rescans, `0` = off |
| `BEATLUMEN_ANALYSIS_MAX_MINUTES` | `20` | Longest track analysed on the server, `0` = unlimited |
| `BEATLUMEN_SCAN_THREADS` | `0` (automatic) | Threads reading tags during scans |
| `BEATLUMEN_WEB_DIR` | embedded | Web client from a mounted folder instead of the embedded one (a `x.gz` next to a file is sent to browsers that accept gzip) |
| `BEATLUMEN_VAULT_PEPPER_FILE` | `/data/vault/pepper` | Secret (at least 32 bytes) mixed into the account verifiers. As a secret outside `/data`, a copy of the volume alone does not even allow testing password guesses. Losing it locks every account out (the files stay encrypted and are lost with it) |
| `BEATLUMEN_VAULT_QUOTA_MB` | `10240` | Encrypted storage per account in MiB, `0` = unlimited |
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
| `--source-url URL` | Source code link |
| `--analysis-max-minutes M` | Longest track analysed |
| `--scan-threads N` | Tag reading threads |
| `--config FILE` | JSON configuration file (mount it read-only) |
| `--vault-pepper-file FILE` | Pepper of the account verifiers |
| `--vault-quota-mb MB` | Encrypted storage per account |
| `--verbose` | Log every request |
| `--health-check` | Exit status 0 if the server answers (used by the health check) |
| `--help`, `--version` | |

Precedence: built-in defaults < `--config` file < environment variables <
command line parameters.

## User accounts

Users with an account store end-to-end encrypted files on the server and
share them with each other; neither the admin nor an attacker on the server
can read them (see [`docs/ENCRYPTION.md`](../docs/ENCRYPTION.md)). Accounts
are created in the apps with an invitation code from the admin:

```sh
docker exec beatlumen /beatlumen-server invite            # one account, 7 days
docker exec beatlumen /beatlumen-server invite --days 1
docker exec beatlumen /beatlumen-server users             # list accounts
```

(Compose: `docker compose exec server /beatlumen-server invite`.) The
pepper (`pepper.bin`) belongs in the backup next to the `/data` volume, but
stored separately from it.

## Health check

The image declares a health check (`/beatlumen-server --health-check`,
every 30 s), which asks the server's info page on the loopback interface.
Docker uses it directly; Podman uses it when the image is built in Docker
format (`podman build --format docker`) or through `compose.yml` and the
Quadlet unit, which declare it themselves. `docker ps` / `podman ps` show
the state.

## Security

The examples run the container with:

* **an unprivileged user** (`10001`) and **no capabilities**
  (`--cap-drop ALL`), **no privilege gain** (`no-new-privileges`);
* **a read-only root file system** (`--read-only`): only `/data` is
  writable, the music and image folders are mounted read-only;
* **an init process** (`--init`) that forwards stop signals, so the
  server ends right away on `docker stop`;
* a process and memory limit (compose: `pids_limit`, `mem_limit`).

### AppArmor (Ubuntu, Debian, openSUSE, …)

`apparmor/beatlumen-server` is stricter than the runtimes' default profile:
TCP and UDP sockets only (listening, remote sources, DNS; no raw or packet
sockets), no capabilities, read access to `/music`, `/images`,
`/run/secrets`, name resolution files and the image's documentation, write
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

## Updating

```sh
git pull
docker compose up -d --build     # or: podman compose up -d --build
```

The data volume is kept; the library is rescanned on start.
