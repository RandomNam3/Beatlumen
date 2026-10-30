//! Timestamped transcription: 30-second windows decoded with Whisper's
//! timestamp rules, segments with start and end times and word times.
//!
//! As in Whisper, a window whose result repeats itself or is unsure is
//! decoded again with growing randomness (temperature), windows without
//! singing are skipped, and the words get their times from the decoder's
//! attention to the audio (`timing`). A known text can be timed directly
//! ([`Transcriber::time_lines`]).

use crate::mel::{log_mel, Mel, HOP, SAMPLE_RATE, WINDOW_FRAMES};
use crate::model::Model;
use crate::net::{encode, Decoder};
pub use crate::timing::Word;
use crate::timing::{
    attention_matrix, dtw_row_starts, frame_energy, refine_starts, word_spans, FRAMES_PER_SEC,
};
use std::collections::HashMap;
use std::io;

/// Temperatures tried in turn while a window's result is not good enough.
const TEMPERATURES: [f32; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
/// A result compressing better than this repeats itself.
const MAX_COMPRESSION: f32 = 2.4;
/// Mean token log probability below which a result is unsure.
const MIN_LOGPROB: f32 = -1.0;
/// Above this no-speech probability (with an unsure result) a window has
/// no singing.
const NO_SPEECH: f32 = 0.6;

/// Special tokens (their ids depend on the vocabulary size).
#[derive(Debug, Clone, Copy)]
pub struct Specials {
    pub eot: u32,
    pub sot: u32,
    pub lang0: u32,
    pub n_lang: u32,
    pub translate: u32,
    pub transcribe: u32,
    pub solm: u32,
    pub prev: u32,
    pub nosp: u32,
    pub notimestamps: u32,
    pub ts_begin: u32,
}

impl Specials {
    pub fn for_model(m: &Model) -> Specials {
        let n = m.params.n_vocab as u32;
        if m.multilingual() {
            let dt = n - 51865;
            Specials {
                eot: 50257,
                sot: 50258,
                lang0: 50259,
                n_lang: 99 + dt,
                translate: 50358 + dt,
                transcribe: 50359 + dt,
                solm: 50360 + dt,
                prev: 50361 + dt,
                nosp: 50362 + dt,
                notimestamps: 50363 + dt,
                ts_begin: 50364 + dt,
            }
        } else {
            Specials {
                eot: 50256,
                sot: 50257,
                lang0: 0,
                n_lang: 0,
                translate: 50357,
                transcribe: 50358,
                solm: 50359,
                prev: 50360,
                nosp: 50361,
                notimestamps: 50362,
                ts_begin: 50363,
            }
        }
    }
}

/// Language codes in the order of the language tokens.
pub const LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];

/// A recognised stretch of speech or singing.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Seconds.
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Mean log probability of its tokens.
    pub avg_logprob: f32,
    /// Its words with their times (empty when not known).
    pub words: Vec<Word>,
}

/// A line of a known text and the time it is sung in (seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct Options<'a> {
    /// Language code ("de"), or None to detect it.
    pub language: Option<&'a str>,
    /// Text the decoder is told came before (e.g. the song text), which
    /// steers spelling and vocabulary.
    pub prompt: Option<&'a str>,
    /// Called with the progress (0..1); false cancels.
    pub progress: &'a mut dyn FnMut(f32) -> bool,
}

pub struct Transcriber<'a> {
    pub m: &'a Model,
    pub sp: Specials,
    index: HashMap<Vec<u8>, u32>,
    max_token_len: usize,
    /// Word times of known texts from another model, when there is one.
    aligner: Option<&'a dyn WordAligner>,
}

/// Word times of a known text from a model made for it (a CTC aligner),
/// used instead of Whisper's attention when it has one for the language.
pub trait WordAligner: Sync {
    /// The words of `text` (split at whitespace, in order, one each) with
    /// times in seconds from the start of `audio` (16 kHz mono), and their
    /// pieces (syllables) when the word has several; `None` when it has no
    /// model for `lang` (ISO 639-1) or the text does not fit the audio.
    /// `at`: where `audio` starts in the song (seconds), for an aligner
    /// that listens to its own track of the song (the singing alone).
    fn align(&self, lang: &str, audio: &[f32], at: f64, text: &str) -> Option<Vec<Word>>;
}

