//! Word and character times for a known text in a stretch of audio, by
//! CTC forced alignment with a wav2vec2 model (what WhisperX does with
//! torchaudio), implemented here without external libraries: the
//! wav2vec2 network, the CTC trellis and the text normalisation.
//!
//! A model directory holds `config.json`, `vocab.json`,
//! `preprocessor_config.json` and `pytorch_model.bin` (or
//! `model.safetensors`), as published on Hugging Face for
//! `Wav2Vec2ForCTC`; [`models::CATALOG`] lists models per language.
//!
//! The model gives each 20 ms of audio the probabilities of its
//! characters; the best path of the text's characters through them gives
//! each character its frames. Align a line or a few (up to about 30 s) at
//! a time: the attention spans the whole stretch.
//!
//! ```no_run
//! let a = bl_align::Aligner::load("models/wav2vec2-base-960h").unwrap();
//! let samples: Vec<f32> = vec![0.0; 16_000 * 3]; // 16 kHz mono
//! for w in a.align(&samples, "You say you love me").unwrap() {
//!     println!("{:.2} {:.2} {}", w.start, w.end, w.text);
//! }
//! ```

#![forbid(unsafe_code)]

pub mod config;
pub mod ctc;
pub mod models;
pub mod text;
pub mod wav2vec2;
pub mod weights;

pub use bl_mt::torch::Keep;
pub use config::{Config, SAMPLE_RATE};
pub use ctc::LogProbs;
use std::path::Path;
use text::Vocab;
use wav2vec2::Model;
use weights::Weights;

/// Largest text file read (config, vocabulary).
const MAX_FILE: u64 = 1 << 20;

/// A word of the text with its times in seconds from the start of the
/// audio, and those of its characters.
#[derive(Debug, Clone, PartialEq)]
pub struct WordTime {
    /// The word as written (split at white space, punctuation kept).
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Each aligned character as written, with its start and end (digits
    /// and punctuation are left out). Empty if the word had none: its
    /// times are then shared out between its neighbours.
    pub chars: Vec<(char, f64, f64)>,
    /// Mean probability of the word's characters on their frames (0 to
    /// 1; 0 for a word without aligned characters).
    pub score: f32,
}

/// A loaded wav2vec2 model with its vocabulary.
pub struct Aligner {
    model: Model,
    vocab: Vocab,
}

fn read_text(dir: &Path, name: &str) -> Result<Option<String>, String> {
    let path = dir.join(name);
    if !path.exists() {
        return Ok(None);
    }
    let len = std::fs::metadata(&path)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    if len > MAX_FILE {
        return Err(format!("{name}: too large"));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{name}: {e}"))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{name}: not UTF-8"))
}

impl Aligner {
    /// Loads a model directory, keeping the weight matrices as f16 (half
    /// the memory of f32; the logits move by about 0.001 on average, the
    /// alignment stays the same).
    pub fn load(dir: impl AsRef<Path>) -> Result<Aligner, String> {
        Self::load_with(dir, Keep::F16)
    }

    /// Loads a model directory, keeping the weight matrices as `keep`.
    pub fn load_with(dir: impl AsRef<Path>, keep: Keep) -> Result<Aligner, String> {
        let dir = dir.as_ref();
        let config = read_text(dir, "config.json")?.ok_or("config.json: missing")?;
        let pre = match read_text(dir, "preprocessor_config.json")? {
            Some(p) => Some(p),
            None => read_text(dir, "feature_extractor_config.json")?,
        };
        let config = Config::parse(&config, pre.as_deref())?;
        let delimiter = read_text(dir, "tokenizer_config.json")?
            .and_then(|t| bl_json::parse(&t).ok())
            .and_then(|v| v.get("word_delimiter_token").as_str().map(str::to_string))
            .unwrap_or_else(|| "|".to_string());
        let vocab = read_text(dir, "vocab.json")?.ok_or("vocab.json: missing")?;
        let vocab = Vocab::parse(&vocab, config.vocab_size, &delimiter)?;
        let weights = Weights::open(dir)?;
        let model = Model::load(config, &weights, keep)?;
        Ok(Aligner { model, vocab })
    }

    pub fn config(&self) -> &Config {
        &self.model.config
    }

