# Third-party notices

Beatlumen is licensed under the GNU Affero General Public License v3.0 or
later (see `LICENSE`). The only external libraries are the cryptography
crates of the end-to-end encryption (`bl-vault`) and the server's TLS
(see below); the following
third-party works were ported or embedded. All of them are under licenses
compatible with AGPL-3.0. Each crate's `NOTICE` file lists exactly which files
contain the ported code and carries the full license text where required.

| Work | Used in | License |
|---|---|---|
| [midori](https://github.com/aeroheim/midori) | effect shaders (`bl-render`), camera framing and particles (`bl-fx`) | MIT |
| [three.js](https://github.com/mrdoob/three.js) | copy, blend, RGB shift and vignette shaders (`bl-render`) | MIT |
| [webgl-noise](https://github.com/stegu/webgl-noise) | 2D simplex noise shader (`bl-render`) | MIT |
| [minimp3](https://github.com/lieff/minimp3) | MP3 decoder tables and kernels (`bl-mp3`) | CC0 1.0 |
| [libopus](https://opus-codec.org/) 1.5.2 | Opus decoder and CELT encoder (`bl-opus`) | BSD-3-Clause |
| [libvorbis](https://xiph.org/vorbis/) | floor 0 and codebook limits (`bl-vorbis`) | BSD-3-Clause |
| [Apple ALAC](https://github.com/macosforge/alac) | ALAC decoder (`bl-media`) | Apache-2.0 |
| RFC 6386 code components | VP8 tables (`bl-image`) | BSD (IETF Trust) |
| [Inter](https://github.com/rsms/inter) typeface | embedded UI font (`apps/beatlumen/assets`) | SIL OFL 1.1 |
| [Open-Unmix](https://github.com/sigsep/open-unmix-pytorch) | vocal separation network and Wiener filter (`bl-sep`); UMX-HQ weights downloaded on use | MIT |
| [wav2vec2](https://huggingface.co/facebook/wav2vec2-base-960h) models (facebook/wav2vec2-base-960h, jonatasgrosman/wav2vec2-large-xlsr-53-*) | word alignment (`bl-align`); weights downloaded on use, not shipped | Apache-2.0 |
| [TeX hyphenation patterns](https://github.com/hyphenation/tex-hyphen) (hyph-utf8) | embedded patterns for English, German, French, Spanish, Italian, Dutch, Portuguese (`bl-hyph`) | MIT, BSD-3-Clause, permissive (per language) |

Only when the listener or the server's admin switches on artist information from
the internet (off by default), these services are asked at run time; nothing of
theirs is shipped with Beatlumen, and the artist page names its sources.

| Service | Used for | Terms of the data |
|---|---|---|
| [MusicBrainz](https://musicbrainz.org/doc/About/Data_License) | artists, members, links, genres | core data CC0; other data CC BY-NC-SA 3.0 |
| [Wikidata](https://www.wikidata.org/wiki/Wikidata:Licensing) | where to find the page and the picture | CC0 |
| [Wikipedia](https://en.wikipedia.org/wiki/Wikipedia:Copyrights) | biography, shown with a link to the page | CC BY-SA 4.0 |
| [Wikimedia Commons](https://commons.wikimedia.org/wiki/Commons:Licensing) | picture, shown with author, license and a link | per picture |
| [ListenBrainz](https://listenbrainz.org) | similar artists | public API of the MetaBrainz Foundation |

Full texts: `crates/bl-opus/NOTICE`, `crates/bl-vorbis/NOTICE`,
`crates/bl-media/NOTICE`, `crates/bl-mp3/NOTICE`,
`crates/bl-image/NOTICE`, `crates/bl-sep/NOTICE`, `crates/bl-hyph/patterns/LICENSE-*.txt`,
`apps/beatlumen/assets/Inter-OFL.txt`, and the MIT texts
below.

The end-to-end encryption (`bl-vault`) uses audited cryptography crates
instead of an own implementation: the [RustCrypto](https://github.com/RustCrypto)
crates `argon2`, `hkdf`, `sha2`, `chacha20poly1305` (MIT OR Apache-2.0,
used under MIT) and [dalek-cryptography](https://github.com/dalek-cryptography)'s
`ed25519-dalek`, `x25519-dalek`, `curve25519-dalek` and `subtle`
(BSD-3-Clause), with their dependencies, plus `p256` and `aes-gcm` for
Web Push encryption. `crates/bl-vault/NOTICE` lists every crate with its
copyright notice and full license text.

The server uses [rustls](https://github.com/rustls/rustls) with
[ring](https://github.com/briansmith/ring) for TLS of its outgoing
connections (push services) and Mozilla's root certificates from
`webpki-roots` (CDLA-Permissive-2.0), and keeps its own data in a
[redb](https://github.com/cberner/redb) database (Copyright (c) 2021
Christopher Berner; MIT OR Apache-2.0, used under MIT);
`apps/beatlumen-server/NOTICE` has the full texts. In the container image these notices are in
`/usr/share/doc/beatlumen/`.

The AirPlay receiver (`bl-airplay`) is an independent implementation of the
AirPlay 1 (RAOP) protocol for interoperability. It contains the RSA key of
AirPlay 1 receivers (the "AirPort Express key", public since 2011 and
distributed with open-source receivers such as
[Shairport Sync](https://github.com/mikebrady/shairport-sync)); no code of
those projects is used. AirPlay is a trademark of Apple Inc.; Beatlumen is
not affiliated with or certified by Apple.

Beatlumen reads and writes Hyperchroma's `.hfx` preset format for
compatibility. It contains no code or assets from Hyperchroma.

The image decoders (`bl-image`: JPEG, PNG/APNG, GIF, WebP, BMP, TIFF, AVIF
with its AV1 decoder, and the HEIF container) are own implementations
written from the format specifications. Their tests create sample images at
test time with ffmpeg (with libaom, SVT-AV1, rav1e and dav1d), ImageMagick,
jpegtran or Pillow when these programs are installed and compare against
them; none of these programs or their code is part of Beatlumen.

The constant tables of the AV1 decoder (default probabilities, scan orders,
quantizer tables and matrices, filter taps) were extracted from the text of
the [AV1 Bitstream & Decoding Process
Specification](https://aomediacodec.github.io/av1-spec/), copyright the
Alliance for Open Media, by `crates/bl-image/tools/av1_tables.py`. AV1 is
covered by the Alliance for Open Media Patent License 1.0, which has to
accompany implementations: its full text is below and in
`crates/bl-image/PATENTS-AV1.md`.

HEIC pictures are decoded by the operating system: Android's ImageDecoder,
the Windows Imaging Component with Microsoft's HEIF and HEVC extensions, or
on Linux the system's libheif when it is installed (loaded at run time).
Beatlumen contains no HEVC decoder and ships none.

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
by Google, licensed under the Apache License 2.0; `smart_playlist.svg` was drawn
for Beatlumen in their style.

## Alliance for Open Media Patent License 1.0 (AV1)

```
Alliance for Open Media Patent License 1.0

1. License Terms

1.1. Patent License

Subject to the terms and conditions of this License, each Licensor, on behalf
of itself and successors in interest and assigns, grants Licensee a non-
sublicensable, perpetual, worldwide, non-exclusive, no-charge, royalty-free,
irrevocable (except as expressly stated in this License) patent license to its
Necessary Claims to make, use, sell, offer for sale, import or distribute any
Implementation.

1.2. Conditions

1.2.1. Availability

As a condition to the grant of rights to Licensee to make, sell, offer for
sale, import or distribute an Implementation under Section 1.1, Licensee must
make its Necessary Claims available under this License, and must reproduce
this License with any Implementation as follows:

  a. For distribution in source code, by including this License in the root
     directory of the source code with its Implementation.

  b. For distribution in any other form (including binary, object form, and/or
     hardware description code (e.g., HDL, RTL, Gate Level Netlist, GDSII,
     etc.)), by including this License in the documentation, legal notices,
     and/or other written materials provided with the Implementation.

1.2.2. Additional Conditions

This license is directly from Licensor to Licensee. Licensee acknowledges as a
condition of benefiting from it that no rights from Licensor are received from
suppliers, distributors, or otherwise in connection with this License.

1.3. Defensive Termination

If any Licensee, its Affiliates, or its agents initiates patent litigation or
files, maintains, or voluntarily participates in a lawsuit against another
entity or any person asserting that any Implementation infringes Necessary
Claims, any patent licenses granted under this License directly to the
Licensee are immediately terminated as of the date of the initiation of action
unless 1) that suit was in response to a corresponding suit regarding an
Implementation first brought against an initiating entity, or 2) that suit was
brought to enforce the terms of this License (including intervention in a
third-party action by a Licensee).

1.4. Disclaimers

The Reference Implementation and Specification are provided "AS IS" and
without warranty. The entire risk as to implementing or otherwise using the
Reference Implementation or Specification is assumed by the implementer and
user. Licensor expressly disclaims any warranties (express, implied, or
otherwise), including implied warranties of merchantability, non-infringement,
fitness for a particular purpose, or title, related to the material. IN NO
EVENT WILL LICENSOR BE LIABLE TO ANY OTHER PARTY FOR LOST PROFITS OR ANY FORM
OF INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES OF ANY CHARACTER
FROM ANY CAUSES OF ACTION OF ANY KIND WITH RESPECT TO THIS LICENSE, WHETHER
BASED ON BREACH OF CONTRACT, TORT (INCLUDING NEGLIGENCE), OR OTHERWISE, AND
WHETHER OR NOT THE OTHER PARTRY HAS BEEN ADVISED OF THE POSSIBILITY OF SUCH
DAMAGE.

2. Definitions

2.1. Affiliate

"Affiliate" means an entity that directly or indirectly Controls, is Controlled
by, or is under common Control of that party.

2.2. Control

"Control" means direct or indirect control of more than 50% of the voting power
to elect directors of that corporation, or for any other entity, the power to
direct management of such entity.

2.3. Decoder

"Decoder" means any decoder that conforms fully with all non-optional portions
of the Specification.

2.4. Encoder

"Encoder" means any encoder that produces a bitstream that can be decoded by a
Decoder only to the extent it produces such a bitstream.

2.5. Final Deliverable

"Final Deliverable" means the final version of a deliverable approved by the
Alliance for Open Media as a Final Deliverable.

2.6. Implementation

"Implementation" means any implementation, including the Reference
Implementation, that is an Encoder and/or a Decoder. An Implementation also
includes components of an Implementation only to the extent they are used as
part of an Implementation.

2.7. License

"License" means this license.

2.8. Licensee

"Licensee" means any person or entity who exercises patent rights granted under
this License.

2.9. Licensor

"Licensor" means (i) any Licensee that makes, sells, offers for sale, imports or
distributes any Implementation, or (ii) a person or entity that has a licensing
obligation to the Implementation as a result of its membership and/or
participation in the Alliance for Open Media working group that developed the
Specification.

2.10. Necessary Claims

"Necessary Claims" means all claims of patents or patent applications, (a) that
currently or at any time in the future, are owned or controlled by the Licensor,
and (b) (i) would be an Essential Claim as defined by the W3C Policy as of
February 5, 2004
(<https://www.w3.org/Consortium/Patent-Policy-20040205/#def-essential>) as if
the Specification was a W3C Recommendation; or (ii) are infringed by the
Reference Implementation.

2.11. Reference Implementation

"Reference Implementation" means an Encoder and/or Decoder released by the
Alliance for Open Media as a Final Deliverable.

2.12. Specification

"Specification" means the specification designated by the Alliance for Open
Media as a Final Deliverable for which this License was issued.
```
