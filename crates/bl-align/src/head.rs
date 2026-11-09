//! Word times from a small CTC head on the encoder of a Whisper model: two
//! convolutions over the encoder's frames (20 ms each) and a layer per
//! language to the letters, a few megabytes where a wav2vec2 model takes more
//! than a gigabyte per language. It listens with what the speech model
//! computes anyway, so a known text needs no recognition: the encoder over
//! the song and the best path of the whole text through its frames.
//!
//! The file is a safetensors file made for one speech model: `norm`, `a` and
//! `b` (weight and bias), and `out.<language>` with `out.any` for the
//! languages without their own layer. The letters are a to z and the
//! apostrophe; others take their plain form (`text.rs`), so a text in another
//! script is left to Whisper's attention.

use crate::ctc::{token_spans, LogProbs, Span};
use crate::set::{syllabled, MIN_SCORE};
use crate::text::{Target, Vocab};
use crate::wav2vec2::{conv_matrix, gelu_exact};
use crate::weights::SafeTensors;
use crate::{times_of_spans, word_times, Keep, WordTime};
use bl_asr::mel::{log_mel, SAMPLE_RATE};
use bl_asr::model::{Model, Tensor};
use bl_asr::net::encode;
use bl_asr::ops::{layer_norm, linear};
use bl_asr::timing::Word;
use bl_asr::transcribe::{Transcriber, WholeSong, WordAligner};
use std::collections::HashMap;
use std::path::Path;

const LETTERS: &str = "'abcdefghijklmnopqrstuvwxyz";
const KERNEL: usize = 5;
/// The frames a frame's result depends on, each way (two convolutions).
const REACH: usize = 4;
/// Seconds per encoder frame.
const FRAME: f64 = 0.02;
/// The song in windows of 30 s this far apart; of each the middle counts.
const HOP_SECS: usize = 25;
/// Kept out of a window at each end (seconds), where the encoder hears less.
const EDGE_SECS: f64 = 2.5;
/// Most cells of the path through a song (one byte each, 1 GB).
const MAX_CELLS: usize = 1 << 30;
/// A line's first letters this far (seconds) before the rest are stray…
const STRAY_GAP: f64 = 2.0;
/// …when there are at most this many of them.
const STRAY_LETTERS: usize = 3;
/// At least this share of the words must be spelled in the head's letters.
const MIN_LATIN: f32 = 0.6;

struct Layer {
    w: Tensor,
    b: Tensor,
}

/// Widest encoder and hidden layer a head may have (Whisper large is 1280
/// wide).
const MAX_WIDTH: usize = 4096;

/// The weights of a head.
pub struct Head {
    /// Width of the encoder frames it reads (the model's state).
    dim: usize,
    hidden: usize,
    norm: Layer,
    a: Layer,
    b: Layer,
    out: HashMap<String, Layer>,
    vocab: Vocab,
}

fn window_rows(x: &[f32], inputs: usize, n: usize, w: &Layer) -> Vec<f32> {
    // Zeros beyond the frames, as a padded convolution.
    let mut pad = vec![0f32; KERNEL / 2 * inputs];
    pad.extend_from_slice(x);
    pad.resize(x.len() + (KERNEL - 1) * inputs, 0.0);
    let width = KERNEL * inputs;
    let mut out = Vec::with_capacity(n * w.w.rows);
    let mut rows = Vec::new();
    for t0 in (0..n).step_by(128) {
        let t1 = (t0 + 128).min(n);
        rows.clear();
        for t in t0..t1 {
            rows.extend_from_slice(&pad[t * inputs..][..width]);
        }
        out.extend(linear(&rows, t1 - t0, &w.w, Some(&w.b)));
    }
    out
}

