//! Timestamped transcription: 30-second windows, greedy decoding with
//! Whisper's timestamp rules, segments with start and end times.

use crate::mel::{log_mel, Mel, HOP, SAMPLE_RATE, WINDOW_FRAMES};
use crate::model::Model;
use crate::net::{encode, Decoder};
use std::collections::HashMap;
use std::io;

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
            m,
            sp,
            index,
            max_token_len,
        }
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

    fn detect_language(&self, dec: &mut Decoder) -> io::Result<u32> {
        dec.reset();
        let logits = dec.step(&[self.sp.sot])?;
        let best = (0..self.sp.n_lang)
            .max_by(|&a, &b| {
                logits[(self.sp.lang0 + a) as usize]
                    .total_cmp(&logits[(self.sp.lang0 + b) as usize])
            })
            .unwrap_or(0);
        Ok(self.sp.lang0 + best)
    }

    /// Transcribes mono audio (`samples` at 16 kHz).
    pub fn transcribe(&self, samples: &[f32], opts: Options) -> io::Result<Vec<Segment>> {
        let p = self.m.params;
        let mel: Mel = log_mel(samples, &self.m.mel_filters, p.n_mels, self.m.mel_bins);
        let content_frames = samples.len() / HOP;
        let prompt_tokens: Vec<u32> = opts
            .prompt
            .map(|t| {
                let tok = self.encode(t);
                // The prompt may use at most half the context.
                let keep = (p.n_text_ctx / 2 - 1).min(tok.len());
                tok[tok.len() - keep..].to_vec()
            })
            .unwrap_or_default();
        let mut lang = opts
            .language
            .and_then(|l| LANGUAGES.iter().position(|c| *c == l))
            .map(|i| self.sp.lang0 + i as u32);
        let mut segments = Vec::new();
        let mut seek = 0usize;
        while seek < content_frames {
            if !(opts.progress)(seek as f32 / content_frames.max(1) as f32) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let window = mel.window(seek);
            let audio = encode(self.m, &window)?;
            let mut dec = Decoder::new(self.m, &audio)?;
            if self.m.multilingual() && lang.is_none() {
                lang = Some(self.detect_language(&mut dec)?);
            }
            let mut init = Vec::new();
            if !prompt_tokens.is_empty() {
                init.push(self.sp.prev);
                init.extend_from_slice(&prompt_tokens);
            }
            init.push(self.sp.sot);
            if let Some(l) = lang {
                init.push(l);
            }
            init.push(self.sp.transcribe);
            dec.reset();
            let (tokens, logprobs, no_speech) = self.decode_window(&mut dec, &init)?;
            let window_secs = WINDOW_FRAMES as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let seek_secs = seek as f64 * HOP as f64 / SAMPLE_RATE as f64;
            let avg = if logprobs.is_empty() {
                0.0
            } else {
                logprobs.iter().sum::<f32>() / logprobs.len() as f32
            };
            // Silence or music without words: skip the window.
            if no_speech > 0.6 && avg < -1.0 {
                seek += WINDOW_FRAMES;
                continue;
            }
            let ts = |t: u32| (t - self.sp.ts_begin) as f64 * 0.02;
            let mut last_ts_end: Option<f64> = None;
            let mut start: Option<f64> = None;
            let mut text_tokens: Vec<u32> = Vec::new();
            let mut text_lp: Vec<f32> = Vec::new();
            for (i, &t) in tokens.iter().enumerate() {
                if t >= self.sp.ts_begin {
                    let time = ts(t);
                    match start {
                        None => start = Some(time),
                        Some(s) => {
                            if !text_tokens.is_empty() {
                                let text = self.decode_text(&text_tokens).trim().to_string();
                                if !text.is_empty() {
                                    segments.push(Segment {
                                        start: seek_secs + s,
                                        end: seek_secs + time.max(s),
                                        text,
                                        avg_logprob: text_lp.iter().sum::<f32>()
                                            / text_lp.len().max(1) as f32,
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
                    text_lp.push(logprobs[i]);
                }
            }
            if !text_tokens.is_empty() {
                // Unterminated last segment: it runs to the window's end.
                let s = start.unwrap_or(0.0);
                let text = self.decode_text(&text_tokens).trim().to_string();
                if !text.is_empty() {
                    segments.push(Segment {
                        start: seek_secs + s,
                        end: seek_secs
                            + window_secs.min(
                                (content_frames - seek) as f64 * HOP as f64 / SAMPLE_RATE as f64,
                            ),
                        text,
                        avg_logprob: text_lp.iter().sum::<f32>() / text_lp.len().max(1) as f32,
                    });
                }
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

    /// Greedy decoding of one window; returns the tokens, their log
    /// probabilities and the no-speech probability.
    fn decode_window(
        &self,
        dec: &mut Decoder,
        init: &[u32],
    ) -> io::Result<(Vec<u32>, Vec<f32>, f32)> {
        let sp = self.sp;
        let n_vocab = self.m.params.n_vocab;
        let mut logits = dec.step(init)?;
        // No-speech probability from the first prediction after <sot>... is
        // read at the position of the <sot> token in Whisper; the logits after
        // the full prompt are a close stand-in.
        let no_speech = log_softmax(&logits)[sp.nosp as usize].exp();
        let mut tokens: Vec<u32> = Vec::new();
        let mut lps = Vec::new();
        let max_len = self.m.params.n_text_ctx / 2;
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
            let best = range
                .clone()
                .max_by(|&a, &b| lp[a].total_cmp(&lp[b]))
                .unwrap_or(sp.eot as usize) as u32;
            if best == sp.eot || !lp[best as usize].is_finite() {
                break;
            }
            tokens.push(best);
            lps.push(lp[best as usize]);
            // Stop on runaway repetition (the same token 6 times).
            if tokens.len() >= 6
                && tokens[tokens.len() - 6..].iter().all(|&t| t == best)
                && best < sp.eot
            {
                break;
            }
            logits = dec.step(&[best])?;
        }
        Ok((tokens, lps, no_speech))
    }
}
