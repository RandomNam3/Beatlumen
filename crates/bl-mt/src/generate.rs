//! Choosing the output tokens: greedy decoding or beam search as in
//! Hugging Face `generate` (length penalty 1, no early stopping: a search
//! ends when no running hypothesis can beat the finished ones).

use crate::marian::{Cache, Encoded, Model};

/// How output tokens are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Search {
    /// The most likely token at each step.
    Greedy,
    /// Beam search with this many hypotheses.
    Beam(usize),
}

/// Log probabilities of `logits` (in place), with the tokens never to be
/// generated removed and, on the last step, the end token forced.
fn log_probs(model: &Model, logits: &mut [f32], last_step: bool) {
    let c = &model.config;
    for &t in &c.bad_words {
        logits[t as usize] = f32::NEG_INFINITY;
    }
    if last_step {
        for (i, v) in logits.iter_mut().enumerate() {
            if i != c.eos as usize {
                *v = f32::NEG_INFINITY;
            }
        }
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|v| (v - max).exp()).sum();
    let lse = max + sum.ln();
    logits.iter_mut().for_each(|v| *v -= lse);
}

/// The output tokens for the encoded input (without the start and end
/// tokens), at most `max_len - 1` of them.
pub fn generate(model: &Model, enc: &Encoded, search: Search, max_len: usize) -> Vec<u32> {
    let max_len = max_len.clamp(2, model.max_positions());
    match search {
        Search::Greedy | Search::Beam(0 | 1) => greedy(model, enc, max_len),
        Search::Beam(n) => beam(model, enc, n, max_len),
    }
}

fn greedy(model: &Model, enc: &Encoded, max_len: usize) -> Vec<u32> {
    let c = &model.config;
    let mut cache = [Cache::default()];
    let mut out = Vec::new();
    let mut last = c.decoder_start;
    for pos in 0..max_len - 1 {
        let mut logits = model.step(enc, &mut cache, &[last], pos);
        log_probs(model, &mut logits, pos + 2 == max_len);
        let best = argmax(&logits);
        if best == c.eos {
            break;
        }
        out.push(best);
        last = best;
    }
    out
}

fn argmax(v: &[f32]) -> u32 {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    best as u32
}

/// The best finished hypotheses (at most `n`), by score.
struct Finished {
    n: usize,
    hyps: Vec<(f32, Vec<u32>)>,
}

impl Finished {
    fn worst(&self) -> f32 {
        self.hyps.last().map_or(f32::NEG_INFINITY, |h| h.0)
    }

    /// Adds a hypothesis with total log probability `sum`; its score is
    /// the mean per token (the start token counted, the end token not).
    fn add(&mut self, tokens: &[u32], sum: f32) {
        let score = sum / tokens.len() as f32;
        if self.hyps.len() < self.n || score > self.worst() {
            let at = self.hyps.partition_point(|h| h.0 >= score);
            self.hyps.insert(at, (score, tokens.to_vec()));
            self.hyps.truncate(self.n);
        }
    }

    /// Whether a running hypothesis with total `best` at `len` tokens can
    /// no longer get in.
    fn done(&self, best: f32, len: usize) -> bool {
        self.hyps.len() >= self.n && self.worst() >= best / len as f32
    }
}

fn beam(model: &Model, enc: &Encoded, n: usize, max_len: usize) -> Vec<u32> {
    let c = &model.config;
    let vocab = c.vocab_size;
    let mut seqs: Vec<Vec<u32>> = vec![vec![c.decoder_start]; n];
    // Only the first hypothesis takes part at first (they are all equal).
    let mut scores: Vec<f32> = (0..n).map(|i| if i == 0 { 0.0 } else { -1e9 }).collect();
    let mut caches = vec![Cache::default(); n];
    let mut finished = Finished {
        n,
        hyps: Vec::new(),
    };
    let mut done = false;
    let mut len = 1;
    while len < max_len {
        let last: Vec<u32> = seqs.iter().map(|s| s[s.len() - 1]).collect();
        let mut logits = model.step(enc, &mut caches, &last, len - 1);
        // The 2n best continuations over all hypotheses: (score, beam, token).
        let mut top: Vec<(f32, usize, u32)> = Vec::with_capacity(2 * n + 1);
        for (b, row) in logits.chunks_mut(vocab).enumerate() {
            log_probs(model, row, len + 1 == max_len);
            for (t, &lp) in row.iter().enumerate() {
                let s = scores[b] + lp;
                if top.len() < 2 * n || s > top[top.len() - 1].0 {
                    let at = top.partition_point(|x| x.0 >= s);
                    top.insert(at, (s, b, t as u32));
                    top.truncate(2 * n);
                }
            }
        }
        let mut next: Vec<(f32, usize, u32)> = Vec::with_capacity(n);
        for (rank, &(s, b, t)) in top.iter().enumerate() {
            if t == c.eos {
                if rank < n {
                    finished.add(&seqs[b], s);
                }
            } else {
                next.push((s, b, t));
                if next.len() == n {
                    break;
                }
            }
        }
        if top.is_empty() || finished.done(top[0].0, len) {
            done = true;
            break;
        }
        // Continue the chosen hypotheses (their caches copied as needed).
        let mut old_caches: Vec<Option<Cache>> = caches.into_iter().map(Some).collect();
        let mut uses = vec![0usize; n];
        for &(_, b, _) in &next {
            uses[b] += 1;
        }
        caches = next
            .iter()
            .map(|&(_, b, _)| {
                uses[b] -= 1;
                if uses[b] == 0 {
                    old_caches[b].take().unwrap_or_default()
                } else {
                    old_caches[b].clone().unwrap_or_default()
                }
            })
            .collect();
        seqs = next
            .iter()
            .map(|&(_, b, t)| {
                let mut s = seqs[b].clone();
                s.push(t);
                s
            })
            .collect();
        scores = next.iter().map(|x| x.0).collect();
        len += 1;
        if seqs.len() < n {
            // Fewer continuations than beams (a tiny vocabulary).
            break;
        }
    }
    if !done {
        for (s, &score) in seqs.iter().zip(&scores) {
            finished.add(s, score);
        }
    }
    finished
        .hyps
        .into_iter()
        .next()
        .map(|(_, s)| s[1..].to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_best_finished() {
        let mut f = Finished {
            n: 2,
            hyps: Vec::new(),
        };
        f.add(&[0, 1], -4.0); // -2
        f.add(&[0, 1, 2, 3], -4.0); // -1
        f.add(&[0], -3.0); // -3: not among the best two
        assert_eq!(f.hyps.len(), 2);
        assert_eq!(f.hyps[0].1, vec![0, 1, 2, 3]);
        assert_eq!(f.worst(), -2.0);
        assert!(f.done(-5.0, 2));
        assert!(!f.done(-3.0, 2));
    }

    #[test]
    fn argmax_finds_the_largest() {
        assert_eq!(argmax(&[0.0, 3.0, -1.0, 3.0]), 1);
        assert_eq!(argmax(&[f32::NEG_INFINITY, -1.0]), 1);
    }
}