impl Head {
    pub fn load(path: impl AsRef<Path>) -> Result<Head, String> {
        let f = SafeTensors::open(path)?;
        let shape = |n: &str| f.shape(n).ok_or(format!("head: no tensor {n}"));
        // Widths from the vectors; every tensor must then have the shape
        // they give it, before anything is read or allocated from them.
        let width = |n: &str| match shape(n)? {
            &[w] if (1..=MAX_WIDTH).contains(&w) => Ok(w),
            _ => Err(format!("head: bad shape of {n}")),
        };
        let dim = width("norm.weight")?;
        let hidden = width("b.bias")?;
        let vocab = Vocab::from_chars(LETTERS, Some('|'));
        let letters = vocab.texts.len();
        let langs: Vec<String> = f
            .names()
            .filter_map(|n| n.strip_prefix("out.")?.strip_suffix(".weight"))
            .map(str::to_string)
            .collect();
        let mut shapes: Vec<(String, Vec<usize>)> = vec![
            ("norm.bias".into(), vec![dim]),
            ("a.weight".into(), vec![hidden, dim, KERNEL]),
            ("a.bias".into(), vec![hidden]),
            ("b.weight".into(), vec![hidden, hidden, KERNEL]),
        ];
        for l in &langs {
            shapes.push((format!("out.{l}.weight"), vec![letters, hidden]));
            shapes.push((format!("out.{l}.bias"), vec![letters]));
        }
        for (n, want) in &shapes {
            if shape(n)? != want.as_slice() {
                return Err(format!("head: bad shape of {n}"));
            }
        }
        let mut names = vec!["norm".to_string(), "a".to_string(), "b".to_string()];
        names.extend(langs.iter().map(|l| format!("out.{l}")));
        let wanted: Vec<(String, Keep)> = names
            .iter()
            .flat_map(|n| [format!("{n}.weight"), format!("{n}.bias")])
            .map(|n| (n, Keep::F32))
            .collect();
        let refs: Vec<(&str, Keep)> = wanted.iter().map(|(n, k)| (n.as_str(), *k)).collect();
        let mut t = f.load(&refs)?;
        let mut take = |n: &str| -> Result<Layer, String> {
            let w = t.remove(&format!("{n}.weight")).ok_or("head: weight")?;
            let b = t.remove(&format!("{n}.bias")).ok_or("head: bias")?;
            Ok(Layer { w, b })
        };
        let conv = |l: Layer, inputs: usize, out: usize| Layer {
            w: conv_matrix(l.w.values(), out, inputs, KERNEL, Keep::F32),
            b: l.b,
        };
        let norm = take("norm")?;
        let a = conv(take("a")?, dim, hidden);
        let b = conv(take("b")?, hidden, hidden);
        let out = langs
            .into_iter()
            .map(|l| {
                let layer = take(&format!("out.{l}"))?;
                Ok((l, layer))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        if !out.contains_key("any") {
            return Err("head: no layer for any language".into());
        }
        Ok(Head {
            dim,
            hidden,
            norm,
            a,
            b,
            out,
            vocab,
        })
    }

    /// The head made for the speech model at `model`, when there is one next
    /// to it: `<model>.head`, for a quantised model also that of the model it
    /// was made from (`ggml-x-q8_0.bin` takes `ggml-x.head`).
    pub fn for_model(model: &Path) -> Option<Head> {
        let stem = model.file_stem()?.to_str()?;
        let plain = stem.rsplit_once("-q").map_or(stem, |(s, _)| s);
        [stem, plain]
            .iter()
            .find_map(|s| Head::load(model.with_file_name(format!("{s}.head"))).ok())
    }

    /// The hidden frames of up to 30 s of `samples` (`n` rows of `hidden`).
    fn features(&self, m: &Model, samples: &[f32]) -> std::io::Result<(Vec<f32>, usize)> {
        let p = m.params;
        let dim = p.n_audio_state;
        if dim != self.dim {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "head made for a model of another width",
            ));
        }
        let mel = log_mel(samples, &m.mel_filters, p.n_mels, m.mel_bins).window(0);
        let mut enc = encode(m, &mel)?;
        let all = enc.len() / dim;
        let n = (((samples.len() as f64 / SAMPLE_RATE as f64) / FRAME) as usize).clamp(1, all);
        let used = (n + REACH).min(all);
        enc.truncate(used * dim);
        layer_norm(&mut enc, dim, &self.norm.w, &self.norm.b);
        let mut x = window_rows(&enc, dim, used, &self.a);
        gelu_exact(&mut x);
        let mut x = window_rows(&x, self.hidden, used, &self.b);
        gelu_exact(&mut x);
        x.truncate(n * self.hidden);
        Ok((x, n))
    }

