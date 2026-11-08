//! Points for the text in a karaoke round: each sung line (the singer's
//! microphone at 16 kHz, the line with a little room around it) scored
//! against the known line by the CTC head on the speech model's encoder
//! (`bl_align::Head::sung_score`), on a worker thread, one line after the
//! other. Uses the picked speech model when it has a head, else the
//! smallest model in the models folder that has one; without any, a round
//! has no text points.

use crate::host::Host;
use crate::jobs::Mailbox;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Below this score a line's words count as not sung, above `FULL` as
/// sung fully (measured: the song's own vocals score 0.27-0.34 in the
/// median, another line's words 0.10).
pub(crate) const NONE: f32 = 0.08;
pub(crate) const FULL: f32 = 0.28;
/// Lines waiting at most; a slow device skips the oldest.
const MAX_WAITING: usize = 3;

/// A line to score.
pub(crate) struct Job {
    pub singer: usize,
    pub line: usize,
    pub audio: Vec<f32>,
    pub text: String,
    pub lang: String,
}

/// What the worker says: a line's score, or that there is no head.
pub(crate) enum Answer {
    Ready,
    Unavailable,
    Line {
        singer: usize,
        line: usize,
        score: Option<f32>,
    },
}

pub(crate) struct TextScorer {
    tx: mpsc::Sender<Job>,
    pub answers: Mailbox<Answer>,
}

/// A line's share of its text points from its score (0..1).
pub(crate) fn accuracy(score: f32) -> f64 {
    ((score - NONE) / (FULL - NONE)).clamp(0.0, 1.0) as f64
}

/// The speech model to score with: the picked one when it has a head,
/// else the smallest with one in `dir`.
pub(crate) fn model_with_head(dir: &Path, picked: &str) -> Option<PathBuf> {
    let has_head = |p: &Path| {
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            return false;
        };
        let plain = stem.rsplit_once("-q").map_or(stem, |(s, _)| s);
        [stem, plain]
            .iter()
            .any(|s| p.with_file_name(format!("{s}.head")).is_file())
    };
    if let Some(m) = bl_asr::models::find(picked) {
        let p = dir.join(m.file);
        if p.is_file() && has_head(&p) {
            return Some(p);
        }
    }
    let mut found: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bin") && has_head(p))
        .map(|p| (std::fs::metadata(&p).map_or(u64::MAX, |m| m.len()), p))
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, p)| p)
}

impl TextScorer {
    /// Starts the worker with the model at `path` (loaded on the worker).
    pub(crate) fn start(host: &dyn Host, path: PathBuf) -> TextScorer {
        let (tx, rx) = mpsc::channel::<Job>();
        let answers: Mailbox<Answer> = Mailbox::default();
        let out = answers.clone();
        host.spawn(Box::new(move || {
            let loaded = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|f| {
                    bl_asr::model::Model::load(&mut std::io::BufReader::new(f))
                        .map_err(|e| e.to_string())
                })
                .ok()
                .and_then(|m| Some((bl_align::Head::for_model(&path)?, m)));
            let Some((head, model)) = loaded else {
                out.post(Answer::Unavailable);
                return;
            };
            out.post(Answer::Ready);
            while let Ok(first) = rx.recv() {
                // Behind: the newest lines first, the oldest dropped.
                let mut jobs = vec![first];
                jobs.extend(rx.try_iter());
                let skip = jobs.len().saturating_sub(MAX_WAITING);
                for (i, j) in jobs.into_iter().enumerate() {
                    let score = if i < skip {
                        None
                    } else {
                        head.sung_score(&model, &j.audio, &j.lang, &j.text)
                    };
                    out.post(Answer::Line {
                        singer: j.singer,
                        line: j.line,
                        score,
                    });
                }
            }
        }));
        TextScorer { tx, answers }
    }

    pub(crate) fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_to_shares() {
        assert_eq!(accuracy(0.05), 0.0);
        assert_eq!(accuracy(0.5), 1.0);
        assert!((accuracy(0.18) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn models_with_heads() {
        let dir = std::env::temp_dir().join(format!("bl-karaoke-heads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(model_with_head(&dir, "base").is_none());
        std::fs::write(dir.join("ggml-base.bin"), vec![0u8; 100]).unwrap();
        assert!(model_with_head(&dir, "base").is_none());
        std::fs::write(dir.join("ggml-big.bin"), vec![0u8; 300]).unwrap();
        std::fs::write(dir.join("ggml-big.head"), b"h").unwrap();
        std::fs::write(dir.join("ggml-small-q5_1.bin"), vec![0u8; 200]).unwrap();
        std::fs::write(dir.join("ggml-small.head"), b"h").unwrap();
        // The smallest with a head (a quantised model takes its plain one's).
        assert_eq!(
            model_with_head(&dir, "").unwrap().file_name().unwrap(),
            "ggml-small-q5_1.bin"
        );
        std::fs::write(dir.join("ggml-base.head"), b"h").unwrap();
        assert_eq!(
            model_with_head(&dir, "base").unwrap().file_name().unwrap(),
            "ggml-base.bin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