    pub fn vocab(&self) -> &Vocab {
        &self.vocab
    }

    /// Memory taken by the weights (bytes).
    pub fn weight_bytes(&self) -> usize {
        self.model.weight_bytes()
    }

    /// The model's log-probabilities of its tokens for each 20 ms frame of
    /// 16 kHz mono `samples` (no frames if shorter than 25 ms).
    pub fn log_probs(&self, samples: &[f32]) -> LogProbs {
        let c = self.config();
        LogProbs::from_logits(&self.logits(samples), c.vocab_size, c.frame_seconds())
    }

    /// The model's raw logits: `vocab_size` values per frame.
    pub fn logits(&self, samples: &[f32]) -> Vec<f32> {
        self.model.logits(samples)
    }

    /// What the model hears (its most likely characters per frame).
    pub fn hears(&self, lp: &LogProbs) -> String {
        let blank = self.config().blank as u32;
        let tokens = lp.greedy(blank);
        let delim = self.vocab.delimiter;
        tokens
            .iter()
            .map(|&t| {
                if Some(t) == delim {
                    " "
                } else {
                    self.vocab.texts.get(t as usize).map_or("", String::as_str)
                }
            })
            .collect::<String>()
            .trim()
            .to_string()
    }

    /// The times of the words of `text` (split at white space) in 16 kHz
    /// mono `samples`: one entry per word, in order. Every word is placed
    /// in the stretch, so leave some room around the sung words: words
    /// past its end are squeezed into its last frames (with low scores).
    /// Fails if the text has more characters than the stretch has frames.
    pub fn align(&self, samples: &[f32], text: &str) -> Result<Vec<WordTime>, String> {
        if text.split_whitespace().next().is_none() {
            return Ok(Vec::new());
        }
        self.align_log_probs(&self.log_probs(samples), text)
    }

    /// As [`align`](Self::align), with log-probabilities computed before
    /// (e.g. once for several candidate texts).
    pub fn align_log_probs(&self, lp: &LogProbs, text: &str) -> Result<Vec<WordTime>, String> {
        word_times(&self.vocab, self.config().blank as u32, lp, text)
    }
}

/// The times of the words of `text` in `lp` (with `blank` the CTC
/// blank), as [`Aligner::align`] gives them.
pub fn word_times(
    vocab: &Vocab,
    blank: u32,
    lp: &LogProbs,
    text: &str,
) -> Result<Vec<WordTime>, String> {
    let words = vocab.words(text);
    let target = vocab.target(&words);
    let duration = lp.frames as f64 * lp.frame_seconds;
    let mut out: Vec<Option<WordTime>> = words.iter().map(|_| None).collect();
    if !target.tokens.is_empty() {
        let spans = ctc::token_spans(lp, &target.tokens, blank)?;
        let secs = |f: usize| f as f64 * lp.frame_seconds;
        // Per word: per character (first frame, end frame), and the
        // probability sum and frame count.
        let mut chars: Vec<Vec<Option<(usize, usize)>>> =
            words.iter().map(|w| vec![None; w.chars.len()]).collect();
        let mut score = vec![(0f64, 0usize); words.len()];
        for (span, owner) in spans.iter().zip(&target.owners) {
            let Some((wi, ci)) = *owner else { continue };
            let c = &mut chars[wi][ci];
            *c = Some(match *c {
                None => (span.start, span.end),
                Some((s, e)) => (s.min(span.start), e.max(span.end)),
            });
            let n = span.end - span.start;
            score[wi].0 += span.score as f64 * n as f64;
            score[wi].1 += n;
        }
        for (wi, w) in words.iter().enumerate() {
            let cs: Vec<(char, f64, f64)> = w
                .chars
                .iter()
                .zip(&chars[wi])
                .filter_map(|((c, _), f)| f.map(|(s, e)| (*c, secs(s), secs(e))))
                .collect();
            if let (Some(first), Some(last)) = (cs.first(), cs.last()) {
                out[wi] = Some(WordTime {
                    text: w.text.clone(),
                    start: first.1,
                    end: last.2,
                    score: (score[wi].0 / score[wi].1.max(1) as f64) as f32,
                    chars: cs,
                });
            }
        }
    }
    Ok(fill_gaps(&words, out, duration))
}