fn log_softmax(x: &[f32]) -> Vec<f32> {
    let max = x.iter().copied().fold(f32::MIN, f32::max);
    let lse = x.iter().map(|v| (v - max).exp()).sum::<f32>().ln() + max;
    x.iter().map(|v| v - lse).collect()
}

impl<'a> Transcriber<'a> {
    pub fn new(m: &'a Model) -> Transcriber<'a> {
        let sp = Specials::for_model(m);
        let mut index = HashMap::new();
        let mut max_token_len = 1;
        for (i, w) in m.vocab.iter().enumerate().take(sp.eot as usize) {
            if !w.is_empty() {
                index.entry(w.clone()).or_insert(i as u32);
                max_token_len = max_token_len.max(w.len());
            }
        }
        Transcriber {
            aligner: None,
            m,
            sp,
            index,
            max_token_len,
        }
    }

    /// Known texts get their word times from `aligner` where it can.
    pub fn with_aligner(mut self, aligner: Option<&'a dyn WordAligner>) -> Transcriber<'a> {
        self.aligner = aligner;
        self
    }

    /// Text to tokens by greedy longest match over the vocabulary (the
    /// model file has no BPE merges; good enough for prompts).
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let b = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let mut found = None;
            for len in (1..=self.max_token_len.min(b.len() - i)).rev() {
                if let Some(&t) = self.index.get(&b[i..i + len]) {
                    found = Some((t, len));
                    break;
                }
            }
            match found {
                Some((t, len)) => {
                    out.push(t);
                    i += len;
                }
                None => i += 1,
            }
        }
        out
    }