    /// As [`features`](Self::features), with the encoder run on only the
    /// frames the audio needs instead of 30 s: a few seconds cost a fraction,
    /// cheap enough to score sung lines while a song plays.
    fn features_short(&self, m: &Model, samples: &[f32]) -> std::io::Result<(Vec<f32>, usize)> {
        let p = m.params;
        let dim = p.n_audio_state;
        let mel = log_mel(samples, &m.mel_filters, p.n_mels, m.mel_bins);
        // Two mel frames to an encoder frame, and room for the convolutions.
        let need = (samples.len() / bl_asr::mel::HOP + 4 * (REACH + 2))
            .next_multiple_of(2)
            .min(bl_asr::mel::WINDOW_FRAMES)
            .min(mel.frames);
        let mut win = vec![0f32; p.n_mels * need];
        for c in 0..p.n_mels {
            win[c * need..(c + 1) * need]
                .copy_from_slice(&mel.data[c * mel.frames..c * mel.frames + need]);
        }
        let mut enc = encode(m, &win)?;
        let all = enc.len() / dim;
        let n = (((samples.len() as f64 / SAMPLE_RATE as f64) / FRAME) as usize).clamp(1, all);
        let used = (n + REACH).min(all);
        enc.truncate(used * dim);
        layer_norm(&mut enc, dim, &self.norm.w, &self.norm.b);
        let mut x = window_rows(&enc, dim, used, &self.a);
        gelu_exact(&mut x);
        let mut x = window_rows(&x, self.hidden, used, &self.b);
        gelu_exact(&mut x);
        x.truncate(n * self.hidden);
        Ok((x, n))
    }

    /// How well a sung line fits its text: the mean probability the head
    /// gives each of the text's letters on their best path through the
    /// audio (16 kHz mono, the line with a little room around it, at most
    /// 30 s), 0..1. Singing the words gives high values, other words or
    /// humming low ones. `None` when the text is not spelled in the head's
    /// letters or the audio is too short for it.
    pub fn sung_score(&self, m: &Model, samples: &[f32], lang: &str, text: &str) -> Option<f32> {
        if !self.fits(text) {
            return None;
        }
        let samples = &samples[..samples.len().min(SAMPLE_RATE as usize * 30)];
        let (f, n) = self.features_short(m, samples).ok()?;
        let lp = self.log_probs(&f, n, lang);
        let target = self.vocab.target(&self.vocab.words(text));
        if target.tokens.is_empty() {
            return None;
        }
        let spans = token_spans(&lp, &target.tokens, 0).ok()?;
        let letters: Vec<f32> = spans
            .iter()
            .zip(&target.owners)
            .filter(|(_, o)| o.is_some())
            .map(|(s, _)| s.score)
            .collect();
        (!letters.is_empty()).then(|| letters.iter().sum::<f32>() / letters.len() as f32)
    }

    fn log_probs(&self, features: &[f32], n: usize, lang: &str) -> LogProbs {
        let l = self.out.get(lang).unwrap_or(&self.out["any"]);
        let logits = linear(features, n, &l.w, Some(&l.b));
        LogProbs::from_logits(&logits, l.w.rows, FRAME)
    }

    /// Whether most of `text` is spelled in the head's letters.
    fn fits(&self, text: &str) -> bool {
        let words = self.vocab.words(text);
        let latin = words.iter().filter(|w| w.alignable()).count();
        !words.is_empty() && latin as f32 >= words.len() as f32 * MIN_LATIN
    }
}

/// A head at work for the speech model it was made for.
pub struct HeadAligner<'a> {
    model: &'a Model,
    head: &'a Head,
    whole_song: bool,
}

impl<'a> HeadAligner<'a> {
    pub fn new(model: &'a Model, head: &'a Head) -> HeadAligner<'a> {
        HeadAligner {
            model,
            head,
            whole_song: true,
        }
    }

    /// With false the song is still recognised, and the head only times the
    /// lines around where they were recognised.
    pub fn set_whole_song(&mut self, on: bool) {
        self.whole_song = on;
    }
}

