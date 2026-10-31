# Handover: song text timing, local work

State after the 0.11.1 beta, for further optimisation and tests on a local
machine. What was tried and measured so far is in
[SPEECH-NOTES.md](SPEECH-NOTES.md); this file is about how to continue.

## Where things stand

- **Release:** `v0.11.1` (pre-release) is published from main at
  `2b27462`. It contains the timing fixes, syllables, the song text editor
  (tap along, move and edit lines) and the tidier song text menu. See
  [CHANGELOG.md](../CHANGELOG.md).
- **Branches:** main is current. `speech-fixes` (merged with
  RandomNam3/Beatlumen#11) and `release-check` (merged with
  RandomNam3/Beatlumen#10) can be deleted.
- **Not used by default:** vocal separation (`bl-sep`) and CTC word
  alignment (`bl-align`). Both are built and work, but measured worse (see
  SPEECH-NOTES). They are reachable from the `transcribe` example
  (`ALIGN_DIR`, `VOCALS`) for further tests.

## Code map

| Part | Where |
|---|---|
| Whole song: plain text, LRC or written down | `crates/bl-asr/src/sync.rs` (`time_song`, `lrc_offset`) |
| Matching a known text to the recognition | `crates/bl-asr/src/align.rs` (`anchor_lines`, `align_words`, `time_anchors`, `pause_lines`) |
| Written-down lines | `crates/bl-asr/src/align.rs` (`split_long`, `capitalized`, `spell_like_title`) |
| Word times per line (attention, energy refinement) | `crates/bl-asr/src/transcribe.rs` (`time_lines`, `WordAligner`) |
| Syllable times | `crates/bl-asr/src/timing.rs` (`syllable_pieces`), `crates/bl-hyph` |
| Decoding, timestamp rules | `crates/bl-asr/src/transcribe.rs` (`transcribe`) |
| Model catalog (retired models) | `crates/bl-asr/src/models.rs` |
| GPU encoder | `crates/bl-gpu`, `crates/bl-asr/src/gpu.rs` (feature `gpu`) |
| Vocal separation, CTC alignment | `crates/bl-sep`, `crates/bl-align` (`set.rs`: `Aligners`) |
| App side: speech jobs | `crates/bl-core/src/speech.rs`, `lyrics.rs` (`lyrics_edit`, `to_enhanced_lrc`) |
| Server side | `apps/beatlumen-server/src/lyrics_sync.rs` |
| UI | `apps/beatlumen/qml/LyricsView.qml` (⋮ menu), `LyricsEditor.qml`, `SettingsPage.qml` |

## Local setup

- Rust stable (MSRV 1.80: no `Option::is_none_or` and similar), Python 3,
  ffmpeg.
- For the apps: Qt 6.11.3 (see `README.md` "Building", `build-appimage.sh`,
  `build-android.sh`, `build-web.sh`; each lists its environment variables
  at the top).
- Speech models: `ggml-<id>.bin` from
  https://huggingface.co/ggerganov/whisper.cpp in one folder. Used in the
  tests: `base-q5_1`, `small`, `large-v3-turbo-q5_0`.

```sh
cargo test -p bl-asr -p bl-hyph -p bl-align -p bl-sep -p bl-core -p beatlumen-server
cargo build --release -p bl-asr --example transcribe            # CPU
cargo build --release -p bl-asr --example transcribe --features gpu  # Vulkan encoder
```

## Test loop

The tools and their use are in
[`tools/speech-eval/README.md`](../tools/speech-eval/README.md). In short:

1. Put the test songs as `songs/<name>/<name>.wav` (16 kHz mono) and
   `<name>.lrc` (reference) outside the repository.
2. `python3 tools/speech-eval/eval.py ~/models songs/*/`: one JSON line per
   song and model.
3. For a closer look: `cmp_text.py REF.lrc OUT.lrc v` (every line's error)
   and the `align` example with `offset.py` (anchors against the
   reference).

Rebuild the example after each change; `eval.py` uses
`target/release/examples/transcribe`.

### Test songs used so far

| Song | Why | Reference |
|---|---|---|
| FRIENDS (R3hab Remix) | long drop, spelled words, backing vocals | LRC from the owner's files (SongSync), checked |
| Who Let The Dogs Out | dense chants, long repeated outro | LRC from the owner's files (SongSync), checked |
| Bad Ideas (Alle Farben) | clean English vocals | LRCLIB, checked |
| Zukunft Pink (Peter Fox feat. Inéz) | German rap, two voices | SongSync and LRCLIB LRCs, both for another release (about 3 s early); the timing's offset check corrects for that |

Use the same songs again to compare with the numbers below, and add songs
the earlier changes were not tuned on.

### Baseline (0.11.1)

Timing a known text: lines within 1 s of the reference (median line error).

| Song | base-q5_1 | small | large-v3-turbo-q5_0 |
|---|---|---|---|
| FRIENDS | 83 % (0.20 s) | 92 % (0.37 s) | 87 % (0.27 s) |
| Who Let The Dogs Out | 45 % (2.01 s) | 45 % (2.16 s) | 70 % (0.31 s) |
| Bad Ideas | 67 % (0.54 s) | 97 % (0.31 s) | 77 % (0.39 s) |

Written down without a text: word error rate.

| Song | base-q5_1 | small | large-v3-turbo-q5_0 |
|---|---|---|---|
| FRIENDS | 41 % | 27 % | 23 % |
| Who Let The Dogs Out | 75 % | 70 % | 59 % |
| Bad Ideas | 47 % | 21 % | 37 % |

A change counts as better only if no song gets clearly worse. The dense
song (Who Let The Dogs Out) is the one that breaks most changes.

## Open points, by likely benefit

1. **Dense music with the small models** (Who Let The Dogs Out: 45 % of
   lines within 1 s). The recognition misses most chant lines, so few
   anchors exist; the lines between them are timed by the attention alone.
   Ideas: anchors from single rare words when no pair matches nearby;
   timing unanchored lines by the energy envelope of the vocals band.
2. **large v3 turbo skips lines when writing down** (Bad Ideas: the first
   line "Lied to my face" and some repeated choruses), and writes long
   segments without timestamps. Changing the timestamp rule was tried and
   failed (see SPEECH-NOTES: it broke the dense song). An option left: a
   second pass over long gaps between segments.
3. **CTC alignment onset:** `bl-align` word starts come 0.2–0.35 s late.
   With an onset correction (shift by the measured lead, or start at the
   first frame above a probability threshold) it may beat the attention
   timing; then switch it on per language when the model is present.
4. **Voices:** two singers are not told apart when the text does not name
   them (Zukunft Pink). Needs a speaker-embedding model (own
   implementation, open weights and licence); clustering Whisper or MFCC
   features found sections, not voices.
5. **Line breaks in written-down texts** fall mid-sentence in dense music
   (no pauses). A better rule could use rhyme or the beat grid
   (`bl-analysis`).
6. **Stronger separation** (Demucs class) could help recognition where
   Open-Unmix hurt; check the weights' licence first.

Feedback from use that the changes answered, to check again in the app:
"F-R-I-E-N-D-S" spelled right; turbo lines no longer too long; the
Zukunft Pink LRC no longer about 2 s off.

## Rules for changes

- Own implementations, as few external libraries as possible, AGPL-3.0;
  new third-party data (weights, patterns) goes into
  `THIRD_PARTY_NOTICES.md`.
- Apps contact nothing the user did not set up: no lyrics databases in
  Beatlumen. LRCLIB is for the test references only.
- No prompt with the song text when timing a known text (see SPEECH-NOTES).
- Changes to the decoding are measured on all test songs, the dense one
  included.
- Findings go into `SPEECH-NOTES.md`, user-facing changes into
  `CHANGELOG.md`.

## Release

1. Version in `Cargo.toml` (workspace), `apps/beatlumen/CMakeLists.txt`
   (`project VERSION`, `BL_ANDROID_VERSION_CODE`) and `build-android.sh`
   (`VERSION_CODE` default); `cargo update -w`; changelog section.
2. Merge into main and wait for CI on main (Android APK signed with the
   repository secrets).
3. Actions → Release → Run workflow with that CI run's id and
   "Pre-release" for a beta. The workflow tags the commit, takes the notes
   from CHANGELOG.md, checks the APK's signing certificate and publishes
   the files with `SHA256SUMS`.

The Windows test job failed once while linking an example (standard
library symbols missing) and passed when run again without changes; if it
happens again, rerun the job before looking for a cause.