    pub fn decode_text(&self, tokens: &[u32]) -> String {
        let mut bytes = Vec::new();
        for &t in tokens {
            if t < self.sp.eot {
                if let Some(w) = self.m.vocab.get(t as usize) {
                    bytes.extend_from_slice(w);
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Transcribes mono audio (`samples` at 16 kHz).
    pub fn transcribe(&self, samples: &[f32], opts: Options) -> io::Result<Vec<Segment>> {
        let p = self.m.params;
        let mel: Mel = log_mel(samples, &self.m.mel_filters, p.n_mels, self.m.mel_bins);
        let content_frames = samples.len() / HOP;
        let max_prompt = p.n_text_ctx / 2 - 1;
        let fixed_prompt: Option<Vec<u32>> = opts.prompt.map(|t| {
            let tok = self.encode(t);
            tok[tok.len().saturating_sub(max_prompt)..].to_vec()
        });
        let mut lang = opts
            .language
            .and_then(|l| LANGUAGES.iter().position(|c| *c == l))
            .map(|i| self.sp.lang0 + i as u32);
        // Without a given prompt, the text of the window before steers the
        // next one (unless it needed much randomness).
        let mut previous: Vec<u32> = Vec::new();
        let mut segments = Vec::new();
        let mut seek = 0usize;
        while seek < content_frames {
            if !(opts.progress)(seek as f32 / content_frames.max(1) as f32) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let window = mel.window(seek);
            let audio = encode(self.m, &window)?;
            let mut dec = Decoder::new(self.m, &audio)?;
            let prompt = fixed_prompt.as_deref().unwrap_or(&previous);
            let mut prefix = Vec::new();
            if !prompt.is_empty() {
                prefix.push(self.sp.prev);
                prefix.extend_from_slice(prompt);
            }
            prefix.push(self.sp.sot);
            let mut best: Option<Decoded> = None;
            for (attempt, &temp) in TEMPERATURES.iter().enumerate() {
                let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (seek as u64) << 8 ^ attempt as u64);
                let d = self.decode_window(&mut dec, &prefix, lang, temp, &mut rng)?;
                let silent = d.no_speech > NO_SPEECH && d.avg_logprob < MIN_LOGPROB;
                let good =
                    !d.looped && d.compression <= MAX_COMPRESSION && d.avg_logprob >= MIN_LOGPROB;
                let done = good || silent;
                // Keep the best try: one without repetition, then the surest.
                let worse =
                    |x: &Decoded| (x.looped || x.compression > MAX_COMPRESSION, -x.avg_logprob);
                let better = !best.as_ref().is_some_and(|b| worse(b) <= worse(&d));
                if better || done {
                    best = Some(d);
                }
                if done {
                    break;
                }
            }
            let mut d = best.expect("at least one try");
            if lang.is_none() && self.m.multilingual() {
                lang = Some(d.lang);
            }
            let window_secs = WINDOW_FRAMES as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let seek_secs = seek as f64 * HOP as f64 / SAMPLE_RATE as f64;
            // Silence or music without words: skip the window.
            if d.no_speech > NO_SPEECH && d.avg_logprob < MIN_LOGPROB {
                previous.clear();
                seek += WINDOW_FRAMES;
                continue;
            }
            // A repetition no try got rid of: twice is enough.
            if d.looped || d.compression > MAX_COMPRESSION {
                self.cut_repetition(&mut d);
            }
            let ts = |t: u32| (t - self.sp.ts_begin) as f64 * 0.02;
            let mut last_ts_end: Option<f64> = None;
            let mut start: Option<f64> = None;
            let mut text_tokens: Vec<u32> = Vec::new();
            let mut text_lp: Vec<f32> = Vec::new();
            // All text tokens of the window and each segment's range of them.
            let mut all_text: Vec<u32> = Vec::new();
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            let first_segment = segments.len();
            let tokens = &d.tokens;
            for (i, &t) in tokens.iter().enumerate() {
                if t >= self.sp.ts_begin {
                    let time = ts(t);
                    match start {
                        None => start = Some(time),
                        Some(s) => {
                            if !text_tokens.is_empty() {
                                let text = self.decode_text(&text_tokens).trim().to_string();
                                if !text.is_empty() {
                                    ranges
                                        .push((all_text.len(), all_text.len() + text_tokens.len()));
                                    all_text.extend_from_slice(&text_tokens);
                                    segments.push(Segment {
                                        start: seek_secs + s,
                                        end: seek_secs + time.max(s),
                                        text,
                                        avg_logprob: text_lp.iter().sum::<f32>()
                                            / text_lp.len().max(1) as f32,
                                        words: Vec::new(),
                                    });
                                }
                                text_tokens.clear();
                                text_lp.clear();
                                last_ts_end = Some(time);
                            }
                            // A pair "<end><start>" closes one segment and opens the next.
                            start = if i + 1 < tokens.len() && tokens[i + 1] >= self.sp.ts_begin {
                                None
                            } else {
                                Some(time)
                            };
                        }
                    }
                } else if t < self.sp.eot {
                    text_tokens.push(t);
                    text_lp.push(d.logprobs[i]);
                }
            }
            let rest = (content_frames - seek) as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let unterminated = !text_tokens.is_empty();
            if unterminated {
                // Unterminated last segment: it runs to the window's end.
                let s = start.unwrap_or(0.0);
                let text = self.decode_text(&text_tokens).trim().to_string();
                if !text.is_empty() {
                    ranges.push((all_text.len(), all_text.len() + text_tokens.len()));
                    all_text.extend_from_slice(&text_tokens);
                    segments.push(Segment {
                        start: seek_secs + s,
                        end: seek_secs + window_secs.min(rest),
                        text,
                        avg_logprob: text_lp.iter().sum::<f32>() / text_lp.len().max(1) as f32,
                        words: Vec::new(),
                    });
                }
            }
            // Word times from the attention to this window's audio.
            if !all_text.is_empty() {
                let frames = ((content_frames - seek).min(WINDOW_FRAMES) / 2).max(1);
                let from = seek * HOP;
                let audio = &samples[from..(from + WINDOW_FRAMES * HOP).min(samples.len())];
                let words = self.word_times(&mut dec, Some(d.lang), &all_text, frames, audio)?;
                for (w, first) in words {
                    let Some(k) = ranges.iter().position(|r| first >= r.0 && first < r.1) else {
                        continue;
                    };
                    let seg = &mut segments[first_segment + k];
                    let (a, b) = (seek_secs + w.start, seek_secs + w.end);
                    // In order, from the segment's start to the window's end
                    // (a segment's last word may run past its end time).
                    let limit = seek_secs + window_secs.min(rest);
                    let floor = seg.words.last().map_or(seg.start, |l: &Word| l.start);
                    let start = a.clamp(floor, limit.max(floor));
                    let end = b.clamp(start, limit.max(start));
                    seg.end = seg.end.max(end);
                    seg.words.push(shifted(w, seek_secs, start, end));
                }
            }
            previous = if d.temperature < 0.5 {
                all_text[all_text.len().saturating_sub(max_prompt)..].to_vec()
            } else {
                Vec::new()
            };
            if unterminated {
                seek += WINDOW_FRAMES;
            } else {
                // Continue at the last complete segment's end (the rest of
                // the window is decoded again with more context).
                let adv = last_ts_end
                    .map(|e| (e * SAMPLE_RATE as f64 / HOP as f64) as usize)
                    .filter(|&f| f > 100)
                    .unwrap_or(WINDOW_FRAMES);
                seek += adv.min(WINDOW_FRAMES);
            }
        }
        (opts.progress)(1.0);
        Ok(segments)
    }

    /// Word times of a known text: `lines` with the times they are sung in
    /// (e.g. from an LRC file or a first recognition), timed word by word
    /// against the audio. Lines are taken together in 30-second windows.
    /// Returns the words of each line (empty for empty lines).
    pub fn time_lines(
        &self,
        samples: &[f32],
        lines: &[Line],
        language: Option<&str>,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> io::Result<Vec<Vec<Word>>> {
        let p = self.m.params;
        let mel: Mel = log_mel(samples, &self.m.mel_filters, p.n_mels, self.m.mel_bins);
        let content_frames = samples.len() / HOP;
        let fps = SAMPLE_RATE as f64 / HOP as f64;
        let window_secs = WINDOW_FRAMES as f64 / fps;
        let mut lang = language
            .and_then(|l| LANGUAGES.iter().position(|c| *c == l))
            .map(|i| self.sp.lang0 + i as u32);
        let mut out: Vec<Vec<Word>> = vec![Vec::new(); lines.len()];
        // Each line's tokens (a leading space starts its first word).
        let tokens: Vec<Vec<u32>> = lines
            .iter()
            .map(|l| {
                let t = l.text.trim();
                // Nothing to time in a line without words ("♪").
                if !t.chars().any(char::is_alphanumeric) {
                    Vec::new()
                } else {
                    self.encode(&format!(" {t}"))
                }
            })
            .collect();
        let todo: Vec<usize> = (0..lines.len())
            .filter(|&i| !tokens[i].is_empty())
            .collect();
        let max_tokens = p.n_text_ctx - 8;
        // Where the words placed so far end: a line whose time is only
        // known roughly is looked for after them.
        let mut after = 0.0f64;
        let mut k = 0;
        while k < todo.len() {
            if !progress(k as f32 / todo.len() as f32) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            // A window from just before the first line, with the lines
            // that end within it.
            let first = &lines[todo[k]];
            let begin = (first.start.max(after.min(first.end - 0.2)) - 0.3).max(0.0);
            let mut group = vec![todo[k]];
            let mut n_tok = tokens[todo[k]].len().min(max_tokens);
            while k + group.len() < todo.len() {
                let next = todo[k + group.len()];
                if lines[next].end - begin > window_secs - 0.5
                    || n_tok + tokens[next].len() > max_tokens
                {
                    break;
                }
                n_tok += tokens[next].len();
                group.push(next);
            }
            let seek = (begin * fps) as usize;
            if seek >= content_frames {
                break;
            }
            let from = seek * HOP;
            let audio = &samples[from..(from + WINDOW_FRAMES * HOP).min(samples.len())];
            // The window's language (from the first window when not given).
            let mut dec = None;
            if lang.is_none() && self.m.multilingual() {
                let window = mel.window(seek);
                let mut d = Decoder::new(self.m, &encode(self.m, &window)?)?;
                d.reset();
                let logits = d.step(&[self.sp.sot])?;
                lang = Some(self.best_language(&logits));
                dec = Some(d);
            }
            let code = lang
                .and_then(|l| LANGUAGES.get((l - self.sp.lang0) as usize))
                .copied()
                .unwrap_or("en");
            // A model made for word timing, when there is one.
            let texts: Vec<&str> = group.iter().map(|&li| lines[li].text.trim()).collect();
            let counts: Vec<usize> = texts.iter().map(|t| t.split_whitespace().count()).collect();
            // Only the stretch of these lines: later in the window other
            // lines are sung, choruses with the same words.
            let until = group.iter().map(|&li| lines[li].end).fold(begin, f64::max) + 0.5;
            let span = ((until - begin).max(0.5) * SAMPLE_RATE as f64) as usize;
            let aligned = self
                .aligner
                .and_then(|a| {
                    a.align(
                        code,
                        &audio[..span.min(audio.len())],
                        from as f64 / SAMPLE_RATE as f64,
                        &texts.join(" "),
                    )
                })
                .filter(|w| w.len() == counts.iter().sum::<usize>());
            if let Some(words) = aligned {
                let mut it = words.into_iter();
                for (&li, &n) in group.iter().zip(&counts) {
                    let l = &lines[li];
                    for w in it.by_ref().take(n) {
                        let words = &mut out[li];
                        let floor = words
                            .last()
                            .map_or(l.start.max(begin) - 0.3, |x: &Word| x.start);
                        let start = (begin + w.start).clamp(floor.max(0.0), l.end.max(l.start));
                        let end = (begin + w.end).clamp(start, l.end.max(start));
                        after = after.max(end);
                        words.push(shifted(w, begin, start, end));
                    }
                }
                k += group.len();
                continue;
            }
            let mut dec = match dec {
                Some(d) => d,
                None => Decoder::new(self.m, &encode(self.m, &mel.window(seek))?)?,
            };
            let mut all: Vec<u32> = Vec::new();
            let mut owner: Vec<usize> = Vec::new();
            for &li in &group {
                let t = &tokens[li][..tokens[li].len().min(max_tokens - all.len())];
                all.extend_from_slice(t);
                owner.extend(std::iter::repeat(li).take(t.len()));
            }
            let frames = ((content_frames - seek).min(WINDOW_FRAMES) / 2).max(1);
            let words = self.word_times(&mut dec, lang, &all, frames, audio)?;
            for (w, first) in words {
                let li = owner[first];
                let l = &lines[li];
                let words = &mut out[li];
                let floor = words
                    .last()
                    .map_or(l.start.max(begin) - 0.3, |x: &Word| x.start);
                let start = (begin + w.start).clamp(floor.max(0.0), l.end.max(l.start));
                let end = (begin + w.end).clamp(start, l.end.max(start));
                after = after.max(end);
                let mut w = shifted(w, begin, start, end);
                // Pieces by syllable, not by the model's tokens.
                w.pieces = crate::timing::syllable_pieces(&w, code);
                words.push(w);
            }
            k += group.len();
        }
        progress(1.0);
        Ok(out)
    }

    /// The words of `text` (tokens said in the window `dec` holds) with
    /// times in seconds from the window's start, and each word's first
    /// token index. `frames`: attention frames with audio (50 per second);
    /// `audio`: the window's samples.
    fn word_times(
        &self,
        dec: &mut Decoder,
        lang: Option<u32>,
        text: &[u32],
        frames: usize,
        audio: &[f32],
    ) -> io::Result<Vec<(Word, usize)>> {
        let mut seq = vec![self.sp.sot];
        if self.m.multilingual() {
            seq.extend(lang);
        }
        seq.push(self.sp.transcribe);
        seq.push(self.sp.notimestamps);
        // The row before each text token predicts it.
        let first_row = seq.len() - 1;
        seq.extend_from_slice(text);
        seq.push(self.sp.eot);
        if seq.len() > self.m.params.n_text_ctx {
            return Ok(Vec::new());
        }
        dec.reset();
        let heads = dec.alignment_heads();
        let (_, weights) = dec.step_with(&seq, &heads)?;
        let n_audio = weights.first().map_or(0, |w| w.len() / seq.len());
        let frames = frames.min(n_audio);
        let rows = text.len() + 1;
        let picked: Vec<Vec<f32>> = weights
            .iter()
            .map(|w| w[first_row * n_audio..(first_row + rows) * n_audio].to_vec())
            .collect();
        let m = attention_matrix(&picked, rows, n_audio, frames);
        let cost: Vec<f32> = m.iter().map(|v| -v).collect();
        let mut starts = dtw_row_starts(&cost, rows, frames);
        let ends = refine_starts(&frame_energy(audio), &mut starts, frames, 10);
        let pieces: Vec<&[u8]> = text
            .iter()
            .map(|&t| self.m.vocab.get(t as usize).map_or(&b""[..], |v| &v[..]))
            .collect();
        let at = |i: usize| starts[i] as f64 / FRAMES_PER_SEC;
        let end_of = |i: usize| ends[i] as f64 / FRAMES_PER_SEC;
        Ok(word_spans(&pieces)
            .into_iter()
            .filter_map(|(first, len)| {
                let word = self.decode_text(&text[first..first + len]);
                let lead = word.len() - word.trim_start().len();
                let trimmed = word.trim().to_string();
                if trimmed.is_empty() {
                    return None;
                }
                // Pieces that are whole characters (a token may hold only
                // part of one), joined they give the word.
                let mut parts: Vec<(f64, String)> = Vec::new();
                let mut bytes: Vec<u8> = Vec::new();
                for i in first..first + len {
                    bytes.extend_from_slice(pieces[i]);
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        let s = s.to_string();
                        bytes.clear();
                        // Punctuation belongs to the piece before it.
                        let mark = s.trim().chars().all(|c| c.is_ascii_punctuation());
                        match parts.last_mut() {
                            Some(last) if mark => last.1.push_str(&s),
                            _ => parts.push((at(i), s)),
                        }
                    }
                }
                // Without the leading space; punctuation stays with its piece.
                if let Some(p) = parts.first_mut() {
                    p.1 = p.1[lead.min(p.1.len())..].to_string();
                }
                parts.retain(|p| !p.1.is_empty());
                if let Some(p) = parts.last_mut() {
                    p.1 = p.1.trim_end().to_string();
                }
                if parts.len() < 2
                    || parts.iter().map(|p| p.1.as_str()).collect::<String>() != trimmed
                {
                    parts.clear();
                }
                Some((
                    Word {
                        text: trimmed,
                        start: at(first),
                        end: end_of(first + len - 1),
                        pieces: parts,
                    },
                    first,
                ))
            })
            .collect())
    }

    fn best_language(&self, logits: &[f32]) -> u32 {
        let best = (0..self.sp.n_lang)
            .max_by(|&a, &b| {
                logits[(self.sp.lang0 + a) as usize]
                    .total_cmp(&logits[(self.sp.lang0 + b) as usize])
            })
            .unwrap_or(0);
        self.sp.lang0 + best
    }

    /// Cuts a repeating end of the window's tokens after two rounds.
    fn cut_repetition(&self, d: &mut Decoded) {
        let text: Vec<usize> = (0..d.tokens.len())
            .filter(|&i| d.tokens[i] < self.sp.eot)
            .collect();
        let only: Vec<u32> = text.iter().map(|&i| d.tokens[i]).collect();
        if let Some((start, period)) = repetition(&only, 3) {
            let keep = start + 2 * period;
            if let Some(&cut) = text.get(keep) {
                d.tokens.truncate(cut);
                d.logprobs.truncate(cut);
            }
        }
    }

    /// Decodes one window (the audio `dec` holds) after `prefix` (prompt
    /// and <sot>): greedy at temperature 0, sampled above.
    fn decode_window(
        &self,
        dec: &mut Decoder,
        prefix: &[u32],
        lang: Option<u32>,
        temperature: f32,
        rng: &mut Rng,
    ) -> io::Result<Decoded> {
        let sp = self.sp;
        let n_vocab = self.m.params.n_vocab;
        dec.reset();
        // Whisper reads the no-speech probability (and the language) from
        // the prediction right after <sot>.
        let first = dec.step(prefix)?;
        let no_speech = log_softmax(&first)[sp.nosp as usize].exp();
        let lang = match lang {
            Some(l) => l,
            None if self.m.multilingual() => self.best_language(&first),
            None => 0,
        };
        let task: Vec<u32> = if self.m.multilingual() {
            vec![lang, sp.transcribe]
        } else {
            vec![sp.transcribe]
        };
        let mut logits = dec.step(&task)?;
        let mut tokens: Vec<u32> = Vec::new();
        let mut lps = Vec::new();
        let mut looped = false;
        // What fits after the prompt (it may take half the context).
        let room = self
            .m
            .params
            .n_text_ctx
            .saturating_sub(prefix.len() + task.len() + 1);
        let max_len = (self.m.params.n_text_ctx / 2).min(room);
        for step in 0..max_len {
            // Suppress special tokens except <eot> and timestamps.
            for t in sp.eot + 1..sp.ts_begin {
                logits[t as usize] = f32::NEG_INFINITY;
            }
            let last_ts = tokens.last().is_some_and(|&t| t >= sp.ts_begin);
            let pen_ts = tokens.len() < 2 || tokens[tokens.len() - 2] >= sp.ts_begin;
            if last_ts {
                if pen_ts {
                    logits[sp.ts_begin as usize..n_vocab].fill(f32::NEG_INFINITY);
                } else {
                    logits[..sp.eot as usize].fill(f32::NEG_INFINITY);
                }
            }
            if step == 0 {
                // Begin with a timestamp of at most 1 s.
                logits[..sp.ts_begin as usize].fill(f32::NEG_INFINITY);
                logits[(sp.ts_begin + 51) as usize..n_vocab].fill(f32::NEG_INFINITY);
            }
            // Timestamps never go back.
            if let Some(&last) = tokens.iter().rev().find(|&&t| t >= sp.ts_begin) {
                let min = if last_ts && !pen_ts { last } else { last + 1 };
                for t in sp.ts_begin..min.min(n_vocab as u32) {
                    logits[t as usize] = f32::NEG_INFINITY;
                }
            }
            let lp = log_softmax(&logits);
            // A timestamp when they are together more likely than any text.
            let ts_mass = lp[sp.ts_begin as usize..]
                .iter()
                .map(|v| v.exp())
                .sum::<f32>()
                .ln();
            let max_text = lp[..sp.ts_begin as usize]
                .iter()
                .copied()
                .fold(f32::MIN, f32::max);
            let range = if ts_mass > max_text {
                sp.ts_begin as usize..n_vocab
            } else {
                0..n_vocab
            };
            let best = if temperature > 0.0 {
                rng.sample(&lp[range.clone()], temperature) + range.start
            } else {
                range
                    .clone()
                    .max_by(|&a, &b| lp[a].total_cmp(&lp[b]))
                    .unwrap_or(sp.eot as usize)
            } as u32;
            if best == sp.eot || !lp[best as usize].is_finite() {
                break;
            }
            tokens.push(best);
            lps.push(lp[best as usize]);
            // Stop on runaway repetition.
            if best < sp.eot {
                let text: Vec<u32> = tokens.iter().copied().filter(|&t| t < sp.eot).collect();
                if repetition(&text, 0).is_some() {
                    looped = true;
                    break;
                }
            }
            logits = dec.step(&[best])?;
        }
        let text_lps: Vec<f32> = tokens
            .iter()
            .zip(&lps)
            .filter(|(t, _)| **t < sp.eot)
            .map(|(_, l)| *l)
            .collect();
        let avg_logprob = if text_lps.is_empty() {
            0.0
        } else {
            text_lps.iter().sum::<f32>() / text_lps.len() as f32
        };
        let text: Vec<u32> = tokens.iter().copied().filter(|&t| t < sp.eot).collect();
        let compression = compression_ratio(self.decode_text(&text).as_bytes());
        Ok(Decoded {
            tokens,
            logprobs: lps,
            no_speech,
            lang,
            avg_logprob,
            compression,
            looped,
            temperature,
        })
    }
}

/// A word of a window moved to the song's time (`offset`), with its start
/// and end limited to `start`..`end` (pieces kept within them, in order).
fn shifted(w: Word, offset: f64, start: f64, end: f64) -> Word {
    let mut floor = start;
    let pieces = w
        .pieces
        .into_iter()
        .map(|(t, s)| {
            let t = (offset + t).clamp(floor, end);
            floor = t;
            (t, s)
        })
        .collect::<Vec<_>>();
    let mut pieces = pieces;
    if let Some(p) = pieces.first_mut() {
        p.0 = start;
    }
    Word {
        text: w.text,
        start,
        end,
        pieces,
    }
}

/// One decoding of a window.
struct Decoded {
    tokens: Vec<u32>,
    logprobs: Vec<f32>,
    no_speech: f32,
    lang: u32,
    avg_logprob: f32,
    compression: f32,
    looped: bool,
    temperature: f32,
}

/// Where the end of `tokens` became a loop: (index where the repeated part
/// starts, its length in tokens). A loop: a piece of up to 16 tokens
/// repeated back to back at least `max(5, 24 / length)` times (fewer with
/// `min_reps` > 0, for cutting a repetition already known).
fn repetition(tokens: &[u32], min_reps: usize) -> Option<(usize, usize)> {
    let n = tokens.len();
    for period in 1..=16usize.min(n / 2) {
        let need = if min_reps > 0 {
            min_reps
        } else {
            5.max(24usize.div_ceil(period))
        };
        if n < period * need {
            continue;
        }
        // Count how many times the last `period` tokens repeat backwards.
        let last = &tokens[n - period..];
        let mut reps = 1;
        while reps * period + period <= n
            && &tokens[n - (reps + 1) * period..n - reps * period] == last
        {
            reps += 1;
        }
        if reps >= need {
            return Some((n - reps * period, period));
        }
    }
    None
}

/// How much smaller the text gets with a simple LZ77 coder (as Whisper's
/// zlib ratio: plain text about 1.2 to 1.8, repetitions far above 2.4).
pub fn compression_ratio(text: &[u8]) -> f32 {
    if text.is_empty() {
        return 1.0;
    }
    const WINDOW: usize = 32 * 1024;
    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 258;
    // Bits as zlib spends them on short texts: a literal about 6.5, a match
    // about 24, and some 11 bytes of its own.
    let mut bits = 0f32;
    let mut heads: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let mut i = 0;
    while i < text.len() {
        let mut best = 0;
        if i + MIN_MATCH <= text.len() {
            let key = [text[i], text[i + 1], text[i + 2]];
            if let Some(cands) = heads.get(&key) {
                for &c in cands.iter().rev().take(32) {
                    if i - c > WINDOW {
                        break;
                    }
                    let mut l = 0;
                    while i + l < text.len() && l < MAX_MATCH && text[c + l] == text[i + l] {
                        l += 1;
                    }
                    best = best.max(l);
                }
            }
        }
        let step = if best >= MIN_MATCH {
            bits += 24.0;
            best
        } else {
            bits += 6.5;
            1
        };
        for j in i..(i + step).min(text.len().saturating_sub(MIN_MATCH - 1)) {
            heads
                .entry([text[j], text[j + 1], text[j + 2]])
                .or_default()
                .push(j);
        }
        i += step;
    }
    let bytes = bits / 8.0 + 11.0;
    text.len() as f32 / bytes
}

/// A small random generator (xorshift*) for sampling.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// An index drawn from log probabilities `lp` at `temperature`.
    fn sample(&mut self, lp: &[f32], temperature: f32) -> usize {
        let max = lp.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let w: Vec<f64> = lp
            .iter()
            .map(|&v| {
                if v.is_finite() {
                    (((v - max) / temperature) as f64).exp()
                } else {
                    0.0
                }
            })
            .collect();
        let total: f64 = w.iter().sum();
        let mut r = self.next() * total;
        for (i, x) in w.iter().enumerate() {
            r -= x;
            if r <= 0.0 && *x > 0.0 {
                return i;
            }
        }
        w.iter().rposition(|x| *x > 0.0).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repetition_is_found() {
        let mut t: Vec<u32> = vec![1, 2, 3, 4];
        for _ in 0..12 {
            t.extend([7, 8]);
        }
        assert_eq!(repetition(&t, 0), Some((4, 2)));
        // A chorus line said twice is no loop.
        let line: Vec<u32> = (10..20).collect();
        let twice = [line.clone(), line].concat();
        assert_eq!(repetition(&twice, 0), None);
        assert_eq!(repetition(&twice, 2), Some((0, 10)));
    }

    #[test]
    fn compression_tells_loops_from_text() {
        let text = b"You say you love me, I say you crazy. We're nothing more than friends. \
            You're not my lover, more like a brother. I know you since we were like ten";
        let r = compression_ratio(text);
        assert!(r > 1.0 && r < 2.0, "{r}");
        let looped = "ooh, ".repeat(100);
        assert!(compression_ratio(looped.as_bytes()) > 10.0);
        // zlib: 2.21 (a chorus said twice is no loop), 4.89 and 27.8.
        let chorus = "Haven't I made it obvious? Haven't I made it clear? ".repeat(2);
        assert!(
            compression_ratio(chorus.as_bytes()) < 2.4,
            "{}",
            compression_ratio(chorus.as_bytes())
        );
        let ready = "I'm ready, I'm ready. ".repeat(6);
        assert!(compression_ratio(ready.as_bytes()) > 3.0);
    }

    #[test]
    fn sampling_follows_the_probabilities() {
        let mut rng = Rng(42);
        let lp = [0.9f32.ln(), 0.1f32.ln(), f32::NEG_INFINITY];
        let n = 2000;
        let firsts = (0..n).filter(|_| rng.sample(&lp, 1.0) == 0).count();
        assert!((1650..1950).contains(&firsts), "{firsts}");
        assert!((0..n).all(|_| rng.sample(&lp, 0.5) != 2));
    }
}
