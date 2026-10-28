# Third-party notices

Beatlumen is licensed under the GNU Affero General Public License v3.0 or
later (see `LICENSE`). The only external libraries are the cryptography
crates of the end-to-end encryption (`bl-vault`, see below); the following
third-party works were ported or embedded. All of them are under licenses
compatible with AGPL-3.0. Each crate's `NOTICE` file lists exactly which files
contain the ported code and carries the full license text where required.

| Work | Used in | License |
|---|---|---|
| [midori](https://github.com/aeroheim/midori) | effect shaders (`bl-render`), camera framing and particles (`bl-fx`) | MIT |
| [three.js](https://github.com/mrdoob/three.js) | copy, blend, RGB shift and vignette shaders (`bl-render`) | MIT |
| [webgl-noise](https://github.com/stegu/webgl-noise) | 2D simplex noise shader (`bl-render`) | MIT |
| [minimp3](https://github.com/lieff/minimp3) | MP3 decoder tables and kernels (`bl-mp3`) | CC0 1.0 |
| [libopus](https://opus-codec.org/) 1.5.2 | Opus decoder (`bl-opus`) | BSD-3-Clause |
| [libvorbis](https://xiph.org/vorbis/) | floor 0 and codebook limits (`bl-vorbis`) | BSD-3-Clause |
| [Apple ALAC](https://github.com/macosforge/alac) | ALAC decoder (`bl-media`) | Apache-2.0 |
| RFC 6386 code components | VP8 tables (`bl-image`) | BSD (IETF Trust) |
| [Inter](https://github.com/rsms/inter) typeface | embedded UI font (`apps/beatlumen/assets`) | SIL OFL 1.1 |

Full texts: `crates/bl-opus/NOTICE`, `crates/bl-vorbis/NOTICE`,
`crates/bl-media/NOTICE`, `crates/bl-mp3/NOTICE`,
`crates/bl-image/NOTICE`, `apps/beatlumen/assets/Inter-OFL.txt`, and the MIT texts
below.

The end-to-end encryption (`bl-vault`) uses audited cryptography crates
instead of an own implementation: the [RustCrypto](https://github.com/RustCrypto)
crates `argon2`, `hkdf`, `sha2`, `chacha20poly1305` (MIT OR Apache-2.0,
used under MIT) and [dalek-cryptography](https://github.com/dalek-cryptography)'s
`ed25519-dalek`, `x25519-dalek`, `curve25519-dalek` and `subtle`
(BSD-3-Clause), with their dependencies. `crates/bl-vault/NOTICE` lists
every crate with its copyright notice and full license text.

The AirPlay receiver (`bl-airplay`) is an independent implementation of the
AirPlay 1 (RAOP) protocol for interoperability. It contains the RSA key of
AirPlay 1 receivers (the "AirPort Express key", public since 2011 and
distributed with open-source receivers such as
[Shairport Sync](https://github.com/mikebrady/shairport-sync)); no code of
those projects is used. AirPlay is a trademark of Apple Inc.; Beatlumen is
not affiliated with or certified by Apple.

Beatlumen reads and writes Hyperchroma's `.hfx` preset format for
compatibility. It contains no code or assets from Hyperchroma.

## MIT License (midori, three.js, webgl-noise)

```
midori:      Copyright (c) 2020 Benjamin Pang
three.js:    Copyright (c) 2010-2024 three.js authors
webgl-noise: Copyright (C) 2011 Ashima Arts, Copyright (C) 2011-2016 Stefan Gustavson

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Qt and Material Symbols

The application user interface uses [Qt 6](https://www.qt.io/) under the GNU
LGPL v3 (dynamically linked). The Android app ships a modified Qt QML
library (`libQt6Qml`): it is built from the official Qt source with the
patches in `apps/beatlumen/android/qt-patches/` by `build-qml.sh` there, and
can be replaced by any compatible build of the library. The icons in
`apps/beatlumen/icons` are [Material Symbols](https://github.com/google/material-design-icons)
by Google, licensed under the Apache License 2.0.
