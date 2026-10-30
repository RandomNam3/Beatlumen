//! Word times from the decoder's attention to the audio, as Whisper finds
//! them: the weights of the alignment heads are normalised per audio
//! frame, smoothed along time and averaged; a monotonic path through them
//! (dynamic time warping) gives each token the frame where it starts.

/// Audio frames per second at the decoder's attention (20 ms each).
pub const FRAMES_PER_SEC: f64 = 50.0;
/// Width of the median filter along time (frames).
const MEDIAN_WIDTH: usize = 7;

/// A word with its start and end in seconds, and the start of each of its
/// pieces (the model's tokens, often syllable-like) when it has several.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// (start, text) of each piece; joined they give `text`. Empty for a
    /// word of one piece.
    pub pieces: Vec<(f64, String)>,
}

/// Heads kept for timing: those whose attention is most focused on a
/// moment of the audio (the ones that follow the words).
const FOCUSED_HEADS: usize = 6;

/// How well a head follows the text through the audio: its focus (the
/// rows' largest weights) times how steadily the focus moves on from row
/// to row and how much of the audio it covers. Heads that stay on one
/// spot (e.g. the start) score low however sharp they are.
fn focus(w: &[f32], rows: usize, stride: usize, frames: usize) -> f32 {
    if rows < 2 || frames == 0 {
        return 0.0;
    }
    let mut peak_sum = 0.0;
    let mut peaks = Vec::with_capacity(rows);
    for r in 0..rows {
        let row = &w[r * stride..r * stride + frames];
        let (i, v) = row
            .iter()
            .enumerate()
            .fold((0, f32::MIN), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
        peak_sum += v;
        peaks.push(i);
    }
    let forward = peaks.windows(2).filter(|p| p[1] >= p[0]).count() as f32 / (rows - 1) as f32;
    let (lo, hi) = (
        peaks.iter().min().copied().unwrap_or(0),
        peaks.iter().max().copied().unwrap_or(0),
    );
    let cover = (hi - lo) as f32 / frames as f32;
    peak_sum / rows as f32 * forward * cover
}

/// One matrix of `rows` x `frames` from the most focused heads' weights
/// (each head: rows of `stride` weights, of which the first `frames` are
/// used).
pub fn attention_matrix(heads: &[Vec<f32>], rows: usize, stride: usize, frames: usize) -> Vec<f32> {
    let frames = frames.min(stride);
    let mut ranked: Vec<(f32, &Vec<f32>)> = heads
        .iter()
        .map(|w| (focus(w, rows, stride, frames), w))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let heads: Vec<&Vec<f32>> = ranked
        .into_iter()
        .take(FOCUSED_HEADS)
        .map(|(_, w)| w)
        .collect();
    let mut sum = vec![0f32; rows * frames];
    if heads.is_empty() || rows == 0 || frames == 0 {
        return sum;
    }
    let mut m = vec![0f32; rows * frames];
    for w in &heads {
        for r in 0..rows {
            m[r * frames..(r + 1) * frames].copy_from_slice(&w[r * stride..r * stride + frames]);
        }
        // Normalised per frame over the rows (mean 0, deviation 1).
        for f in 0..frames {
            let mean = (0..rows).map(|r| m[r * frames + f]).sum::<f32>() / rows as f32;
            let var = (0..rows)
                .map(|r| (m[r * frames + f] - mean).powi(2))
                .sum::<f32>()
                / rows as f32;
            let inv = 1.0 / var.sqrt().max(1e-8);
            for r in 0..rows {
                m[r * frames + f] = (m[r * frames + f] - mean) * inv;
            }
        }
        for r in 0..rows {
            let row = median_filter(&m[r * frames..(r + 1) * frames], MEDIAN_WIDTH);
            for (s, v) in sum[r * frames..(r + 1) * frames].iter_mut().zip(row) {
                *s += v;
            }
        }
    }
    let n = heads.len() as f32;
    sum.iter_mut().for_each(|v| *v /= n);
    sum
}

/// Median of each `width` neighbourhood (edges reflected).
fn median_filter(x: &[f32], width: usize) -> Vec<f32> {
    let half = width / 2;
    let n = x.len() as isize;
    let mut win = Vec::with_capacity(width);
    (0..x.len())
        .map(|i| {
            win.clear();
            for d in -(half as isize)..=half as isize {
                let mut j = i as isize + d;
                if j < 0 {
                    j = -j;
                }
                if j >= n {
                    j = 2 * (n - 1) - j;
                }
                win.push(x[j.clamp(0, n - 1) as usize]);
            }
            win.sort_by(f32::total_cmp);
            win[win.len() / 2]
        })
        .collect()
}

/// The path of least cost through `cost` (`rows` x `cols`) from the
/// top left to the bottom right, moving down, right or diagonally; for
/// each row the first column the path reaches in it.
pub fn dtw_row_starts(cost: &[f32], rows: usize, cols: usize) -> Vec<usize> {
    if rows == 0 || cols == 0 {
        return vec![0; rows];
    }
    let w = cols + 1;
    let mut acc = vec![f32::INFINITY; (rows + 1) * w];
    // 0: diagonal, 1: from above, 2: from the left.
    let mut trace = vec![0u8; (rows + 1) * w];
    acc[0] = 0.0;
    for i in 1..=rows {
        for j in 1..=cols {
            let (c0, c1, c2) = (
                acc[(i - 1) * w + j - 1],
                acc[(i - 1) * w + j],
                acc[i * w + j - 1],
            );
            let (best, t) = if c0 <= c1 && c0 <= c2 {
                (c0, 0)
            } else if c1 <= c2 {
                (c1, 1)
            } else {
                (c2, 2)
            };
            acc[i * w + j] = cost[(i - 1) * cols + j - 1] + best;
            trace[i * w + j] = t;
        }
    }
    for j in 0..=cols {
        trace[j] = 2;
    }
    for i in 0..=rows {
        trace[i * w] = 1;
    }
    let mut starts = vec![usize::MAX; rows];
    let (mut i, mut j) = (rows, cols);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let s = &mut starts[i - 1];
            *s = (*s).min(j - 1);
        }
        match trace[i * w + j] {
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    // Rows the path only crossed at the edge take the next row's start.
    let mut next = cols - 1;
    for s in starts.iter_mut().rev() {
        if *s == usize::MAX {
            *s = next;
        }
        next = *s;
    }
    starts
}

/// Frames the word before may still sound into the next one's stretch.
const TAIL: usize = 15;

/// Loudness (RMS) of each 20 ms frame of 16 kHz `samples`.
pub fn frame_energy(samples: &[f32]) -> Vec<f32> {
    samples
        .chunks(320)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect()
}

/// Moves each row's start past a quiet stretch and returns each row's
/// end: the path enters a row right when the row before ends (the
/// attention for a word already looks at the pause before it), so a word
/// after a pause would start with the pause and the word before would last
/// through it. Where the audio is clearly quieter than the window's loud
/// parts near the start of a row's stretch, the row starts after the
/// quiet part and the row before ends where it began. Only stretches
/// longer than `min_len` frames change.
pub fn refine_starts(
    energy: &[f32],
    starts: &mut [usize],
    ends_at: usize,
    min_len: usize,
) -> Vec<usize> {
    let rows = starts.len();
    let mut ends: Vec<usize> = (0..rows)
        .map(|r| starts.get(r + 1).copied().unwrap_or(ends_at))
        .collect();
    let n = energy.len().min(ends_at);
    if n == 0 {
        return ends;
    }
    let mut sorted = energy[..n].to_vec();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted[n * 9 / 10];
    let quiet = loud * 0.15;
    let heard = |f: usize| f < n && energy[f] >= quiet;
    for r in 0..rows {
        let s = starts[r];
        let e = starts.get(r + 1).copied().unwrap_or(n).min(n);
        if e <= s + min_len {
            continue;
        }
        // The end of the word before may still sound for a moment.
        let Some(q) = (s..(s + TAIL).min(e)).find(|&f| !heard(f)) else {
            continue;
        };
        let Some(on) = (q..e).find(|&f| heard(f) && heard(f + 1)) else {
            continue;
        };
        if on - q >= min_len / 2 {
            starts[r] = on;
            if r > 0 {
                ends[r - 1] = q.max(starts[r - 1]);
            }
        }
    }
    ends
}

/// Groups tokens into words: a token starting with a space (or the first)
/// begins a word, others (word pieces, punctuation) join the one before.
/// Returns (first token index, token count) per word.
pub fn word_spans(pieces: &[&[u8]]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        let starts = p.first().is_some_and(|b| *b == b' ') || out.is_empty();
        let only_punct = p.iter().all(|b| b.is_ascii_punctuation() || *b == b' ');
        if starts && !(only_punct && !out.is_empty() && !p.starts_with(b" ")) {
            out.push((i, 1));
        } else if let Some(last) = out.last_mut() {
            last.1 += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_follows_the_diagonal_of_a_clear_matrix() {
        // Three tokens said at frames 2, 5 and 8 of 10.
        let (rows, cols) = (3, 10);
        let mut cost = vec![1f32; rows * cols];
        for (r, range) in [(0, 2..5), (1, 5..8), (2, 8..10)] {
            for c in range {
                cost[r * cols + c] = -1.0;
            }
        }
        let starts = dtw_row_starts(&cost, rows, cols);
        assert_eq!(starts, vec![0, 5, 8]);
    }

    #[test]
    fn matrix_is_normalised_and_smoothed() {
        // Two rows, each attending to its own half, one noisy spike.
        let stride = 20;
        let mut w = vec![0f32; 2 * stride];
        for f in 0..10 {
            w[f] = 0.9;
        }
        for f in 10..20 {
            w[stride + f] = 0.9;
        }
        w[15] = 0.9; // a single-frame spike in row 0
        let m = attention_matrix(&[w], 2, stride, 20);
        assert!(m[3] > m[20 + 3], "row 0 leads early on");
        assert!(m[20 + 15] > m[15], "the spike is filtered out");
    }

    #[test]
    fn tokens_group_into_words() {
        let p: Vec<&[u8]> = vec![b" Hel", b"lo", b",", b" world", b"!", b" F", b"-", b"R"];
        assert_eq!(word_spans(&p), vec![(0, 3), (3, 2), (5, 3)]);
    }
}
