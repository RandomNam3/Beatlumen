//! CTC forced alignment: the most likely way the model's frames spell a
//! known token sequence (Viterbi through the CTC trellis, as
//! `torchaudio.functional.forced_align`).
//!
//! The trellis has a blank before, between and after the tokens (state
//! `2i + 1` is token `i`, even states are blanks). Each frame stays in its
//! state or moves on by one; it may skip a blank between two different
//! tokens, but a repeated token needs a blank between its two copies.
//! Ties prefer staying, then the next state (as torchaudio does).

/// Natural-log probabilities: one row of `vocab` values per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct LogProbs {
    pub frames: usize,
    pub vocab: usize,
    pub data: Vec<f32>,
    /// Seconds per frame.
    pub frame_seconds: f64,
}

impl LogProbs {
    /// Log-softmax of logits (`vocab` values per frame).
    pub fn from_logits(logits: &[f32], vocab: usize, frame_seconds: f64) -> LogProbs {
        let mut data = logits.to_vec();
        for row in data.chunks_mut(vocab.max(1)) {
            let max = row.iter().copied().fold(f32::MIN, f32::max);
            let sum: f64 = row.iter().map(|&v| ((v - max) as f64).exp()).sum();
            let log = max + sum.ln() as f32;
            row.iter_mut().for_each(|v| *v -= log);
        }
        LogProbs {
            frames: data.len() / vocab.max(1),
            vocab,
            data,
            frame_seconds,
        }
    }

    pub fn row(&self, t: usize) -> &[f32] {
        &self.data[t * self.vocab..(t + 1) * self.vocab]
    }

    /// The most likely token of each frame, repeats merged and blanks
    /// left out (what the model hears).
    pub fn greedy(&self, blank: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut prev = None;
        for t in 0..self.frames {
            let row = self.row(t);
            let best = (0..row.len()).fold(0, |b, i| if row[i] > row[b] { i } else { b }) as u32;
            if Some(best) != prev && best != blank {
                out.push(best);
            }
            prev = Some(best);
        }
        out
    }
}

/// The frames `start..end` a token takes on the best path, and the mean
/// probability the model gives it there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub score: f32,
}

/// Frames the tokens need at least: one each, and one more for a blank
/// between each pair of equal neighbours.
pub fn min_frames(tokens: &[u32]) -> usize {
    tokens.len() + tokens.windows(2).filter(|p| p[0] == p[1]).count()
}

