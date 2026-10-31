# Speech timing: findings

What was learned while making song texts time themselves (`crates/bl-asr`),
from tests with real songs and reports from use. Kept for later work on the
timing and for a possible own model ("Beatlumen Lyrics", see
[ROADMAP.md](ROADMAP.md)).

## How it was measured

- **Songs:**
  - FRIENDS (R3hab Remix): English pop with a long drop, spelled-out words and backing vocals.
  - Who Let The Dogs Out: dense chants and a long repeated outro.
  - Bad Ideas (Alle Farben): English, clean vocals.
  - Zukunft Pink (Peter Fox feat. Inéz): German rap and two voices.
- **Two tasks:**
  - Writing the singing down without a text: word error rate against a
    reference text.
  - Timing a known text: line start error against reference LRC times. The
    measures are the median error and the share of lines within 0.5 s and
    within 1 s.
- **Tools:** in [`tools/speech-eval`](../tools/speech-eval/README.md):
  - `eval.py` runs every model on every song.
  - `cmp_text.py` pairs lines by their text, not by their position, so
    references with another line count can be compared.
  - `offset.py` measures the shift between recognised lines and an LRC.
  - `lrclib_refs.py` fetches reference LRCs for test songs from LRCLIB. It is
    used only for testing; Beatlumen itself contacts no lyrics database.
- **References can be wrong.** An LRC with the same length as the audio can
  belong to another release: another tempo (times drifting up to 18 s), or
  another intro (all lines 3 s early, as with Zukunft Pink). A reference
  only counts after it agrees with a second one or with the recognition.

## Models

The two songs were compared after the timing fixes below. The first figure is
for FRIENDS, the second for Who Let The Dogs Out. "Lines ≤ 1 s" is for
timing a known text.

| Model | Size | Wrong words without a text | Lines ≤ 1 s | Kept |
|---|---|---|---|---|
| tiny | 78 MB | 77 % / 80 % | 77 % / 41 % | no, worst everywhere |
| base-q5_1 | 60 MB | 41 % / 75 % | 83 % / 45 % | yes, small and fast |
| base | 148 MB | 43 % / 74 % | 85 % / 47 % | yes |
| small-q5_1 | 190 MB | 52 % / 73 % | 96 % / 38 % | no, worse than small and base |
| small | 488 MB | 29 % / 70 % | 92 % / 45 % | yes, recommended |
| medium-q5_0 | 539 MB | 35 % / 60 % | 87 % / 60 % | no, as slow as turbo but worse |
| large-v3-turbo-q5_0 | 574 MB | 23 % / 59 % | 87 % / 70 % | yes, best |

- **Quantisation hurts more than its size suggests.** small-q5_1 was worse
  than small at writing down, and on dense music worse than base.
- **Speed:** a 2.5-minute song on 4 CPU cores took about 1 min with base and
  about 6 min with large v3 turbo. The GPU encoder helps the big models most.

## What went wrong, and what helped

### Giving the model text as context (prompt)

- **The song text as prompt:** the model copied the text instead of
  listening. It skipped a whole verse and made words up elsewhere (log
  probability -4.2 for that window). Since then there is no prompt when
  timing a known text. Result: 79 % instead of 55 % of the words matched.
- **A neutral style prompt** ("Hello, how are you? I'm fine. Let's go!")
  meant to get punctuation and capitals: parts of it appeared in the
  transcript ("Let's go!", "I'm fine."). Do not prompt with any text that
  could be sung.

### Long segments without timestamps

