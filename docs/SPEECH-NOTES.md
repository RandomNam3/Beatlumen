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
- **Tools:** written for the tests and kept outside the repository:
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
- **GPU check (Radeon RX 9070 XT, RADV):** all models and songs gave the
  same results as on the CPU (one song's word error rate 0.1 points apart)
  and ran 3 to 10 times faster: FRIENDS took 40 s instead of 406 s with
  large v3 turbo, 30 s instead of 181 s with small.

## Goal for the next models

Lines within 0.5 s, not 1 s: at least 90 % of the lines on every test song
(see [PLAN-0.12.md](PLAN-0.12.md)). The share within 0.5 s is the main
measure from now on. The stock models reach 37–77 % on the test songs
today, so this needs a model trained on singing and better word times
(the CTC aligner below).

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
  used for recognition. A stronger separation model (HT-Demucs) did not
  change this either (see the CTC head below).
- **CTC word alignment (wav2vec2, `bl-align`):** frame-exact output and
  character times, matching transformers and torchaudio. On the songs:
  - word starts came 0.2–0.35 s later than both reference LRCs;
  - given a 30-second window, it placed repeated chorus words in the wrong
    repetition. Fixed by giving it only the stretch of its lines;
  - its line timing stayed worse than Whisper's attention with the energy
    refinement (FRIENDS with base-q5_1: 83 % of lines within 1 s instead of
    96 %; on the separated singing 83 %).
  It is wired in (`WordAligner`) but not used by default. Trained on
  singing it got much better, and a small head on the speech model's own
  encoder better still (below).
- **Syllables (`bl-hyph`):** used. Long words are filled syllable by
  syllable ("re-ally", "with-out", "po-lite"), timed by where their letters
  fall in the model's pieces.
- **Tap sync and line editor:** in the app. Tapping lags the singing by
  about 0.1 s, which is taken off each tapped time.

## Timing a known text with a CTC head

Measured in November 2026 with own models trained on singing (Whisper small
and base, kept private), each with a head made for it.

- **Reference LRCs start early.** Line times from lyrics sites come about
  0.13–0.2 s before the singing, on some songs more, and the times of
  Whisper's attention share that lead. Compared with such references, exact
  timing looks worse. On separated vocals (HT-Demucs, 30 test songs) the
  level rises by 0.8 dB at reference line starts, 0.6 dB at attention starts
  and 6.7 dB at the head's starts.
- **So the main measure is now JamendoLyrics MultiLang:** 79 songs in
  English, German, French and Spanish with word times annotated by hand,
  used for measuring only. LRC references are still checked (30 test songs,
  100 more songs), read with their lead in mind.
- **The head** (`bl-align`, `head.rs`): a layer norm, two convolutions
  (kernel 5, 256 channels) and one output per language (en, de, es, fr, pl,
  pt, it, nl, fi, and one for all others) to the letters a–z and the
  apostrophe, trained with CTC on the frozen encoder of the speech model.
  About 2.8 MB in f16. It listens with the encoder that runs anyway, so a
  known text needs no recognition: the encoder over the whole song in
  30-second windows (the middle 25 s of each kept), then the best path of
  the whole text through the frames.
- **Lines within 0.5 s on JamendoLyrics (mean per song):**

  | Way | All | de | en | es | fr |
  |---|---|---|---|---|---|
  | Recognition, then Whisper's attention (small trained on singing) | 0.829 | 0.802 | 0.827 | 0.853 | 0.832 |
  | wav2vec2 trained on singing, after recognition | | 0.917 | 0.904 | | |
  | Head on small, whole song, no recognition | **0.933** | 0.976 | 0.895 | 0.960 | 0.899 |
  | Head on base-q5_1 | 0.928 | 0.955 | 0.896 | 0.963 | 0.896 |

  60 of the 79 songs reach 90 % with the head on small, 23 with the
  attention (which could not time 3 songs at all, the head on base 1).
  Word starts: median error 0.034 s, against 0.098 s for the attention. On
  56 songs with LRC references the head on small has 0.892 instead of 0.879
  (German 0.922 instead of 0.880, English 0.865 instead of 0.878).
- **Speed** (median seconds per song, five songs of 2.5 to 5 minutes):

  | Way | 4 cores | 6 cores | GPU |
  |---|---|---|---|
  | small, recognition and attention | 43.2 | 34.4 | 23.5 |
  | small, head | 17.6 | 13.3 | 7.7 |
  | base-q5_1, recognition and attention | 19.2 | 17.3 | |
  | base-q5_1, head | 5.6 | 4.5 | 2.8 |

  GPU and CPU give the same files with the head. Songs that fall back to
  recognition can differ slightly, as before.
- **Texts that do not fit:** the share of the frames heard as letters that
  the path spends outside the text's letters. Fitting texts reached at
  most 0.08 (small) and 0.15 (base), texts with missing lines were mostly above
  0.1, another song's text 0.14 and more (median 0.35). Above 0.1 the song is
  recognised and timed as before; if the recognition does not find the
  text either, the head's result is taken up to 0.2. Without repeats or with
  15 % of the lines left out, the head alone fell from 0.923 to about 0.83.
- **Stray first letters:** where the head hears little, the path can put a
  line's first letter or two at any cheap frame long before the line is
  sung. A start of at most three letters before a gap of more than 2 s now
  moves to the rest: 0.933 instead of 0.923. Splitting lines only between
  words helped less (0.926), because the stray letter is usually the start
  of the first word.
- **Songs without a text:** the singing is written down as before, and the
  head gives the word times of each written-down line instead of Whisper's
  attention. On JamendoLyrics, counting the lines whose first word was
  recognised (68 % with small, 52 % with base-q5_1) and the recognised words
  against their hand-annotated times:

  | Model | Lines within 0.5 s | Songs ≥ 90 % | Words within 0.3 s | Word median |
  |---|---|---|---|---|
  | small, attention | 0.844 | 32 | 0.776 | 0.102 s |
  | small, head | **0.936** | 60 | 0.926 | 0.035 s |
  | base-q5_1, attention | 0.662 | 9 | 0.686 | 0.179 s |
  | base-q5_1, head | **0.897** | 50 | 0.877 | 0.034 s |

  The words themselves stay the same (word error rate 0.40 with small, 0.60
  with base-q5_1): what is still missing there is recognition, not timing.
  The head costs little time here (median seconds per song, five songs of
  2.5 to 4.5 minutes): small 51.2 instead of 49.9 on 4 cores and 41.4
  instead of 41.2 on 6, base-q5_1 18.8 instead of 18.0 and 15.2 instead of
  14.2. GPU and CPU give the same files, except where the recognition itself
  differs (one song with base-q5_1).
- **Tried and dropped:**
  - A head with 512 channels: worse in every language (0.904).
  - Timing a written-down text as a whole like a given one: 0.927 of the
    lines within 0.5 s instead of 0.936, words within 0.3 s 0.929 instead of
    0.926.
  - A state for singing outside the text between lines: no change.
  - Vocalisations ("oh ah ah", "aw ah") are the weakest lines (31 % within
    0.5 s): the head does not hear them as letters, so the path squeezes
    them next to a neighbour, up to 30 s off. Leaving them out of the path
    and spreading them over their gap helped the hand-timed songs a little
    (0.937) and the LRC references not (0.861 instead of 0.866). They are
    1–2 % of the lines.
- **Separation and beats, measured again:** recognition on HT-Demucs vocals
  timed 0.881–0.888 of the lines within 0.5 s against 0.895 on the mix (LRC
  references); Open-Unmix 0.865–0.880. The head on separated vocals: 0.878
  instead of 0.923 on JamendoLyrics. Snapping line starts to the beat grid
  was worse as well. Neither is used.
- **Still open:** English is the weakest language, although it has the most
  training data; the base model's head is clearly weaker on English songs
  with LRC references (0.817). The letters are a–z only, so texts in other
  scripts stay with the attention.

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
  - Only material that may be used: for a model that is passed on, music
    and texts whose licence allows it; a model kept on the user's own
    server may use the user's own collection (see the roadmap).
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