/// The best path of `tokens` through `lp`: the trellis state of each
/// frame. Fails if there are fewer frames than the tokens need, a token
/// is outside the vocabulary or the blank is.
pub fn best_path(lp: &LogProbs, tokens: &[u32], blank: u32) -> Result<Vec<usize>, String> {
    let (frames, vocab) = (lp.frames, lp.vocab);
    if blank as usize >= vocab || tokens.iter().any(|&t| t as usize >= vocab || t == blank) {
        return Err("alignment: token outside the vocabulary".into());
    }
    if frames == 0 {
        return Err("alignment: no audio".into());
    }
    if frames < min_frames(tokens) {
        return Err("alignment: the text is too long for the audio".into());
    }
    let states = 2 * tokens.len() + 1;
    let label = |s: usize| if s % 2 == 1 { tokens[s / 2] } else { blank } as usize;
    // A blank may be skipped from state s - 2 to s (both tokens) unless
    // they are the same token.
    let skip = |s: usize| s % 2 == 1 && s >= 3 && tokens[s / 2] != tokens[s / 2 - 1];
    let mut prev = vec![f32::NEG_INFINITY; states];
    let mut cur = vec![f32::NEG_INFINITY; states];
    // How far back each state's best predecessor is (0, 1 or 2).
    let mut back = vec![0u8; frames * states];
    let row = lp.row(0);
    prev[0] = row[blank as usize];
    if states > 1 {
        prev[1] = row[label(1)];
    }
    for t in 1..frames {
        let row = lp.row(t);
        // States that can still reach the end in time, and those reachable
        // by now; the rest stay impossible.
        let left = frames - t;
        let first = states.saturating_sub(2 * left);
        let last = (2 * t + 2).min(states);
        cur.iter_mut().for_each(|v| *v = f32::NEG_INFINITY);
        let back = &mut back[t * states..(t + 1) * states];
        for s in first..last {
            let mut best = prev[s];
            let mut from = 0;
            if s >= 1 && prev[s - 1] > best {
                best = prev[s - 1];
                from = 1;
            }
            if skip(s) && prev[s - 2] > best {
                best = prev[s - 2];
                from = 2;
            }
            cur[s] = best + row[label(s)];
            back[s] = from;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    // The path ends in the last token or the blank after it.
    let mut s = if states > 1 && prev[states - 2] >= prev[states - 1] {
        states - 2
    } else {
        states - 1
    };
    if prev[s] == f32::NEG_INFINITY {
        return Err("alignment: no path through the audio".into());
    }
    let mut path = vec![0; frames];
    for t in (0..frames).rev() {
        path[t] = s;
        s -= back[t * states + s] as usize;
    }
    Ok(path)
}

/// The frames of each token on the best path.
pub fn token_spans(lp: &LogProbs, tokens: &[u32], blank: u32) -> Result<Vec<Span>, String> {
    let path = best_path(lp, tokens, blank)?;
    let mut spans = vec![
        Span {
            start: 0,
            end: 0,
            score: 0.0
        };
        tokens.len()
    ];
    let mut t = 0;
    while t < path.len() {
        let s = path[t];
        let mut end = t + 1;
        while end < path.len() && path[end] == s {
            end += 1;
        }
        if s % 2 == 1 {
            let tok = tokens[s / 2] as usize;
            let p: f64 = (t..end).map(|f| (lp.row(f)[tok] as f64).exp()).sum();
            spans[s / 2] = Span {
                start: t,
                end,
                score: (p / (end - t) as f64) as f32,
            };
        }
        t = end;
    }
    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Log-probs from rows of probabilities.
    fn lp(rows: &[&[f32]]) -> LogProbs {
        let vocab = rows[0].len();
        LogProbs {
            frames: rows.len(),
            vocab,
            data: rows.iter().flat_map(|r| r.iter().map(|p| p.ln())).collect(),
            frame_seconds: 0.02,
        }
    }

    #[test]
    fn follows_the_likely_frames() {
        // Blank 0, tokens 1 and 2: "1 1 _ 2 _".
        let m = lp(&[
            &[0.1, 0.8, 0.1],
            &[0.1, 0.8, 0.1],
            &[0.8, 0.1, 0.1],
            &[0.1, 0.1, 0.8],
            &[0.8, 0.1, 0.1],
        ]);
        assert_eq!(best_path(&m, &[1, 2], 0).unwrap(), vec![1, 1, 2, 3, 4]);
        let s = token_spans(&m, &[1, 2], 0).unwrap();
        assert_eq!((s[0].start, s[0].end, s[1].start, s[1].end), (0, 2, 3, 4));
        assert!((s[0].score - 0.8).abs() < 1e-6);
    }

    #[test]
    fn a_repeated_token_needs_a_blank() {
        // Token 1 twice: the path must pass through a blank between them,
        // even where the model hears no blank.
        let m = lp(&[&[0.1, 0.9], &[0.1, 0.9], &[0.1, 0.9]]);
        assert_eq!(best_path(&m, &[1, 1], 0).unwrap(), vec![1, 2, 3]);
        let two = lp(&[&[0.1, 0.9], &[0.1, 0.9]]);
        assert!(best_path(&two, &[1, 1], 0).is_err());
        assert_eq!(min_frames(&[1, 1, 2, 2, 2]), 8);
    }

    #[test]
    fn uses_every_frame_and_ends_in_place() {
        // More frames than tokens: the token keeps the frames it is heard
        // in, the blank the rest.
        let m = lp(&[
            &[0.9, 0.1],
            &[0.2, 0.8],
            &[0.3, 0.7],
            &[0.9, 0.1],
            &[0.9, 0.1],
        ]);
        assert_eq!(best_path(&m, &[1], 0).unwrap(), vec![0, 1, 1, 2, 2]);
        // A single frame for a single token.
        let one = lp(&[&[0.9, 0.1]]);
        assert_eq!(best_path(&one, &[1], 0).unwrap(), vec![1]);
        // No tokens: all blank.
        assert_eq!(best_path(&m, &[], 0).unwrap(), vec![0; 5]);
    }

    #[test]
    fn rejects_bad_input() {
        let m = lp(&[&[0.5, 0.5]]);
        assert!(best_path(&m, &[2], 0).is_err());
        assert!(best_path(&m, &[0], 0).is_err());
        assert!(best_path(&m, &[1, 1], 0).is_err());
        let empty = LogProbs {
            frames: 0,
            vocab: 2,
            data: Vec::new(),
            frame_seconds: 0.02,
        };
        assert!(best_path(&empty, &[1], 0).is_err());
    }

    #[test]
    fn log_softmax_and_greedy() {
        let l = LogProbs::from_logits(&[1.0, 2.0, 3.0, 5.0, 0.0, 0.0], 3, 0.02);
        let sum: f32 = l.row(0).iter().map(|v| v.exp()).sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert_eq!(l.greedy(0), vec![2]);
        let m = lp(&[
            &[0.1, 0.8, 0.1],
            &[0.1, 0.8, 0.1],
            &[0.8, 0.1, 0.1],
            &[0.1, 0.8, 0.1],
            &[0.1, 0.1, 0.8],
        ]);
        assert_eq!(m.greedy(0), vec![1, 1, 2]);
    }
}
