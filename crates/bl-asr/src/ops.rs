//! Numerical kernels: matrix products spread over all cores, layer norm,
//! GELU, softmax.

use crate::model::Tensor;

static MAX_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Limits the threads used for the computations (0 = all cores).
pub fn set_max_threads(n: usize) {
    MAX_THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
}

pub fn threads() -> usize {
    let all = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 32);
    match MAX_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => all,
        n => n.min(all),
    }
}

/// Dot product with several accumulators (vectorises well).
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0f32; 8];
    let chunks = n / 8;
    for i in 0..chunks {
        let (x, y) = (&a[i * 8..i * 8 + 8], &b[i * 8..i * 8 + 8]);
        for j in 0..8 {
            acc[j] += x[j] * y[j];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * 8..n {
        s += a[i] * b[i];
    }
    s
}

/// Weight rows expanded at a time (and used for all input rows).
const BLOCK: usize = 16;

/// `x` (`n` rows of `w.cols`) times `w` transposed, plus `bias`:
/// `n` rows of `w.rows`. The output columns are split over the threads;
/// each expands its weight rows (f16 or quantised) in blocks once and
/// uses them for every input row.
pub fn linear(x: &[f32], n: usize, w: &Tensor, bias: Option<&Tensor>) -> Vec<f32> {
    let (k, m) = (w.cols, w.rows);
    debug_assert_eq!(x.len(), n * k);
    let work = n * m * k;
    let threads = if work < 1 << 18 {
        1
    } else {
        threads().min(m.div_ceil(BLOCK))
    };
    let chunk = m.div_ceil(threads).next_multiple_of(BLOCK);
    let column_range = |c0: usize, c1: usize| -> Vec<f32> {
        let width = c1 - c0;
        let mut part = vec![0f32; n * width];
        let mut buf = vec![0f32; BLOCK * k];
        for b0 in (c0..c1).step_by(BLOCK) {
            let b1 = (b0 + BLOCK).min(c1);
            let rows: &[f32] = match w.as_f32() {
                Some(all) => &all[b0 * k..b1 * k],
                None => {
                    for (r, out) in (b0..b1).zip(buf.chunks_exact_mut(k)) {
                        w.row_into(r, out);
                    }
                    &buf[..(b1 - b0) * k]
                }
            };
            for i in 0..n {
                let xi = &x[i * k..(i + 1) * k];
                let o = &mut part[i * width + (b0 - c0)..i * width + (b1 - c0)];
                for (v, wr) in o.iter_mut().zip(rows.chunks_exact(k)) {
                    *v = dot(xi, wr);
                }
            }
        }
        part
    };
    let parts: Vec<(usize, Vec<f32>)> = if threads == 1 {
        vec![(0, column_range(0, m))]
    } else {
        std::thread::scope(|s| {
            (0..threads)
                .filter_map(|t| {
                    let (c0, c1) = (t * chunk, ((t + 1) * chunk).min(m));
                    (c0 < c1).then(|| (c0, s.spawn(move || column_range(c0, c1))))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(c0, h)| (c0, h.join().expect("matmul thread")))
                .collect()
        })
    };
    let mut out = vec![0f32; n * m];
    for (c0, part) in parts {
        let width = part.len() / n.max(1);
        for i in 0..n {
            out[i * m + c0..i * m + c0 + width].copy_from_slice(&part[i * width..(i + 1) * width]);
        }
    }
    if let Some(b) = bias {
        for row in out.chunks_mut(m) {
            for (v, bb) in row.iter_mut().zip(b.values()) {
                *v += bb;
            }
        }
    }
    out
}

pub fn layer_norm(x: &mut [f32], dim: usize, w: &Tensor, b: &Tensor) {
    let (w, b) = (w.values(), b.values());
    for row in x.chunks_mut(dim) {
        let mean = row.iter().sum::<f32>() / dim as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / dim as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        for (i, v) in row.iter_mut().enumerate() {
            *v = (*v - mean) * inv * w[i] + b[i];
        }
    }
}

pub fn gelu(x: &mut [f32]) {
    const C: f32 = 0.797_884_6; // sqrt(2 / pi)
    for v in x {
        let u = *v;
        *v = 0.5 * u * (1.0 + (C * (u + 0.044_715 * u * u * u)).tanh());
    }
}

pub fn softmax(x: &mut [f32]) {
    let max = x.iter().copied().fold(f32::MIN, f32::max);
    let mut sum = 0.0;
    for v in x.iter_mut() {
        *v = (*v - max).exp();
        sum += *v;
    }
    let inv = 1.0 / sum;
    for v in x.iter_mut() {
        *v *= inv;
    }
}