- Whisper places word times inside a segment by its cross-attention.
  - **What happens:** when a segment runs on over a long instrumental part
    (up to the 30-second window's end), the word times break down. One word
    stretches over 23 s, and the words after it are squeezed into nothing at
    the segment's end. The FRIENDS chorus at 53 s was placed at 72–79 s
    this way.
  - **A fix that was dropped:** marking such words as unreliable for
    anchoring gave mixed results (better on one song, worse on another), so
    it was not kept.
  - **What a real fix needs:** a better aligner, or a model that ends
    segments at line ends.
- **large v3 turbo writes whole verses in one segment:** in lower case,
  without punctuation and without timestamps, even though the standard rule
  (take a timestamp when timestamps together are more likely than any text
  token) is applied.
  - **Tried and dropped:** favouring a timestamp after 12 text tokens without
    one, where the most likely next token starts a word. Segments went from
    15–30 s to 4–8 s and some songs got better, but on dense chants (Who Let
    The Dogs Out) it threw the decoding off for the rest of the song: large
    v3 turbo's median line error went from 0.31 s to 7.5 s, base's from
    1.5 s to 9.2 s. With 24 tokens it no longer helped turbo. Any change to
    the decoding has to be checked on such songs.
- **Lines without punctuation or capitals:**
  - Lines over 48 characters are split at the longest pause, then at
    punctuation, then near the middle.
  - Lines get a capital at the start, and "i" becomes "I".
  - **Weakness:** dense music has almost no pauses between words, so the
    breaks still fall mid-sentence sometimes. A model that marks line ends
    would solve this.

### Matching a known text to the recognition

- **Repeated lines (choruses) make plain word matching unreliable.**
  - A single common word ("the", "who") matched in another repetition can
    pin a line to the wrong chorus.
  - **Fix:** a line is anchored only by two matched words in a row, and the
    sequence alignment rewards runs of matches. The median line error on
    FRIENDS went from 0.69 s to 0.20 s.
- **Backing vocals in parentheses are mostly not recognised.** If they take
  part in the matching, their repetitions of the lead's words take the
  lead's times. They are now left out of the matching.
- **Lines that were not heard** (spelled-out words, chants) are no longer
  given estimated times. The word timing searches for them between the
  anchored lines around them, after the words placed before them.
  - Before this fix, every such line was searched from the same starting
    point, so they all piled up there.
  - Who Let The Dogs Out: median line error from 6.2 s to 2.0 s.

### Existing LRC files

- **An LRC made for another release:** its line times were trusted as fixed
  windows. With all lines 3 s early, every word was squeezed to its line's
  end (60 of 69 lines).
  - **Fix:** three 30-second stretches are recognised, and their lines
    matched to the LRC lines timed nearby. The times are shifted by the
    median difference when it is at least 1 s and consistent (quartiles
    within 1 s).
  - **What a correct LRC looks like:** line times come 0.2–0.4 s before the
    recognised first word, so that lead is allowed for.
  - **Result:** 2 of 69 lines squeezed instead of 60.

### Recognition errors that need a better model

- **Spelled-out words get letters dropped:** "F-R-I-N-D-S", "f-r-i-n-e-s".
  - **Fix:** a spelled word that is a near miss of a word of the song's
    title is written like the title. It must be the same length or shorter,
    one letter off, or two for words of 7 or more letters.
  - Other spelled words are left as heard. A longer word, such as
    "L-O-V-E-D" for the title "Love", is another word.
- **Singing taken for other words:** "Every end is" for "F-R-I-E-N-D-S",
  "you" for "ooh", "Heaven, I made it" for "Haven't I made it", "Have them"
  for "Haven't we".
- **Hallucinations at the ends of music:** "Thank you.", "(upbeat music)" for
  a long chant, and "*Bad R&B Music*". Text in `[…]`, `(…)` and `*…*` is
  dropped as a sound annotation.
- **Voices:**
  - Two singers (Peter Fox and Inéz) are not told apart when the text does
    not name them.
  - Clustering the Whisper encoder's features or MFCCs found song sections,
    not voices.
  - Telling voices apart needs a speaker-embedding model.

## The LRCGen pipeline, measured

LRCGen's steps were built natively and measured on the test songs:

- **Vocal separation (Open-Unmix UMX-HQ, `bl-sep`):** separates a song in
  about 6 s and matches the original to 133 dB. Whisper on the separated
  singing wrote down **worse** (FRIENDS: 55 % instead of 45 % wrong words
  with base-q5_1, 37 % instead of 22 % with small); its line timing was
  about the same. The separation leaves artefacts that Whisper, trained on
  recordings with music and noise, handles worse than the music itself. Not
  used for recognition. A stronger separation model (Demucs-class) might
  change this; its weights' licence is unclear.
- **CTC word alignment (wav2vec2, `bl-align`):** frame-exact output and
  character times, matching transformers and torchaudio. On the songs:
  - word starts came 0.2–0.35 s later than both reference LRCs;
  - given a 30-second window, it placed repeated chorus words in the wrong
    repetition. Fixed by giving it only the stretch of its lines;
  - its line timing stayed worse than Whisper's attention with the energy
    refinement (FRIENDS with base-q5_1: 83 % of lines within 1 s instead of
    96 %; on the separated singing 83 %).
  It is wired in (`WordAligner`) but not used by default. Worth trying again
  with an onset correction and with a model trained on singing.
- **Syllables (`bl-hyph`):** used. Long words are filled syllable by
  syllable ("re-ally", "with-out", "po-lite"), timed by where their letters
  fall in the model's pieces.
- **Tap sync and line editor:** in the app. Tapping lags the singing by
  about 0.1 s, which is taken off each tapped time.

## Ideas from other projects

LRCGen (github.com/final-wav/LRCGen, GPL-3.0, Python) solves the same task
with Whisper and other libraries. Its ideas, not its code:

- **Vocal isolation before recognition:** UVR5 / MDX-Net / Demucs through
  audio-separator. It reports clearly better recognition on loud music. This
  matches the errors above, which mostly come from dense music.
- **Forced alignment with a CTC acoustic model:** wav2vec2 through WhisperX,
  run inside known line windows. It gives tighter word times than Whisper's
  attention, because a CTC model is trained for frame-exact output and has
  no long-segment collapse.
- **Syllables:**
  - Words are split into syllables with hyphenation dictionaries (pyphen,
    per language).
  - Syllable onsets come from the vocals' loudness (RMS) envelope.
  - Here, pieces come from the model's tokens instead; hyphenation patterns
    would give real syllables.
- **Ad-libs and backing vocals** on their own lane, and TTML output
  (word-synced, with background vocals).
- **Manual tools:** tap sync (Space for each line while the song plays,
  Backspace to go back) on a vocals-only waveform, and a timeline editor
  (drag lines, trim them, undo). Worth having for the songs no model gets
  right.
- **Note:** LRCGen keeps an LRC's line times as they are, so it would have
  the same problem with LRCs for another release.

## For an own model

What the findings above ask of a model trained for songs:

- **Training data:**
  - Dense music (drops, chants, choruses over loud beats).
  - Spelled-out words, ad-libs and backing vocals.
  - Several languages, German included.
  - Long instrumental parts, labelled as silence, never as text.
  - Only material that may be used (see the roadmap).
- **Output conventions** to train for:
  - A timestamp at every sung line's start and end, so segments are lines.
  - Capitals and punctuation as in lyrics.
  - Backing vocals in parentheses.
  - No sound annotations.
- **Alignment:** a CTC head, or a separate small CTC aligner, for word and
  syllable times, instead of cross-attention.
- **Robustness:**
  - Train on mixtures at several vocal-to-music levels, plus vocals-only
    stems (for use after vocal isolation).
  - Add the hallucination cases as negative examples: silence and
    instrumental endings with no text.
- **Evaluation:**
  - Keep the tasks above (writing down, timing a known text) and their
    metrics.
  - Test songs the model never saw.
  - Check every reference against the audio first.
- **Prompts:** do not rely on them. Titles are the only safe hint, and they
  are better used after recognition, as with the spelled words.
