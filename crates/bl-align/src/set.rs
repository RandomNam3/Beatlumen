//! The alignment models in a folder, for the song text timing in bl-asr:
//! the model of a song's language is loaded when first needed and kept,
//! and its word times come with the words' syllables (bl-hyph) timed by
//! their first letter.

use crate::{models, Aligner, WordTime};
use bl_asr::timing::Word;
use bl_asr::transcribe::WordAligner;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Below this median word score the text was not sung in the stretch
/// (line times that are off): the words are left to Whisper.
const MIN_SCORE: f32 = 0.02;

/// The models under `dir` (one folder per model, named by its catalog id).
pub struct Aligners {
    dir: PathBuf,
    loaded: Mutex<HashMap<&'static str, Option<Arc<Aligner>>>>,
    /// The song's singing alone (16 kHz), listened to instead of the mix.
    vocals: Option<Vec<f32>>,
}

impl Aligners {
    pub fn new(dir: impl Into<PathBuf>) -> Aligners {
        Aligners {
            dir: dir.into(),
            loaded: Mutex::default(),
            vocals: None,
        }
    }

    /// Listen to the song's singing alone (16 kHz mono, from the song's
    /// start) instead of the audio the timing passes.
    pub fn set_vocals(&mut self, vocals: Option<Vec<f32>>) {
        self.vocals = vocals;
    }

    /// Whether the model for `lang` is in the folder (all its files).
    pub fn available(&self, lang: &str) -> bool {
        models::for_language(lang).is_some_and(|m| {
            m.files
                .iter()
                .all(|f| self.dir.join(m.id).join(f.0).is_file())
        })
    }

    fn get(&self, lang: &str) -> Option<Arc<Aligner>> {
        let m = models::for_language(lang)?;
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = loaded.get(m.id) {
            return a.clone();
        }
        let a = self
            .available(lang)
            .then(|| Aligner::load(self.dir.join(m.id)).ok().map(Arc::new))
            .flatten();
        loaded.insert(m.id, a.clone());
        a
    }
}

impl WordAligner for Aligners {
    fn align(&self, lang: &str, audio: &[f32], at: f64, text: &str) -> Option<Vec<Word>> {
        let audio = match &self.vocals {
            Some(v) => {
                let from = ((at * 16000.0) as usize).min(v.len());
                &v[from..(from + audio.len()).min(v.len())]
            }
            None => audio,
        };
        let words = self.get(lang)?.align(audio, text).ok()?;
        let mut scores: Vec<f32> = words
            .iter()
            .filter(|w| !w.chars.is_empty())
            .map(|w| w.score)
            .collect();
        scores.sort_by(f32::total_cmp);
        if scores
            .get(scores.len() / 2)
            .map_or(true, |&s| s < MIN_SCORE)
        {
            return None;
        }
        Some(words.iter().map(|w| syllabled(w, lang)).collect())
    }
}

/// A word with its syllables timed by the first aligned letter of each.
fn syllabled(w: &WordTime, lang: &str) -> Word {
    // The start of each character of the word as written (None for those
    // not aligned: punctuation, digits).
    let mut times = Vec::new();
    let mut aligned = w.chars.iter().peekable();
    for c in w.text.chars() {
        let hit = aligned
            .peek()
            .filter(|(a, _, _)| a.to_lowercase().eq(c.to_lowercase()))
            .map(|&&(_, t, _)| t);
        if hit.is_some() {
            aligned.next();
        }
        times.push(hit);
    }
    let parts = bl_hyph::syllables(&w.text, lang);
    let mut pieces = Vec::new();
    if parts.len() > 1 {
        let mut at = 0usize;
        let mut last = w.start;
        for p in parts {
            let n = p.chars().count();
            let t = times[at..(at + n).min(times.len())]
                .iter()
                .flatten()
                .next()
                .copied()
                .unwrap_or(last)
                .max(last);
            pieces.push((t, p));
            last = t;
            at += n;
        }
    }
    Word {
        text: w.text.clone(),
        start: w.start,
        end: w.end.max(w.start),
        pieces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syllables_take_the_times_of_their_letters() {
        let w = WordTime {
            text: "Zukunft,".into(),
            start: 1.0,
            end: 2.0,
            chars: "zukunft"
                .chars()
                .enumerate()
                .map(|(i, c)| (c, 1.0 + i as f64 * 0.1, 1.05 + i as f64 * 0.1))
                .collect(),
            score: 0.9,
        };
        let s = syllabled(&w, "de");
        assert_eq!(s.pieces.len(), 2);
        assert_eq!(s.pieces[0].1, "Zu");
        assert_eq!(s.pieces[1].1, "kunft,");
        assert!((s.pieces[1].0 - 1.2).abs() < 1e-9);
        // One syllable: no pieces.
        let one = WordTime {
            text: "you".into(),
            chars: vec![('y', 0.0, 0.1), ('o', 0.1, 0.2), ('u', 0.2, 0.3)],
            ..w
        };
        assert!(syllabled(&one, "en").pieces.is_empty());
    }
}