/// Moves the stray first letters of each line (`line_of` a word) next to the
/// rest: where the head hears little, such as sung vowels, the path may put
/// a line's first letter or two at any cheap frame long before the line is
/// sung. At most `STRAY_LETTERS` before a gap of more than `gap` frames, so
/// that a note held within a line stays (on the hand-timed songs 93.4 % of
/// the lines within half a second instead of 92.3 %).
fn pull_strays(spans: &mut [Span], target: &Target, line_of: impl Fn(usize) -> usize, gap: usize) {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut last = None;
    for (k, owner) in target.owners.iter().enumerate() {
        let Some((word, _)) = *owner else { continue };
        let line = line_of(word);
        if last != Some(line) {
            lines.push(Vec::new());
            last = Some(line);
        }
        lines.last_mut().unwrap().push(k);
    }
    for ks in lines {
        let mut i = 0;
        while let Some(m) =
            (i + 1..ks.len()).find(|&m| spans[ks[m]].start > spans[ks[m - 1]].start + gap)
        {
            if m - i > STRAY_LETTERS {
                break;
            }
            i = m;
        }
        let to = spans[ks[i]].start;
        for (j, &k) in ks[..i].iter().enumerate() {
            let start = to - (i - j);
            spans[k] = Span {
                start,
                end: start + 1,
                score: spans[k].score,
            };
        }
    }
}

/// The share of the frames heard as letters that the path spends outside the
/// text's letters (`delimiter` and the blank 0 are no letters).
fn heard_outside(lp: &LogProbs, tokens: &[u32], spans: &[Span], delimiter: Option<u32>) -> f32 {
    let mut inside = vec![false; lp.frames];
    for (s, &t) in spans.iter().zip(tokens) {
        if Some(t) != delimiter {
            inside[s.start..s.end].iter_mut().for_each(|v| *v = true);
        }
    }
    let (mut heard, mut outside) = (0usize, 0usize);
    for (t, &on_text) in inside.iter().enumerate() {
        let r = lp.row(t);
        let best = (0..r.len()).fold(0, |b, i| if r[i] > r[b] { i } else { b }) as u32;
        if best != 0 && Some(best) != delimiter {
            heard += 1;
            outside += !on_text as usize;
        }
    }
    outside as f32 / heard.max(1) as f32
}

fn median_score(words: &[WordTime]) -> f32 {
    let mut s: Vec<f32> = words
        .iter()
        .filter(|w| !w.chars.is_empty())
        .map(|w| w.score)
        .collect();
    s.sort_by(f32::total_cmp);
    s.get(s.len() / 2).copied().unwrap_or(0.0)
}

