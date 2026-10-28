<p align="center"><img src="apps/beatlumen/assets/icon.png" width="128" alt=""></p>

# Beatlumen

Beatlumen is a music player that turns your songs into beat-synchronised
visuals: background images come alive with camera moves, particles, light and
colour effects that follow the rhythm and the structure of every track.
It is an open-source successor to the discontinued *Hyperchroma* and reads
its `.hfx` preset files.

*Deutsch:* Beatlumen ist ein Musikplayer, der deine Songs in rhythmische
Visuals verwandelt. Er läuft unter Linux, Windows, Android und im Browser,
spielt lokale Dateien ab und streamt von deinem eigenen Beatlumen-Server.

## Features

- Plays MP3, AAC/M4A, ALAC, Opus, Vorbis, FLAC, WAV/AIFF and Matroska/WebM
  audio with its own decoders (written in Rust).
- Audio analysis finds beats (lows, mids, highs) and song sections; effects
  react to both. *Adaptive* mode keeps quiet parts calm and lets drops hit.
- Styles (presets) with camera, particle, filter and transition effects, an
  intensity control and a full editor for every effect.
- Library with songs, albums, artists, folders, playlists and a play queue;
  artist and title are guessed from file names when tags are missing.
- Background images from your folders, the server or the album cover.
- Light/dark mode and accent colour follow your system.
- **Beatlumen Server** shares a music folder on your network and serves the
  web version of the app.

**Privacy:** the apps only ever connect to the server you configure; the
server makes no outgoing connections at all. No accounts, no tracking.

## Platforms and downloads

| Platform | Build |
|---|---|
| Linux | `apps/beatlumen` (Qt 6, CMake) |
| Windows | same, built by CI with MSVC (`windeployqt`) |
| Android | `./build-android.sh` → `dist/android/beatlumen.apk` |
| Web | `./build-web.sh` → `dist/web`, served by the server |
| Server | `cargo build --release -p beatlumen-server` |

## Building

Requirements: Rust (stable), CMake ≥ 3.21, Ninja, Qt 6.11 with Qt Multimedia,
Qt Shader Tools and Qt Image Formats.

```sh
# Desktop app (Linux / Windows)
cmake -S apps/beatlumen -B build/desktop -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_PREFIX_PATH=/path/to/Qt/6.11.3/gcc_64
cmake --build build/desktop
./build/desktop/beatlumen [--server http://host:7878] [files...]

# Server
cargo build --release -p beatlumen-server
./target/release/beatlumen-server --music ~/Music --web-dir dist/web [--password secret]

# Web version (needs emsdk 4.0.7 and Qt for WebAssembly, single-threaded)
EMSDK_DIR=~/emsdk ./build-web.sh

# Android (needs the Android SDK/NDK and Qt for Android)
./build-android.sh
```

The CMake build compiles the Rust core (`crates/bl-ffi`) through cargo
automatically. Tests: `cargo test --workspace --release` (some tests generate
fixtures with ffmpeg and are skipped without it).

## Architecture

```
apps/beatlumen          Qt Quick user interface (QML + a small C++ bridge)
apps/beatlumen-server   HTTP server: library, streaming, analysis, web app
crates/bl-core          player, library, queues, styles, visualizer (JSON command API)
crates/bl-ffi           C interface of bl-core for the Qt app
crates/bl-fx            effect engine (triggers, camera, particles, transitions)
crates/bl-render        OpenGL / OpenGL ES / WebGL 2 renderer
crates/bl-analysis      FFT, beat and section analysis
crates/bl-media         containers, tags, resampling; codecs in bl-mp3, bl-aac,
                        bl-opus, bl-vorbis, bl-flac, bl-ogg
crates/bl-net, bl-json, bl-image   HTTP, JSON and image codecs
```

The Rust crates use no third-party crates. See `docs/ROADMAP.md` for planned
features (lyrics, user accounts, sharing, provider integrations).

## License

GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later).
Third-party material and its licenses are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

If you run a modified `beatlumen-server` for other people, the AGPL requires
you to offer them the source code of your version: start the server with
`--source-url <link to your source>`. The web app shows that link under
Settings → About.
Beatlumen is not affiliated with Hyperchroma and contains none of its code
or assets.
