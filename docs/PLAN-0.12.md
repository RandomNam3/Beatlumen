# Plan: own speech models, voices and karaoke

The song text items of [`ROADMAP.md`](ROADMAP.md) after 0.11.1, in the order
they are built. Each phase ends with tests, a measurement on the test songs
where it touches the timing (see [`SPEECH-NOTES.md`](SPEECH-NOTES.md)), a
pull request into `main` and an update of this file.

Principles stay as before: own implementations instead of external
libraries, third-party weights in `THIRD_PARTY_NOTICES.md`, nothing listens
or connects without the user turning it on, everything works in the apps
without a server, German and English texts. Beatlumen contacts no lyrics
database.

Legend: `[ ]` open, `[~]` in progress, `[x]` done.

## Goals

Measured on songs a model never saw:

- Timing a known text: at least 90 % of the lines within 0.5 s and 97 %
  within 1 s on every test song; median word error at most 0.15 s.
- Writing down without a text: clearly fewer wrong words, under 40 % on
  dense songs; no test song clearly worse.
- Plain speech (FLEURS samples in German, English, French, Spanish,
  Italian, Polish, Japanese, Chinese): word error rate at most 2 points
  worse than the stock model.
- Voices: at least 85 % of the lines of duets given the right voice.
- Karaoke melody: at least 85 % of the sung frames within a semitone of
  hand-checked notes; microphone latency compensated to ±30 ms.

## Phase 1: own models (outside the repository)

Training happens with separate PyTorch tools on the user's own GPU; only
the results come into Beatlumen.

- [ ] Data: songs with checked, timed texts. A reference only counts when
  the current timing agrees with it (offset and drift checked); per line
  only lines anchored by the recognition and within 0.3 s of the
  reference. Part corrected by hand with the line editor. Instrumental
  parts and endings as examples with no text. Test songs, and all songs of
  their artists, never in training.
- [ ] Whisper `small` fine-tuned (lines as segments, timestamps every
  0.02 s, language token, no prompt), then `large-v3-turbo` (LoRA merged
  before export) and `base` for weak phones. Augmentation: vocals and
  music remixed at several levels, tempo, reverb, compression, band
  limits, vocals alone. Plain speech mixed in so the model keeps its
  languages.
- [ ] CTC aligner (wav2vec2 XLSR class) fine-tuned on singing, with an
  onset correction (the stock models are 0.2–0.35 s late on songs).
- [ ] Export: ggml f16 for `bl-asr` (same tensor names as the whisper.cpp
  files; quantisation cost more quality than it saved), safetensors for
  `bl-align`. Checked against the training framework's output for the
  same window.

## Phase 2: own model sources

- [ ] `bl-asr` and `bl-align` catalogs: a source per entry instead of one
  base URL.
- [ ] Server: lists the models in its model folder (name, size, SHA-256)
  and serves them to logged-in apps; admin page for own models.
- [ ] Apps: models from the connected server next to the public catalog,
  downloaded and checked like the others.
- [ ] Model size picked by device: memory and the GPU check (`bl-gpu`).
- [ ] The CTC aligner used for word times by default when it measures
  better (`WordAligner` in `crates/bl-asr/src/transcribe.rs`).

## Phase 3: melody and notes (`crates/bl-pitch`)

- [ ] Pitch of the separated singing: own pYIN (YIN candidates, voicing
  probability, Viterbi over pitch states), fast enough for phones. A small
  pitch network only if pYIN is not good enough.
- [ ] Live pitch for microphones (YIN, 30–50 ms window, octave guard).
- [ ] Notes: one per syllable (syllable times from `bl-asr`/`bl-hyph`),
  extra notes for melismas, optional beat grid from `bl-analysis`.
- [ ] Karaoke track (lines, syllables, notes, voices, key); UltraStar
  text import and export (duets as voices).
- [ ] Stored like song texts: `GET/PUT/DELETE /api/tracks/<id>/karaoke`
  on the server, in the offline container on the device; computed in the
  server's background jobs or on the device.

## Phase 4: microphones

- [ ] Microphone input in the apps: several sources on Linux
  (PulseAudio/PipeWire, loaded at run time like the system audio capture)
  and Windows (WASAPI), the microphone on Android (permission asked when
  a microphone is first opened). Samples pushed with a timestamp per
  stream; level meter.
- [ ] Phones as wireless microphones: Opus (`bl-opus-enc`) with
  timestamps on the common server time, through the server to the playing
  device.
- [ ] Latency calibration per microphone.
- [ ] `SECURITY.md`: microphone access, only while the karaoke mode uses
  it, recordings neither stored nor sent anywhere else.

## Phase 5: karaoke mode

- [ ] Points for pitch (per note, octave ignored, tolerance by difficulty)
  and text (CTC score of the sung line against the known line).
- [ ] Page in the app: song, microphones per singer or voice, text with
  note bars and live pitch, results; the singing turned down with
  `bl-sep` on request.

## Phase 6: voices

- [ ] Speaker embeddings (ECAPA-TDNN class, own implementation, weights
  with an open licence) on the separated singing, per timed line,
  clustered into voices; names from the text win.
- [ ] Line editor: set and name the voice of a line.

## Phase 7: languages

- [ ] Japanese and Chinese: matching by characters, morae as syllables,
  CTC models per language.
- [ ] Hyphenation patterns for more European languages.