impl WordAligner for HeadAligner<'_> {
    fn align(&self, lang: &str, audio: &[f32], _at: f64, text: &str) -> Option<Vec<Word>> {
        if !self.head.fits(text) {
            return None;
        }
        let window = SAMPLE_RATE as usize * 30;
        let audio = &audio[..audio.len().min(window)];
        let (f, n) = self.head.features(self.model, audio).ok()?;
        let lp = self.head.log_probs(&f, n, lang);
        let words = word_times(&self.head.vocab, 0, &lp, text).ok()?;
        (median_score(&words) >= MIN_SCORE)
            .then(|| words.iter().map(|w| syllabled(w, lang)).collect())
    }

    fn whole_song(
        &self,
        samples: &[f32],
        lines: &[&str],
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Option<WholeSong> {
        if !self.whole_song {
            return None;
        }
        let text = lines.join(" ");
        if !self.head.fits(&text) {
            return None;
        }
        let sr = SAMPLE_RATE as usize;
        let hidden = self.head.hidden;
        let frames = (samples.len() as f64 / sr as f64 / FRAME) as usize + 1;
        let mut features = vec![0f32; frames * hidden];
        let starts: Vec<usize> = (0..samples.len().saturating_sub(5 * sr).max(1))
            .step_by(HOP_SECS * sr)
            .collect();
        // Where the most is sung, for the language.
        let mut sung = (0usize, 0usize);
        let edge = (EDGE_SECS / FRAME) as usize;
        for (i, &s) in starts.iter().enumerate() {
            if !progress(i as f32 / starts.len() as f32 * 0.9) {
                return None;
            }
            let end = (s + 30 * sr).min(samples.len());
            let (f, n) = self.head.features(self.model, &samples[s..end]).ok()?;
            let at = (s as f64 / sr as f64 / FRAME) as usize;
            let from = if s == 0 { 0 } else { edge };
            let to = if end < samples.len() {
                n.min(1500 - edge)
            } else {
                n
            };
            let to = to.min(frames - at);
            if from < to {
                features[(at + from) * hidden..(at + to) * hidden]
                    .copy_from_slice(&f[from * hidden..to * hidden]);
            }
            let lp = self.head.log_probs(&f, n, "any");
            let voiced = (0..lp.frames)
                .filter(|&t| {
                    let r = lp.row(t);
                    (0..r.len()).fold(0, |b, i| if r[i] > r[b] { i } else { b }) != 0
                })
                .count();
            if voiced >= sung.0 {
                sung = (voiced, s);
            }
        }
        let end = (sung.1 + 30 * sr).min(samples.len());
        let lang = Transcriber::new(self.model)
            .language_of(&samples[sung.1..end])
            .ok()?;
        let lp = self.head.log_probs(&features, frames, lang);
        let spelled = self.head.vocab.words(&text);
        let target = self.head.vocab.target(&spelled);
        if lp.frames.saturating_mul(2 * target.tokens.len() + 1) > MAX_CELLS {
            return None;
        }
        let mut spans = token_spans(&lp, &target.tokens, 0).ok()?;
        let delimiter = self.head.vocab.delimiter;
        let outside = heard_outside(&lp, &target.tokens, &spans, delimiter);
        let ends: Vec<usize> = lines
            .iter()
            .scan(0, |n, l| {
                *n += l.split_whitespace().count();
                Some(*n)
            })
            .collect();
        let gap = (STRAY_GAP / lp.frame_seconds) as usize;
        pull_strays(
            &mut spans,
            &target,
            |w| ends.partition_point(|&e| e <= w),
            gap,
        );
        let words = times_of_spans(&spelled, &target, &spans, &lp);
        if median_score(&words) < MIN_SCORE {
            return None;
        }
        progress(1.0);
        let mut it = words.iter();
        Some(WholeSong {
            lines: lines
                .iter()
                .map(|l| {
                    it.by_ref()
                        .take(l.split_whitespace().count())
                        .map(|w| syllabled(w, lang))
                        .collect()
                })
                .collect(),
            outside,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_heard_outside_the_text_are_counted() {
        // Frames: letter 1, blank, letter 2, delimiter 3, letter 2.
        let row = |best: usize| {
            let mut r = vec![0.0f32; 4];
            r[best] = 5.0;
            r
        };
        let logits: Vec<f32> = [1, 0, 2, 3, 2].iter().flat_map(|&b| row(b)).collect();
        let lp = LogProbs::from_logits(&logits, 4, FRAME);
        let spans = [Span {
            start: 0,
            end: 1,
            score: 1.0,
        }];
        // Three frames are heard as letters, one is on the text's letter.
        let share = heard_outside(&lp, &[1], &spans, Some(3));
        assert!((share - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn stray_first_letters_join_their_line() {
        let span = |start: usize| Span {
            start,
            end: start + 1,
            score: 0.0,
        };
        // Line 0: "ab" at 0 and 1. Line 1: "c" alone at 10, "defg" from 200
        // on. Line 2: "hijk" at 300, then "lm" held long after.
        let starts = [0, 1, 10, 200, 201, 202, 203, 300, 301, 302, 303, 600, 601];
        let words = [0, 0, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3];
        let mut spans: Vec<Span> = starts.iter().map(|&s| span(s)).collect();
        let target = Target {
            tokens: vec![1; starts.len()],
            owners: words.iter().map(|&w| Some((w, 0))).collect(),
        };
        pull_strays(&mut spans, &target, |w| [0, 1, 1, 2][w], 100);
        let got: Vec<usize> = spans.iter().map(|s| s.start).collect();
        assert_eq!(
            got,
            [0, 1, 199, 200, 201, 202, 203, 300, 301, 302, 303, 600, 601]
        );
    }

    #[test]
    fn a_padded_convolution_sums_the_frames_around() {
        // One output, one input, weights 1 to 5 from the earliest frame.
        let weights: Vec<f32> = (1..=5).map(|v| v as f32).collect();
        let l = Layer {
            w: conv_matrix(&weights, 1, 1, KERNEL, Keep::F32),
            b: Tensor::from_f32(1, 1, vec![0.5]),
        };
        let x = [1.0, 0.0, 0.0, 0.0, 0.0, 2.0];
        let got = window_rows(&x, 1, x.len(), &l);
        // Frame 0 is the middle of the kernel: weight 3 on itself, and the
        // weights after it reach the frames that follow.
        let want = [
            3.5,
            2.5,
            1.5,
            0.5 + 2.0 * 5.0,
            0.5 + 2.0 * 4.0,
            0.5 + 2.0 * 3.0,
        ];
        assert_eq!(got, want);
    }
}