/// Gives the words without aligned characters times: runs of them share
/// the gap between the aligned words around them evenly (at the start or
/// end of the text they get no length; with no aligned word at all they
/// share the whole stretch).
fn fill_gaps(words: &[text::Word], out: Vec<Option<WordTime>>, duration: f64) -> Vec<WordTime> {
    let mut result = Vec::with_capacity(out.len());
    let mut i = 0;
    while i < out.len() {
        if let Some(w) = &out[i] {
            result.push(w.clone());
            i += 1;
            continue;
        }
        let run_end = (i..out.len())
            .find(|&j| out[j].is_some())
            .unwrap_or(out.len());
        let before = result.last().map(|w: &WordTime| w.end);
        let after = out.get(run_end).and_then(|w| w.as_ref()).map(|w| w.start);
        let (lo, hi) = match (before, after) {
            (Some(b), Some(a)) => (b, a.max(b)),
            (Some(b), None) => (b, b),
            (None, Some(a)) => (a, a),
            (None, None) => (0.0, duration),
        };
        let n = run_end - i;
        for (k, w) in words[i..run_end].iter().enumerate() {
            let step = (hi - lo) / n as f64;
            result.push(WordTime {
                text: w.text.clone(),
                start: lo + step * k as f64,
                end: lo + step * (k + 1) as f64,
                chars: Vec::new(),
                score: 0.0,
            });
        }
        i = run_end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use text::Vocab;

    fn word(text: &str, start: f64, end: f64) -> Option<WordTime> {
        Some(WordTime {
            text: text.into(),
            start,
            end,
            chars: Vec::new(),
            score: 1.0,
        })
    }

    #[test]
    fn word_times_from_a_small_matrix() {
        // Blank 0, delimiter 1, A 2, B 3; frames heard: _ A A | B _ B _.
        let v = Vocab::from_chars("AB", Some('|'));
        let heard = [0, 2, 2, 1, 3, 0, 3, 0];
        let mut data = Vec::new();
        for &h in &heard {
            for t in 0..4 {
                data.push(if t == h { 0.85f32 } else { 0.05 }.ln());
            }
        }
        let lp = LogProbs {
            frames: heard.len(),
            vocab: 4,
            data,
            frame_seconds: 0.5,
        };
        let w = word_times(&v, 0, &lp, "a, 42 bb!").unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].text.as_str(), w[0].start, w[0].end), ("a,", 0.5, 1.5));
        assert_eq!(w[0].chars, vec![('a', 0.5, 1.5)]);
        assert!((w[0].score - 0.85).abs() < 1e-5);
        // "42" has nothing to align: it gets the gap between its
        // neighbours.
        assert_eq!((w[1].start, w[1].end), (1.5, 2.0));
        assert!(w[1].chars.is_empty());
        assert_eq!(w[2].chars, vec![('b', 2.0, 2.5), ('b', 3.0, 3.5)]);
        assert_eq!((w[2].start, w[2].end), (2.0, 3.5));
        assert!(word_times(&v, 0, &lp, "").unwrap().is_empty());
        // More letters than frames.
        assert!(word_times(&v, 0, &lp, "aaaaa").is_err());
    }

    #[test]
    fn unaligned_words_share_the_gaps() {
        let v = Vocab::from_chars("ab", None);
        let words = v.words("1 a 2 3 b 4");
        let out = vec![
            None,
            word("a", 1.0, 2.0),
            None,
            None,
            word("b", 3.0, 4.0),
            None,
        ];
        let r = fill_gaps(&words, out, 10.0);
        let times: Vec<(f64, f64)> = r.iter().map(|w| (w.start, w.end)).collect();
        assert_eq!(
            times,
            [
                (1.0, 1.0),
                (1.0, 2.0),
                (2.0, 2.5),
                (2.5, 3.0),
                (3.0, 4.0),
                (4.0, 4.0)
            ]
        );
        let texts: Vec<&str> = r.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["1", "a", "2", "3", "b", "4"]);
        let r = fill_gaps(&v.words("1 2"), vec![None, None], 4.0);
        assert_eq!((r[1].start, r[1].end), (2.0, 4.0));
    }
}
