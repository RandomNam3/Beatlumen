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

/// `x` (`n` rows of `w.cols`) times `w` transposed, plus `bias`:
/// `n` rows of `w.rows`.
pub fn linear(x: &[f32], n: usize, w: &Tensor, bias: Option<&Tensor>) -> Vec<f32> {
    let (k, m) = (w.cols, w.rows);
    debug_assert_eq!(x.len(), n * k);
    let mut out = vec![0f32; n * m];
    let work = n * m * k;
    let threads = if work < 1 << 18 { 1 } else { threads() };
    // Split over output columns when there are few rows (decoding one
    // token), else over rows.
    if n < threads {
        let chunk = m.div_ceil(threads);
        let cols: Vec<(usize, Vec<f32>)> = std::thread::scope(|s| {
            (0..threads)
                .map(|t| {
                    s.spawn(move || {
                        let (c0, c1) = (t * chunk, ((t + 1) * chunk).min(m));
                        let mut part = vec![0f32; n * c1.saturating_sub(c0)];
                        for i in 0..n {
                            let xi = &x[i * k..(i + 1) * k];
                            for (j, o) in (c0..c1).enumerate() {
                                part[i * (c1 - c0) + j] = dot(xi, w.row(o));
                            }
                        }
                        (c0, part)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().expect("matmul thread"))
                .collect()
        });
        for (c0, part) in cols {
            let w = part.len() / n.max(1);
            for i in 0..n {
                out[i * m + c0..i * m + c0 + w].copy_from_slice(&part[i * w..(i + 1) * w]);
            }
        }
    } else {
        let rows_per = n.div_ceil(threads);
        std::thread::scope(|s| {
            for (t, chunk) in out.chunks_mut(rows_per * m).enumerate() {
                s.spawn(move || {
                    for (ri, orow) in chunk.chunks_mut(m).enumerate() {
                        let i = t * rows_per + ri;
                        let xi = &x[i * k..(i + 1) * k];
                        // Blocks of output columns keep weight rows in cache.
                        for (o, v) in orow.iter_mut().enumerate() {
                            *v = dot(xi, w.row(o));
                        }
                    }
                });
            }
        });
    }
    if let Some(b) = bias {
        for row in out.chunks_mut(m) {
            for (v, bb) in row.iter_mut().zip(&b.data) {
                *v += bb;
            }
        }
    }
    out
}

pub fn layer_norm(x: &mut [f32], dim: usize, w: &Tensor, b: &Tensor) {
    for row in x.chunks_mut(dim) {
        let mean = row.iter().sum::<f32>() / dim as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / dim as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        for (i, v) in row.iter_mut().enumerate() {
            *v = (*v - mean) * inv * w.data[i] + b.data[i];
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
