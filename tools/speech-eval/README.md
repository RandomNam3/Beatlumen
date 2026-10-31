# Speech evaluation tools

Scripts that measure how well the song text recognition and timing
(`crates/bl-asr`) work on real songs. They are test tools only: Beatlumen
itself contacts no lyrics database. Findings so far are in
[`docs/SPEECH-NOTES.md`](../../docs/SPEECH-NOTES.md).

Needs Python 3 (standard library only) and ffmpeg/ffprobe.

## Test songs

Each song is a folder named like its files:

```text
songs/friends/friends.wav   16 kHz mono 16-bit PCM
songs/friends/friends.lrc   reference times (line times are enough)
```

Make the WAV with:

```sh
ffmpeg -i song.mp3 -ac 1 -ar 16000 -c:a pcm_s16le songs/friends/friends.wav
```

Songs and reference texts are not in the repository (copyright). Keep them
outside it, or under an ignored folder.

References from LRCLIB (https://lrclib.net), for testing only:

- `lrclib_refs.py OUT_DIR AUDIO_FILE...` reads each file's tags (title,
  artist, album, length), fetches the synced text and writes the folder
  above (WAV converted).
- `lrclib_get.py TITLE ARTIST DURATION OUT.lrc` is one search, printing all
  matches (to pick another release by hand).

**Check every reference first.** An LRC of the same length can belong to
another release: other tempo (times drift), other intro (all lines shifted).
Compare it with a recognition (`cmp_text.py`) before trusting it.

## Running

Build the example once (add `--features gpu` for the Vulkan encoder):

```sh
cargo build --release -p bl-asr --example transcribe
```

Models are the whisper.cpp GGML files (`ggml-<id>.bin`, from
https://huggingface.co/ggerganov/whisper.cpp), in one folder. Then:

```sh
MODELS="base-q5_1 small large-v3-turbo-q5_0" \
  python3 tools/speech-eval/eval.py ~/models songs/friends songs/dogs songs/badideas
```

For each song and model it prints one JSON line:

| Field | Meaning |
|---|---|
| `wer` | word error rate, song written down without a text |
| `lines` | lines timed / lines in the reference |
| `line_err_median` | median line start error (s), timing the known text |
| `within_0_5s`, `within_1s` | share of lines within 0.5 s / 1 s |
| `secs_none`, `secs_text` | run times of both tasks |

The LRC outputs go to `speech-eval-out/` (`<song>-<model>-none.lrc` and
`-text.lrc`), `results.json` collects all lines. Settings by environment:
`TRANSCRIBE` (binary), `MODELS`, `OUT` (output folder).

The line errors need the same line count as the reference. Otherwise
compare by text:

```sh
python3 tools/speech-eval/cmp_text.py songs/friends/friends.lrc speech-eval-out/friends-small-text.lrc v
```

(`v` lists every paired line with its difference.)

## Single runs

```sh
# Time a text (or an LRC) like the apps and the server do:
target/release/examples/transcribe MODEL song.wav --lrc text.txt
# Write the singing down:
target/release/examples/transcribe MODEL song.wav --lrc
```

Environment for the `--lrc` form: `TITLE` (song title, for spelled words),
`ALIGN_DIR` (wav2vec2 models, word times by CTC alignment), `VOCALS` (a
16 kHz WAV of the singing alone, for the aligner).

Anchors of a known text, step by step (the recognised segments are cached,
so the matching can be changed and run again in a second):

```sh
cargo run --release -p bl-asr --example align -- MODEL song.wav text.txt segs.cache > anchors.txt
python3 tools/speech-eval/offset.py song.lrc anchors.txt
```

`offset.py` prints the median difference between matched anchors and the
reference's line times: a shifted reference, or a timing that is early or
late throughout.

Other examples:

- `cargo run --release -p bl-sep --example separate -- vocals.pth song.mp3 out.wav --16k`
  separates the singing (Open-Unmix UMX-HQ weights, see `crates/bl-sep`).
- `cargo run --release -p bl-align --example align -- MODEL_DIR file.wav "the text"`
  gives CTC word times (models listed in `crates/bl-align/src/models.rs`).
