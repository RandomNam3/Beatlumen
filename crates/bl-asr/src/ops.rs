//! Numerical kernels: matrix products spread over all cores, layer norm,
//! GELU, softmax.
//!
//! The inner loops come in versions for the CPU's vector instructions,
//! chosen once at run time: AVX2 with FMA and F16C on x86-64, NEON on
//! aarch64, plain Rust elsewhere (and as the reference the tests compare
//! the others with). The vector versions need `unsafe` only to call the
//! CPU's instructions; they read and write memory through bounds-checked
//! slices, so a wrong size panics instead of touching other memory.

use crate::model::{Packed, Tensor, QK};
use std::sync::OnceLock;

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

/// The kernels to use: the vector instructions this CPU has.
#[derive(Clone, Copy, Debug)]
enum Simd {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Avx2(x86::Avx2),
    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    Neon(neon::Neon),
}

/// The best kernels for this CPU (asked once).
fn simd() -> Simd {
    static SIMD: OnceLock<Simd> = OnceLock::new();
    *SIMD.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if let Some(t) = x86::Avx2::detect() {
            return Simd::Avx2(t);
        }
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        if let Some(t) = neon::Neon::detect() {
            return Simd::Neon(t);
        }
        Simd::Scalar
    })
}

/// Calls kernel `f` of the chosen kernel set.
macro_rules! dispatch {
    ($simd:expr, $f:ident($($a:expr),* $(,)?)) => {
        match $simd {
            #[cfg(target_arch = "x86_64")]
            Simd::Avx2(t) => t.$f($($a),*),
            #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
            Simd::Neon(t) => t.$f($($a),*),
            Simd::Scalar => scalar::$f($($a),*),
        }
    };
}

/// Codes of the quantised formats, for kernels compiled per format.
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_feature = "neon")
))]
mod format {
    pub const Q4_0: u8 = 0;
    pub const Q4_1: u8 = 1;
    pub const Q5_0: u8 = 2;
    pub const Q5_1: u8 = 3;
    pub const Q8_0: u8 = 4;
}

/// Calls `f::<FORMAT, ...>(args)` with the code of the format `t`.
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_feature = "neon")
))]
macro_rules! per_format {
    ($t:expr, $f:ident::<_ $(, $g:ident)*>($($a:expr),* $(,)?)) => {
        match $t {
            QType::Q4_0 => $f::<{ format::Q4_0 } $(, $g)*>($($a),*),
            QType::Q4_1 => $f::<{ format::Q4_1 } $(, $g)*>($($a),*),
            QType::Q5_0 => $f::<{ format::Q5_0 } $(, $g)*>($($a),*),
            QType::Q5_1 => $f::<{ format::Q5_1 } $(, $g)*>($($a),*),
            QType::Q8_0 => $f::<{ format::Q8_0 } $(, $g)*>($($a),*),
        }
    };
}

/// Dot product (of the common length).
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    dispatch!(simd(), dot(&a[..n], &b[..n]))
}

/// `y += a * x` (over the common length).
pub fn axpy(a: f32, x: &[f32], y: &mut [f32]) {
    let n = x.len().min(y.len());
    dispatch!(simd(), axpy(a, &x[..n], &mut y[..n]))
}

/// Writes the stored rows `p` as f32 values into `out` (exactly as many).
pub(crate) fn expand(p: Packed, out: &mut [f32]) {
    expand_with(simd(), p, out);
}

fn expand_with(simd: Simd, p: Packed, out: &mut [f32]) {
    match p {
        Packed::F32(v) => out.copy_from_slice(v),
        Packed::F16(v) => {
            assert_eq!(v.len(), out.len(), "row length");
            dispatch!(simd, expand_f16(v, out))
        }
        Packed::Quant(t, v) => {
            let blocks = v.len() / t.block_bytes();
            assert!(
                v.len() % t.block_bytes() == 0 && blocks * QK == out.len(),
                "row length"
            );
            dispatch!(simd, expand_quant(t, v, out))
        }
    }
}

/// Input rows up to which `linear` multiplies straight from the stored
/// weights (decoder steps); more rows share expanded f32 weights.
const FUSED_ROWS: usize = 4;

/// Input rows and weight rows multiplied together by the tiled kernel
/// (each loaded vector is used 4 or 3 times; 12 accumulators fit in the
/// 16 registers of AVX2).
const TILE_X: usize = 3;
const TILE_W: usize = 4;

/// Bytes of weights expanded to f32 at a time (stays in the L2 cache
/// while all input rows pass by).
const BLOCK_BYTES: usize = 96 * 1024;

/// Multiply-adds below which `linear` stays on the calling thread.
const THREAD_WORK: usize = 1 << 20;

/// `x` (`n` rows of `w.cols`) times `w` transposed, plus `bias`:
/// `n` rows of `w.rows`. The output columns are split over the threads.
/// For a few input rows the dots are computed straight from the stored
/// weights (f16 or quantised blocks expanded in registers); for more,
/// each thread expands blocks of weight rows to f32 once and multiplies
/// them with tiles of input rows.
pub fn linear(x: &[f32], n: usize, w: &Tensor, bias: Option<&Tensor>) -> Vec<f32> {
    let (k, m) = (w.cols, w.rows);
    debug_assert_eq!(x.len(), n * k);
    if n == 0 {
        return Vec::new();
    }
    let simd = simd();
    let fused = n <= FUSED_ROWS && !matches!(w.packed(0, 0), Packed::F32(_));
    let work = n * m * k;
    let threads = if work < THREAD_WORK {
        1
    } else {
        threads().min(m.div_ceil(16))
    };
    let chunk = m.div_ceil(threads).next_multiple_of(16);
    let column_range = |c0: usize, c1: usize| -> Vec<f32> {
        let mut part = vec![0f32; n * (c1 - c0)];
        match (fused, n) {
            (true, 1) => fused_rows::<1>(simd, x, w, c0, c1, &mut part),
            (true, 2) => fused_rows::<2>(simd, x, w, c0, c1, &mut part),
            (true, 3) => fused_rows::<3>(simd, x, w, c0, c1, &mut part),
            (true, _) => fused_rows::<4>(simd, x, w, c0, c1, &mut part),
            (false, _) => tiled(simd, x, n, w, c0, c1, &mut part),
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
        let width = part.len() / n;
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

/// `part` (`N` rows of `c1 - c0`) = the `N` rows of `x` times the weight
/// rows `c0..c1` (stored as f16 or quantised) transposed, each weight row
/// read once for all input rows.
fn fused_rows<const N: usize>(
    simd: Simd,
    x: &[f32],
    w: &Tensor,
    c0: usize,
    c1: usize,
    part: &mut [f32],
) {
    let k = w.cols;
    let width = c1 - c0;
    let xs: [&[f32]; N] = std::array::from_fn(|i| &x[i * k..(i + 1) * k]);
    for r in c0..c1 {
        let d = match w.packed(r, r + 1) {
            Packed::F16(row) => dispatch!(simd, half_dots(row, &xs)),
            Packed::Quant(t, row) => dispatch!(simd, quant_dots(t, row, &xs)),
            Packed::F32(row) => xs.map(|xi| dispatch!(simd, dot(xi, row))),
        };
        for (i, v) in d.into_iter().enumerate() {
            part[i * width + r - c0] = v;
        }
    }
}

/// `part` (`n` rows of `c1 - c0`) = `x` times the weight rows `c0..c1`
/// transposed: blocks of weight rows are expanded to f32 (unless stored
/// so) and multiplied with tiles of input rows.
fn tiled(simd: Simd, x: &[f32], n: usize, w: &Tensor, c0: usize, c1: usize, part: &mut [f32]) {
    let k = w.cols;
    let width = c1 - c0;
    if k == 0 {
        return;
    }
    let block = (BLOCK_BYTES / (4 * k)).clamp(TILE_W, 64) / TILE_W * TILE_W;
    let mut buf = Vec::new();
    for b0 in (c0..c1).step_by(block) {
        let b1 = (b0 + block).min(c1);
        let rows: &[f32] = match w.packed(b0, b1) {
            Packed::F32(v) => v,
            p => {
                buf.resize((b1 - b0) * k, 0.0);
                expand_with(simd, p, &mut buf);
                &buf
            }
        };
        let mut i = 0;
        while i < n {
            let out = &mut part[i * width..];
            if n - i >= TILE_X {
                let xs: [&[f32]; TILE_X] =
                    std::array::from_fn(|r| &x[(i + r) * k..(i + r + 1) * k]);
                tile_rows(simd, &xs, rows, k, out, width, b0 - c0);
                i += TILE_X;
            } else {
                tile_rows(
                    simd,
                    &[&x[i * k..(i + 1) * k]],
                    rows,
                    k,
                    out,
                    width,
                    b0 - c0,
                );
                i += 1;
            }
        }
    }
}

/// Dots of the input rows `xs` with each weight row in `rows` (`k` long),
/// written to columns `col..` of the rows of `out` (`width` apart).
fn tile_rows<const R: usize>(
    simd: Simd,
    xs: &[&[f32]; R],
    rows: &[f32],
    k: usize,
    out: &mut [f32],
    width: usize,
    col: usize,
) {
    let count = rows.len() / k;
    let row = |j: usize| &rows[j * k..(j + 1) * k];
    let mut j = 0;
    while j < count {
        if count - j >= TILE_W {
            let ws: [&[f32]; TILE_W] = std::array::from_fn(|c| row(j + c));
            let t = dispatch!(simd, tile(xs, &ws));
            for (r, t) in t.iter().enumerate() {
                out[r * width + col + j..][..TILE_W].copy_from_slice(t);
            }
            j += TILE_W;
        } else {
            let t = dispatch!(simd, tile(xs, &[row(j)]));
            for (r, t) in t.iter().enumerate() {
                out[r * width + col + j] = t[0];
            }
            j += 1;
        }
    }
}

pub fn layer_norm(x: &mut [f32], dim: usize, w: &Tensor, b: &Tensor) {
    let (w, b) = (w.values(), b.values());
    dispatch!(simd(), layer_norm(x, dim, w, b))
}

/// GELU with the tanh approximation, computed as
/// `0.5 u (1 + tanh z) = u / (1 + e^(-2z))`.
pub fn gelu(x: &mut [f32]) {
    dispatch!(simd(), gelu(x))
}

pub fn softmax(x: &mut [f32]) {
    dispatch!(simd(), softmax(x))
}

/// The first `k` values of each slice (panics if one is shorter). Inlined,
/// so the compiler knows the lengths and drops later bounds checks.
#[cfg(any(
    target_arch = "x86_64",
    all(target_arch = "aarch64", target_feature = "neon")
))]
#[inline(always)]
fn prefixes<'a, const N: usize>(s: &[&'a [f32]; N], k: usize) -> [&'a [f32]; N] {
    let mut out: [&[f32]; N] = [&[]; N];
    for (o, s) in out.iter_mut().zip(s) {
        *o = &s[..k];
    }
    out
}

// Element-wise loops written so the compiler vectorises them: no branches,
// and sums kept in lanes. Inlined into the kernel sets (with AVX2 for x86).

/// e^x, within 2 ulp (Cephes' single precision polynomial); 0 below
/// -87.3, and e^88 at most.
#[inline(always)]
fn exp_lane(x: f32) -> f32 {
    // Adding 1.5 * 2^23 rounds to an integer, kept in the low bits.
    const ROUND: f32 = 12_582_912.0;
    let c = x.clamp(-87.3, 88.0);
    let t = c * std::f32::consts::LOG2_E + ROUND;
    let n = t - ROUND;
    // c - n ln 2, with ln 2 in two parts for precision.
    let r = c - n * 0.693_359_4 + n * 2.121_944_4e-4;
    let p = 1.987_569_1e-4;
    let p = p * r + 1.398_199_9e-3;
    let p = p * r + 8.333_452e-3;
    let p = p * r + 4.166_579_6e-2;
    let p = p * r + 0.166_666_65;
    let p = p * r + 0.5;
    let e = p * r * r + r + 1.0;
    // 2^n from the integer in `t`'s low bits.
    let scale = f32::from_bits(((t.to_bits() as i32 - ROUND.to_bits() as i32 + 127) << 23) as u32);
    if x < -87.3 {
        0.0
    } else {
        e * scale
    }
}

/// Lanes for the sums.
const LANES: usize = 16;

#[inline(always)]
fn gelu_lanes(x: &mut [f32]) {
    const C: f32 = 0.797_884_6; // sqrt(2 / pi)
    for v in x {
        let u = *v;
        *v = u / (1.0 + exp_lane(-2.0 * C * (u + 0.044_715 * u * u * u)));
    }
}

#[inline(always)]
fn softmax_lanes(x: &mut [f32]) {
    let mut max = [f32::MIN; LANES];
    let mut chunks = x.chunks_exact(LANES);
    for c in &mut chunks {
        for (m, &v) in max.iter_mut().zip(c) {
            *m = m.max(v);
        }
    }
    let max = chunks
        .remainder()
        .iter()
        .chain(&max)
        .copied()
        .fold(f32::MIN, f32::max);
    let mut sum = [0f32; LANES];
    let mut chunks = x.chunks_exact_mut(LANES);
    for c in &mut chunks {
        for (s, v) in sum.iter_mut().zip(c) {
            *v = exp_lane(*v - max);
            *s += *v;
        }
    }
    let mut total: f32 = sum.iter().sum();
    for v in chunks.into_remainder() {
        *v = exp_lane(*v - max);
        total += *v;
    }
    let inv = 1.0 / total;
    for v in x.iter_mut() {
        *v *= inv;
    }
}

#[inline(always)]
fn sum_lanes(x: &[f32], mean: f32, square: bool) -> f32 {
    let mut acc = [0f32; LANES];
    let mut chunks = x.chunks_exact(LANES);
    for c in &mut chunks {
        for (a, &v) in acc.iter_mut().zip(c) {
            let d = v - mean;
            *a += if square { d * d } else { d };
        }
    }
    let mut s: f32 = acc.iter().sum();
    for &v in chunks.remainder() {
        let d = v - mean;
        s += if square { d * d } else { d };
    }
    s
}

#[inline(always)]
fn layer_norm_lanes(x: &mut [f32], dim: usize, w: &[f32], b: &[f32]) {
    let (w, b) = (&w[..dim], &b[..dim]);
    for row in x.chunks_mut(dim) {
        let mean = sum_lanes(row, 0.0, false) / dim as f32;
        let var = sum_lanes(row, mean, true) / dim as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        for ((v, &w), &b) in row.iter_mut().zip(w).zip(b) {
            *v = (*v - mean) * inv * w + b;
        }
    }
}

/// The plain kernels (any CPU; the reference for the tests).
mod scalar {
    use crate::model::{f16_to_f32, QType, QK};

    /// Dot product with several accumulators (vectorises a little).
    pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
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

    pub(super) fn axpy(a: f32, x: &[f32], y: &mut [f32]) {
        for (v, u) in y.iter_mut().zip(x) {
            *v += a * u;
        }
    }

    pub(super) fn expand_f16(src: &[u16], out: &mut [f32]) {
        for (o, &h) in out.iter_mut().zip(src) {
            *o = f16_to_f32(h);
        }
    }

    pub(super) fn expand_quant(t: QType, src: &[u8], out: &mut [f32]) {
        for (b, o) in src
            .chunks_exact(t.block_bytes())
            .zip(out.chunks_exact_mut(QK))
        {
            t.dequantize(b, o);
        }
    }

    /// Dots of the f16 `row` with each of `xs`, 32 values at a time.
    pub(super) fn half_dots<const N: usize>(row: &[u16], xs: &[&[f32]; N]) -> [f32; N] {
        let mut out = [0f32; N];
        let mut buf = [0f32; QK];
        for (c, h) in row.chunks(QK).enumerate() {
            let w = &mut buf[..h.len()];
            expand_f16(h, w);
            for (o, x) in out.iter_mut().zip(xs) {
                *o += dot(w, &x[c * QK..c * QK + h.len()]);
            }
        }
        out
    }

    /// Dots of the quantised `row` with each of `xs`, block by block.
    pub(super) fn quant_dots<const N: usize>(t: QType, row: &[u8], xs: &[&[f32]; N]) -> [f32; N] {
        let mut out = [0f32; N];
        let mut w = [0f32; QK];
        for (c, b) in row.chunks_exact(t.block_bytes()).enumerate() {
            t.dequantize(b, &mut w);
            for (o, x) in out.iter_mut().zip(xs) {
                *o += dot(&w, &x[c * QK..(c + 1) * QK]);
            }
        }
        out
    }

    pub(super) fn tile<const R: usize, const C: usize>(
        xs: &[&[f32]; R],
        ws: &[&[f32]; C],
    ) -> [[f32; C]; R] {
        let mut out = [[0f32; C]; R];
        for (o, x) in out.iter_mut().zip(xs) {
            for (v, w) in o.iter_mut().zip(ws) {
                *v = dot(x, w);
            }
        }
        out
    }

    pub(super) fn gelu(x: &mut [f32]) {
        super::gelu_lanes(x)
    }

    pub(super) fn softmax(x: &mut [f32]) {
        super::softmax_lanes(x)
    }

    pub(super) fn layer_norm(x: &mut [f32], dim: usize, w: &[f32], b: &[f32]) {
        super::layer_norm_lanes(x, dim, w, b)
    }
}

/// Kernels for x86-64 CPUs with AVX2, FMA and F16C (Intel since 2013,
/// AMD since 2015): 8 lanes.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::format::{Q4_0, Q4_1, Q5_0, Q5_1, Q8_0};
    use super::{format, prefixes};
    use crate::model::{f16_to_f32, QType, QK};
    use std::arch::x86_64::*;

    /// Proof that the CPU has AVX2, FMA and F16C: only `detect` makes
    /// one, after asking the CPU. Its methods are the safe entry points
    /// to the kernels below, which are compiled for those features.
    #[derive(Clone, Copy, Debug)]
    pub(in crate::ops) struct Avx2(());

    impl Avx2 {
        pub(in crate::ops) fn detect() -> Option<Avx2> {
            let ok = is_x86_feature_detected!("avx2")
                && is_x86_feature_detected!("fma")
                && is_x86_feature_detected!("f16c");
            ok.then_some(Avx2(()))
        }

        // The SAFETY argument for every call below: `self` exists only on
        // CPUs with the features the kernel is compiled for, and the
        // kernels access memory only through bounds-checked slices.

        pub(in crate::ops) fn dot(self, a: &[f32], b: &[f32]) -> f32 {
            // SAFETY: the CPU has the features (see above).
            unsafe { dot(a, b) }
        }

        pub(in crate::ops) fn axpy(self, a: f32, x: &[f32], y: &mut [f32]) {
            // SAFETY: the CPU has the features (see above).
            unsafe { axpy(a, x, y) }
        }

        pub(in crate::ops) fn expand_f16(self, src: &[u16], out: &mut [f32]) {
            // SAFETY: the CPU has the features (see above).
            unsafe { expand_f16(src, out) }
        }

        pub(in crate::ops) fn expand_quant(self, t: QType, src: &[u8], out: &mut [f32]) {
            let bb = t.block_bytes();
            // SAFETY: the CPU has the features (see above).
            unsafe { per_format!(t, expand_quant::<_>(src, bb, out)) }
        }

        pub(in crate::ops) fn half_dots<const N: usize>(
            self,
            row: &[u16],
            xs: &[&[f32]; N],
        ) -> [f32; N] {
            // SAFETY: the CPU has the features (see above).
            unsafe { half_dots(row, xs) }
        }

        pub(in crate::ops) fn quant_dots<const N: usize>(
            self,
            t: QType,
            row: &[u8],
            xs: &[&[f32]; N],
        ) -> [f32; N] {
            let bb = t.block_bytes();
            // SAFETY: the CPU has the features (see above).
            unsafe { per_format!(t, quant_dots::<_, N>(row, bb, xs)) }
        }

        pub(in crate::ops) fn tile<const R: usize, const C: usize>(
            self,
            xs: &[&[f32]; R],
            ws: &[&[f32]; C],
        ) -> [[f32; C]; R] {
            // SAFETY: the CPU has the features (see above).
            unsafe { tile(xs, ws) }
        }

        pub(in crate::ops) fn gelu(self, x: &mut [f32]) {
            // SAFETY: the CPU has the features (see above).
            unsafe { gelu(x) }
        }

        pub(in crate::ops) fn softmax(self, x: &mut [f32]) {
            // SAFETY: the CPU has the features (see above).
            unsafe { softmax(x) }
        }

        pub(in crate::ops) fn layer_norm(self, x: &mut [f32], dim: usize, w: &[f32], b: &[f32]) {
            // SAFETY: the CPU has the features (see above).
            unsafe { layer_norm(x, dim, w, b) }
        }
    }

    /// `s[..8]` (panics if shorter).
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn load(s: &[f32]) -> __m256 {
        let s = &s[..8];
        // SAFETY: `s` has 8 readable values; the load needs no alignment.
        unsafe { _mm256_loadu_ps(s.as_ptr()) }
    }

    /// Writes `v` to `s[..8]` (panics if shorter).
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn store(s: &mut [f32], v: __m256) {
        let s = &mut s[..8];
        // SAFETY: `s` has 8 writable values; the store needs no alignment.
        unsafe { _mm256_storeu_ps(s.as_mut_ptr(), v) }
    }

    /// The half floats `s[..8]` as f32.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn load_f16(s: &[u16]) -> __m256 {
        let s = &s[..8];
        // SAFETY: `s` has 16 readable bytes; the load needs no alignment.
        let h = unsafe { _mm_loadu_si128(s.as_ptr().cast()) };
        _mm256_cvtph_ps(h)
    }

    /// The bytes `s[..16]`.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn load_bytes(s: &[u8]) -> __m128i {
        let s = &s[..16];
        // SAFETY: `s` has 16 readable bytes; the load needs no alignment.
        unsafe { _mm_loadu_si128(s.as_ptr().cast()) }
    }

    /// Sum of the lanes.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn hsum(v: __m256) -> f32 {
        let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        _mm_cvtss_f32(_mm_add_ss(s, _mm_movehdup_ps(s)))
    }

    /// The half float in `b[at..at + 2]`, in all lanes.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn half(b: &[u8], at: usize) -> __m256 {
        let h = u16::from_le_bytes([b[at], b[at + 1]]);
        _mm256_broadcastss_ps(_mm_cvtph_ps(_mm_cvtsi32_si128(h as i32)))
    }

    /// The low 8 bytes of `v` (unsigned) as f32.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn u8_to_f32(v: __m128i) -> __m256 {
        _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(v))
    }

    /// The low 8 bytes of `v` (signed) as f32.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn i8_to_f32(v: __m128i) -> __m256 {
        _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(v))
    }

    /// Bit `i` of `qh` as 0x10 in byte `i`: the fifth bits of the values
    /// of a q5 block.
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn fifth_bits(qh: u32) -> __m256i {
        // Byte i gets byte i / 8 of `qh`, then is tested for bit i % 8.
        let spread = _mm256_shuffle_epi8(
            _mm256_set1_epi32(qh as i32),
            _mm256_set_epi64x(
                0x0303_0303_0303_0303,
                0x0202_0202_0202_0202,
                0x0101_0101_0101_0101,
                0,
            ),
        );
        let bit = _mm256_set1_epi64x(0x8040_2010_0804_0201_u64 as i64);
        let set = _mm256_cmpeq_epi8(_mm256_and_si256(spread, bit), bit);
        _mm256_and_si256(set, _mm256_set1_epi8(0x10))
    }

    /// The 32 values of block `b` in format `T` (layouts as in
    /// `QType::dequantize`).
    #[inline]
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn block<const T: u8>(b: &[u8]) -> [__m256; 4] {
        let d = half(b, 0);
        if T == Q8_0 {
            let lo = load_bytes(&b[2..18]);
            let hi = load_bytes(&b[18..34]);
            return [
                _mm256_mul_ps(i8_to_f32(lo), d),
                _mm256_mul_ps(i8_to_f32(_mm_srli_si128(lo, 8)), d),
                _mm256_mul_ps(i8_to_f32(hi), d),
                _mm256_mul_ps(i8_to_f32(_mm_srli_si128(hi, 8)), d),
            ];
        }
        // The 16 nibble bytes follow d, the minimum (q4_1, q5_1) and the
        // fifth bits (q5).
        let at = match T {
            Q4_0 => 2,
            Q4_1 => 4,
            Q5_0 => 6,
            _ => 8,
        };
        let min = match T {
            Q4_0 => _mm256_mul_ps(d, _mm256_set1_ps(-8.0)),
            Q5_0 => _mm256_mul_ps(d, _mm256_set1_ps(-16.0)),
            _ => half(b, 2),
        };
        let q = load_bytes(&b[at..at + 16]);
        let nibble = _mm_set1_epi8(0x0f);
        let mut lo = _mm_and_si128(q, nibble);
        let mut hi = _mm_and_si128(_mm_srli_epi16(q, 4), nibble);
        if T == Q5_0 || T == Q5_1 {
            let qh = u32::from_le_bytes([b[at - 4], b[at - 3], b[at - 2], b[at - 1]]);
            let bits = fifth_bits(qh);
            lo = _mm_or_si128(lo, _mm256_castsi256_si128(bits));
            hi = _mm_or_si128(hi, _mm256_extracti128_si256(bits, 1));
        }
        [
            _mm256_fmadd_ps(u8_to_f32(lo), d, min),
            _mm256_fmadd_ps(u8_to_f32(_mm_srli_si128(lo, 8)), d, min),
            _mm256_fmadd_ps(u8_to_f32(hi), d, min),
            _mm256_fmadd_ps(u8_to_f32(_mm_srli_si128(hi, 8)), d, min),
        ]
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [_mm256_setzero_ps(); 4];
        let (mut ca, mut cb) = (a.chunks_exact(32), b.chunks_exact(32));
        for (x, y) in (&mut ca).zip(&mut cb) {
            for (j, acc) in acc.iter_mut().enumerate() {
                *acc = _mm256_fmadd_ps(load(&x[j * 8..]), load(&y[j * 8..]), *acc);
            }
        }
        let (mut ca, mut cb) = (
            ca.remainder().chunks_exact(8),
            cb.remainder().chunks_exact(8),
        );
        for (x, y) in (&mut ca).zip(&mut cb) {
            acc[0] = _mm256_fmadd_ps(load(x), load(y), acc[0]);
        }
        let sum = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
        let mut s = hsum(sum);
        for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
            s += x * y;
        }
        s
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn axpy(a: f32, x: &[f32], y: &mut [f32]) {
        let va = _mm256_set1_ps(a);
        let (mut cx, mut cy) = (x.chunks_exact(8), y.chunks_exact_mut(8));
        for (x, y) in (&mut cx).zip(&mut cy) {
            store(y, _mm256_fmadd_ps(va, load(x), load(y)));
        }
        for (x, y) in cx.remainder().iter().zip(cy.into_remainder()) {
            *y += a * x;
        }
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn expand_f16(src: &[u16], out: &mut [f32]) {
        let (mut cs, mut co) = (src.chunks_exact(8), out.chunks_exact_mut(8));
        for (s, o) in (&mut cs).zip(&mut co) {
            store(o, load_f16(s));
        }
        for (&h, o) in cs.remainder().iter().zip(co.into_remainder()) {
            *o = f16_to_f32(h);
        }
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn expand_quant<const T: u8>(src: &[u8], bb: usize, out: &mut [f32]) {
        for (b, o) in src.chunks_exact(bb).zip(out.chunks_exact_mut(QK)) {
            let v = block::<T>(b);
            for (j, v) in v.into_iter().enumerate() {
                store(&mut o[j * 8..], v);
            }
        }
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn half_dots<const N: usize>(row: &[u16], xs: &[&[f32]; N]) -> [f32; N] {
        let k = row.len();
        let xs = prefixes(xs, k);
        // Two accumulators per input row hide the latency of the FMAs.
        let mut acc = [[_mm256_setzero_ps(); 2]; N];
        let mut p = 0;
        while p + 16 <= k {
            let w0 = load_f16(&row[p..]);
            let w1 = load_f16(&row[p + 8..]);
            for (acc, x) in acc.iter_mut().zip(&xs) {
                acc[0] = _mm256_fmadd_ps(w0, load(&x[p..]), acc[0]);
                acc[1] = _mm256_fmadd_ps(w1, load(&x[p + 8..]), acc[1]);
            }
            p += 16;
        }
        if p + 8 <= k {
            let w0 = load_f16(&row[p..]);
            for (acc, x) in acc.iter_mut().zip(&xs) {
                acc[0] = _mm256_fmadd_ps(w0, load(&x[p..]), acc[0]);
            }
            p += 8;
        }
        let mut out = [0f32; N];
        for ((o, acc), x) in out.iter_mut().zip(&acc).zip(&xs) {
            *o = hsum(_mm256_add_ps(acc[0], acc[1]));
            for (&h, v) in row[p..].iter().zip(&x[p..]) {
                *o += f16_to_f32(h) * v;
            }
        }
        out
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn quant_dots<const T: u8, const N: usize>(
        row: &[u8],
        bb: usize,
        xs: &[&[f32]; N],
    ) -> [f32; N] {
        let mut acc = [[_mm256_setzero_ps(); 2]; N];
        for (c, b) in row.chunks_exact(bb).enumerate() {
            let w = block::<T>(b);
            for (acc, x) in acc.iter_mut().zip(xs) {
                let x = &x[c * QK..(c + 1) * QK];
                acc[0] = _mm256_fmadd_ps(w[0], load(x), acc[0]);
                acc[1] = _mm256_fmadd_ps(w[1], load(&x[8..]), acc[1]);
                acc[0] = _mm256_fmadd_ps(w[2], load(&x[16..]), acc[0]);
                acc[1] = _mm256_fmadd_ps(w[3], load(&x[24..]), acc[1]);
            }
        }
        let mut out = [0f32; N];
        for (o, acc) in out.iter_mut().zip(&acc) {
            *o = hsum(_mm256_add_ps(acc[0], acc[1]));
        }
        out
    }

    /// All `R` x `C` dots of `xs` and `ws` (of the length of `xs[0]`),
    /// each loaded vector used `C` or `R` times.
    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn tile<const R: usize, const C: usize>(
        xs: &[&[f32]; R],
        ws: &[&[f32]; C],
    ) -> [[f32; C]; R] {
        let k = xs.first().map_or(0, |x| x.len());
        let (xs, ws) = (prefixes(xs, k), prefixes(ws, k));
        let mut acc = [[_mm256_setzero_ps(); C]; R];
        let mut p = 0;
        while p + 8 <= k {
            let mut xv = [_mm256_setzero_ps(); R];
            for (v, x) in xv.iter_mut().zip(&xs) {
                *v = load(&x[p..]);
            }
            for (j, w) in ws.iter().enumerate() {
                let wv = load(&w[p..]);
                for (acc, &xv) in acc.iter_mut().zip(&xv) {
                    acc[j] = _mm256_fmadd_ps(xv, wv, acc[j]);
                }
            }
            p += 8;
        }
        let mut out = [[0f32; C]; R];
        for ((o, acc), x) in out.iter_mut().zip(&acc).zip(&xs) {
            for ((o, &acc), w) in o.iter_mut().zip(acc).zip(&ws) {
                *o = hsum(acc);
                for (a, b) in x[p..].iter().zip(&w[p..]) {
                    *o += a * b;
                }
            }
        }
        out
    }

    // The element-wise kernels: the portable loops compiled for AVX2.

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn gelu(x: &mut [f32]) {
        super::gelu_lanes(x)
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn softmax(x: &mut [f32]) {
        super::softmax_lanes(x)
    }

    #[target_feature(enable = "avx2,fma,f16c")]
    unsafe fn layer_norm(x: &mut [f32], dim: usize, w: &[f32], b: &[f32]) {
        super::layer_norm_lanes(x, dim, w, b)
    }
}

/// Kernels for aarch64 (NEON is part of the architecture): 4 lanes.
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use super::format::{Q4_0, Q4_1, Q5_0, Q5_1, Q8_0};
    use super::{format, prefixes};
    use crate::model::{f16_to_f32, QType, QK};
    use std::arch::aarch64::*;

    /// Proof that NEON may be used (always, on the targets this is built
    /// for). Its methods are the safe entry points to the kernels below.
    #[derive(Clone, Copy, Debug)]
    pub(in crate::ops) struct Neon(());

    impl Neon {
        pub(in crate::ops) fn detect() -> Option<Neon> {
            Some(Neon(()))
        }

        // The SAFETY argument for every call below: the target has NEON
        // (checked by the `cfg` of this module), and the kernels access
        // memory only through bounds-checked slices.

        pub(in crate::ops) fn dot(self, a: &[f32], b: &[f32]) -> f32 {
            // SAFETY: the CPU has NEON (see above).
            unsafe { dot(a, b) }
        }

        pub(in crate::ops) fn axpy(self, a: f32, x: &[f32], y: &mut [f32]) {
            // SAFETY: the CPU has NEON (see above).
            unsafe { axpy(a, x, y) }
        }

        pub(in crate::ops) fn expand_f16(self, src: &[u16], out: &mut [f32]) {
            // SAFETY: the CPU has NEON (see above).
            unsafe { expand_f16(src, out) }
        }

        pub(in crate::ops) fn expand_quant(self, t: QType, src: &[u8], out: &mut [f32]) {
            let bb = t.block_bytes();
            // SAFETY: the CPU has NEON (see above).
            unsafe { per_format!(t, expand_quant::<_>(src, bb, out)) }
        }

        pub(in crate::ops) fn half_dots<const N: usize>(
            self,
            row: &[u16],
            xs: &[&[f32]; N],
        ) -> [f32; N] {
            // SAFETY: the CPU has NEON (see above).
            unsafe { half_dots(row, xs) }
        }

        pub(in crate::ops) fn quant_dots<const N: usize>(
            self,
            t: QType,
            row: &[u8],
            xs: &[&[f32]; N],
        ) -> [f32; N] {
            let bb = t.block_bytes();
            // SAFETY: the CPU has NEON (see above).
            unsafe { per_format!(t, quant_dots::<_, N>(row, bb, xs)) }
        }

        pub(in crate::ops) fn tile<const R: usize, const C: usize>(
            self,
            xs: &[&[f32]; R],
            ws: &[&[f32]; C],
        ) -> [[f32; C]; R] {
            // SAFETY: the CPU has NEON (see above).
            unsafe { tile(xs, ws) }
        }

        // The element-wise loops vectorise with NEON as they are.

        pub(in crate::ops) fn gelu(self, x: &mut [f32]) {
            super::gelu_lanes(x)
        }

        pub(in crate::ops) fn softmax(self, x: &mut [f32]) {
            super::softmax_lanes(x)
        }

        pub(in crate::ops) fn layer_norm(self, x: &mut [f32], dim: usize, w: &[f32], b: &[f32]) {
            super::layer_norm_lanes(x, dim, w, b)
        }
    }

    /// `s[..4]` (panics if shorter).
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn load(s: &[f32]) -> float32x4_t {
        let s = &s[..4];
        // SAFETY: `s` has 4 readable values; the load needs no alignment.
        unsafe { vld1q_f32(s.as_ptr()) }
    }

    /// Writes `v` to `s[..4]` (panics if shorter).
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn store(s: &mut [f32], v: float32x4_t) {
        let s = &mut s[..4];
        // SAFETY: `s` has 4 writable values; the store needs no alignment.
        unsafe { vst1q_f32(s.as_mut_ptr(), v) }
    }

    /// The bytes `s[..16]`.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn load_bytes(s: &[u8]) -> uint8x16_t {
        let s = &s[..16];
        // SAFETY: `s` has 16 readable bytes; the load needs no alignment.
        unsafe { vld1q_u8(s.as_ptr()) }
    }

    /// Half floats (in the low 16 bits of the lanes) as f32, exactly like
    /// `f16_to_f32`.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn halves(h: uint32x4_t) -> float32x4_t {
        let sign = vshlq_n_u32::<16>(vandq_u32(h, vdupq_n_u32(0x8000)));
        let exp = vandq_u32(h, vdupq_n_u32(0x7c00));
        let shifted = vshlq_n_u32::<13>(vandq_u32(h, vdupq_n_u32(0x7fff)));
        let normal = vaddq_u32(shifted, vdupq_n_u32((127 - 15) << 23));
        let special = vorrq_u32(shifted, vdupq_n_u32(0x7f80_0000));
        let frac = vcvtq_f32_u32(vandq_u32(h, vdupq_n_u32(0x3ff)));
        let sub = vreinterpretq_u32_f32(vmulq_n_f32(frac, 1.0 / 16_777_216.0));
        let bits = vbslq_u32(vceqq_u32(exp, vdupq_n_u32(0x7c00)), special, normal);
        let bits = vbslq_u32(vceqq_u32(exp, vdupq_n_u32(0)), sub, bits);
        vreinterpretq_f32_u32(vorrq_u32(bits, sign))
    }

    /// The half floats `s[..8]` as f32.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn load_f16(s: &[u16]) -> [float32x4_t; 2] {
        let s = &s[..8];
        // SAFETY: `s` has 8 readable values; the load needs no alignment.
        let h = unsafe { vld1q_u16(s.as_ptr()) };
        [
            halves(vmovl_u16(vget_low_u16(h))),
            halves(vmovl_high_u16(h)),
        ]
    }

    /// The 16 bytes `v` (unsigned) as f32.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn u8_to_f32(v: uint8x16_t) -> [float32x4_t; 4] {
        let (lo, hi) = (vmovl_u8(vget_low_u8(v)), vmovl_high_u8(v));
        [
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(lo))),
            vcvtq_f32_u32(vmovl_high_u16(lo)),
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(hi))),
            vcvtq_f32_u32(vmovl_high_u16(hi)),
        ]
    }

    /// The 16 bytes `v` (signed) as f32.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn i8_to_f32(v: int8x16_t) -> [float32x4_t; 4] {
        let (lo, hi) = (vmovl_s8(vget_low_s8(v)), vmovl_high_s8(v));
        [
            vcvtq_f32_s32(vmovl_s16(vget_low_s16(lo))),
            vcvtq_f32_s32(vmovl_high_s16(lo)),
            vcvtq_f32_s32(vmovl_s16(vget_low_s16(hi))),
            vcvtq_f32_s32(vmovl_high_s16(hi)),
        ]
    }

    /// The bits of `a` then `b` as 0x10 in 16 bytes: fifth bits of the
    /// values of a q5 block.
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn fifth_bits(a: u8, b: u8) -> uint8x16_t {
        const BIT: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
        let bit = load_bytes(&BIT);
        let spread = vcombine_u8(vdup_n_u8(a), vdup_n_u8(b));
        vandq_u8(vtstq_u8(spread, bit), vdupq_n_u8(0x10))
    }

    /// The 32 values of block `b` in format `T` (layouts as in
    /// `QType::dequantize`).
    #[inline]
    #[target_feature(enable = "neon")]
    unsafe fn block<const T: u8>(b: &[u8]) -> [float32x4_t; 8] {
        let half = |at: usize| f16_to_f32(u16::from_le_bytes([b[at], b[at + 1]]));
        let d = half(0);
        if T == Q8_0 {
            let lo = i8_to_f32(vreinterpretq_s8_u8(load_bytes(&b[2..18])));
            let hi = i8_to_f32(vreinterpretq_s8_u8(load_bytes(&b[18..34])));
            return [
                vmulq_n_f32(lo[0], d),
                vmulq_n_f32(lo[1], d),
                vmulq_n_f32(lo[2], d),
                vmulq_n_f32(lo[3], d),
                vmulq_n_f32(hi[0], d),
                vmulq_n_f32(hi[1], d),
                vmulq_n_f32(hi[2], d),
                vmulq_n_f32(hi[3], d),
            ];
        }
        // The 16 nibble bytes follow d, the minimum (q4_1, q5_1) and the
        // fifth bits (q5).
        let at = match T {
            Q4_0 => 2,
            Q4_1 => 4,
            Q5_0 => 6,
            _ => 8,
        };
        let min = vdupq_n_f32(match T {
            Q4_0 => -8.0 * d,
            Q5_0 => -16.0 * d,
            _ => half(2),
        });
        let q = load_bytes(&b[at..at + 16]);
        let mut lo = vandq_u8(q, vdupq_n_u8(0x0f));
        let mut hi = vshrq_n_u8::<4>(q);
        if T == Q5_0 || T == Q5_1 {
            lo = vorrq_u8(lo, fifth_bits(b[at - 4], b[at - 3]));
            hi = vorrq_u8(hi, fifth_bits(b[at - 2], b[at - 1]));
        }
        let (lo, hi) = (u8_to_f32(lo), u8_to_f32(hi));
        let d = vdupq_n_f32(d);
        [
            vfmaq_f32(min, lo[0], d),
            vfmaq_f32(min, lo[1], d),
            vfmaq_f32(min, lo[2], d),
            vfmaq_f32(min, lo[3], d),
            vfmaq_f32(min, hi[0], d),
            vfmaq_f32(min, hi[1], d),
            vfmaq_f32(min, hi[2], d),
            vfmaq_f32(min, hi[3], d),
        ]
    }

    #[target_feature(enable = "neon")]
    unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [vdupq_n_f32(0.0); 4];
        let (mut ca, mut cb) = (a.chunks_exact(16), b.chunks_exact(16));
        for (x, y) in (&mut ca).zip(&mut cb) {
            for (j, acc) in acc.iter_mut().enumerate() {
                *acc = vfmaq_f32(*acc, load(&x[j * 4..]), load(&y[j * 4..]));
            }
        }
        let (mut ca, mut cb) = (
            ca.remainder().chunks_exact(4),
            cb.remainder().chunks_exact(4),
        );
        for (x, y) in (&mut ca).zip(&mut cb) {
            acc[0] = vfmaq_f32(acc[0], load(x), load(y));
        }
        let sum = vaddq_f32(vaddq_f32(acc[0], acc[1]), vaddq_f32(acc[2], acc[3]));
        let mut s = vaddvq_f32(sum);
        for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
            s += x * y;
        }
        s
    }

    #[target_feature(enable = "neon")]
    unsafe fn axpy(a: f32, x: &[f32], y: &mut [f32]) {
        let (mut cx, mut cy) = (x.chunks_exact(4), y.chunks_exact_mut(4));
        for (x, y) in (&mut cx).zip(&mut cy) {
            store(y, vfmaq_n_f32(load(y), load(x), a));
        }
        for (x, y) in cx.remainder().iter().zip(cy.into_remainder()) {
            *y += a * x;
        }
    }

    #[target_feature(enable = "neon")]
    unsafe fn expand_f16(src: &[u16], out: &mut [f32]) {
        let (mut cs, mut co) = (src.chunks_exact(8), out.chunks_exact_mut(8));
        for (s, o) in (&mut cs).zip(&mut co) {
            let [a, b] = load_f16(s);
            store(o, a);
            store(&mut o[4..], b);
        }
        for (&h, o) in cs.remainder().iter().zip(co.into_remainder()) {
            *o = f16_to_f32(h);
        }
    }

    #[target_feature(enable = "neon")]
    unsafe fn expand_quant<const T: u8>(src: &[u8], bb: usize, out: &mut [f32]) {
        for (b, o) in src.chunks_exact(bb).zip(out.chunks_exact_mut(QK)) {
            let v = block::<T>(b);
            for (j, v) in v.into_iter().enumerate() {
                store(&mut o[j * 4..], v);
            }
        }
    }

    #[target_feature(enable = "neon")]
    unsafe fn half_dots<const N: usize>(row: &[u16], xs: &[&[f32]; N]) -> [f32; N] {
        let k = row.len();
        let xs = prefixes(xs, k);
        let mut acc = [[vdupq_n_f32(0.0); 2]; N];
        let mut p = 0;
        while p + 8 <= k {
            let [w0, w1] = load_f16(&row[p..]);
            for (acc, x) in acc.iter_mut().zip(&xs) {
                acc[0] = vfmaq_f32(acc[0], w0, load(&x[p..]));
                acc[1] = vfmaq_f32(acc[1], w1, load(&x[p + 4..]));
            }
            p += 8;
        }
        let mut out = [0f32; N];
        for ((o, acc), x) in out.iter_mut().zip(&acc).zip(&xs) {
            *o = vaddvq_f32(vaddq_f32(acc[0], acc[1]));
            for (&h, v) in row[p..].iter().zip(&x[p..]) {
                *o += f16_to_f32(h) * v;
            }
        }
        out
    }

    #[target_feature(enable = "neon")]
    unsafe fn quant_dots<const T: u8, const N: usize>(
        row: &[u8],
        bb: usize,
        xs: &[&[f32]; N],
    ) -> [f32; N] {
        let mut acc = [[vdupq_n_f32(0.0); 2]; N];
        for (c, b) in row.chunks_exact(bb).enumerate() {
            let w = block::<T>(b);
            for (acc, x) in acc.iter_mut().zip(xs) {
                let x = &x[c * QK..(c + 1) * QK];
                for (j, &w) in w.iter().enumerate() {
                    acc[j % 2] = vfmaq_f32(acc[j % 2], w, load(&x[j * 4..]));
                }
            }
        }
        let mut out = [0f32; N];
        for (o, acc) in out.iter_mut().zip(&acc) {
            *o = vaddvq_f32(vaddq_f32(acc[0], acc[1]));
        }
        out
    }

    /// All `R` x `C` dots of `xs` and `ws` (of the length of `xs[0]`),
    /// each loaded vector used `C` or `R` times.
    #[target_feature(enable = "neon")]
    unsafe fn tile<const R: usize, const C: usize>(
        xs: &[&[f32]; R],
        ws: &[&[f32]; C],
    ) -> [[f32; C]; R] {
        let k = xs.first().map_or(0, |x| x.len());
        let (xs, ws) = (prefixes(xs, k), prefixes(ws, k));
        let mut acc = [[vdupq_n_f32(0.0); C]; R];
        let mut p = 0;
        while p + 4 <= k {
            let mut xv = [vdupq_n_f32(0.0); R];
            for (v, x) in xv.iter_mut().zip(&xs) {
                *v = load(&x[p..]);
            }
            for (j, w) in ws.iter().enumerate() {
                let wv = load(&w[p..]);
                for (acc, &xv) in acc.iter_mut().zip(&xv) {
                    acc[j] = vfmaq_f32(acc[j], xv, wv);
                }
            }
            p += 4;
        }
        let mut out = [[0f32; C]; R];
        for ((o, acc), x) in out.iter_mut().zip(&acc).zip(&xs) {
            for ((o, &acc), w) in o.iter_mut().zip(acc).zip(&ws) {
                *o = vaddvq_f32(acc);
                for (a, b) in x[p..].iter().zip(&w[p..]) {
                    *o += a * b;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{f16_to_f32, QType, Store};

    /// Small deterministic generator (xorshift).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Uniform in [-1, 1).
        fn f32(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }

        fn vec(&mut self, n: usize) -> Vec<f32> {
            (0..n).map(|_| self.f32()).collect()
        }
    }

    /// Half float of a value in the normal range (truncated).
    fn to_f16(v: f32) -> u16 {
        let b = v.to_bits();
        let sign = ((b >> 16) & 0x8000) as u16;
        if v.abs() < 6.2e-5 {
            return sign;
        }
        let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
        sign | ((exp as u16) << 10) | (((b >> 13) & 0x3ff) as u16)
    }

    const FORMATS: [Option<QType>; 6] = [
        None,
        Some(QType::Q4_0),
        Some(QType::Q4_1),
        Some(QType::Q5_0),
        Some(QType::Q5_1),
        Some(QType::Q8_0),
    ];

    /// Random `rows` x `cols` weights in format `q` (`None` = f16).
    fn weights(rng: &mut Rng, cols: usize, rows: usize, q: Option<QType>) -> Tensor {
        let store = match q {
            None => Store::F16((0..cols * rows).map(|_| to_f16(rng.f32() * 0.1)).collect()),
            Some(t) => {
                let bb = t.block_bytes();
                let mut raw: Vec<u8> = (0..cols * rows / QK * bb)
                    .map(|_| rng.next() as u8)
                    .collect();
                for blk in raw.chunks_exact_mut(bb) {
                    let d = to_f16(0.01 + rng.f32().abs() * 0.01);
                    blk[..2].copy_from_slice(&d.to_le_bytes());
                    if matches!(t, QType::Q4_1 | QType::Q5_1) {
                        blk[2..4].copy_from_slice(&to_f16(rng.f32() * 0.1).to_le_bytes());
                    }
                }
                Store::Quant(t, raw)
            }
        };
        Tensor::new(cols, rows, store).expect("tensor size")
    }

    /// Rows `r0..r1` of `w` as f32 with the plain kernels.
    fn plain_rows(w: &Tensor, r0: usize, r1: usize) -> Vec<f32> {
        let mut out = vec![0f32; (r1 - r0) * w.cols];
        expand_with(Simd::Scalar, w.packed(r0, r1), &mut out);
        out
    }

    /// Dot product in f64 and the sum of the absolute products (the scale
    /// rounding errors are relative to).
    fn exact_dot(a: &[f32], b: &[f32]) -> (f64, f64) {
        a.iter().zip(b).fold((0.0, 0.0), |(s, m), (&x, &y)| {
            let p = x as f64 * y as f64;
            (s + p, m + p.abs())
        })
    }

    fn assert_close(got: f32, (want, scale): (f64, f64), what: &str) {
        let err = (got as f64 - want).abs();
        assert!(
            err <= 1e-5 * scale + 1e-30,
            "{what}: {got} vs {want} (scale {scale})"
        );
    }

    #[test]
    fn vector_kernels_are_chosen() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            matches!(simd(), Simd::Avx2(_)),
            is_x86_feature_detected!("avx2")
                && is_x86_feature_detected!("fma")
                && is_x86_feature_detected!("f16c")
        );
        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        assert!(matches!(simd(), Simd::Neon(_)));
    }

    #[test]
    fn dot_and_axpy() {
        let mut rng = Rng(1);
        for simd in [simd(), Simd::Scalar] {
            for len in [0, 1, 3, 4, 7, 8, 9, 15, 16, 31, 32, 33, 64, 100, 257, 768] {
                let (a, b) = (rng.vec(len), rng.vec(len + 3));
                let d = dispatch!(simd, dot(&a, &b[..len]));
                assert_close(d, exact_dot(&a, &b), &format!("{simd:?} dot {len}"));
                let mut y = b[..len].to_vec();
                dispatch!(simd, axpy(0.75, &a, &mut y));
                for ((&y, &a), &b) in y.iter().zip(&a).zip(&b) {
                    let want = b as f64 + 0.75 * a as f64;
                    assert!((y as f64 - want).abs() <= 1e-6, "{simd:?} axpy {len}");
                }
            }
        }
        assert_eq!(dot(&[1.0, 2.0, 3.0], &[4.0, 5.0]), 14.0);
    }

    #[test]
    fn expansion() {
        let mut rng = Rng(2);
        for q in FORMATS {
            for cols in [32, 64, 96] {
                let w = weights(&mut rng, cols, 3, q);
                let want = plain_rows(&w, 0, 3);
                let mut got = vec![0f32; want.len()];
                expand(w.packed(0, 3), &mut got);
                for (&g, &v) in got.iter().zip(&want) {
                    assert!(
                        (g - v).abs() <= 1e-6 * v.abs().max(0.1),
                        "{q:?}: {g} vs {v}"
                    );
                }
                assert_eq!(w.row(2).as_ref(), &got[2 * cols..]);
            }
        }
        // f16 rows of any length, with special values.
        let h: Vec<u16> = (0..=u16::MAX).step_by(7).collect();
        let mut got = vec![0f32; h.len()];
        expand(Packed::F16(&h), &mut got);
        for (&h, &g) in h.iter().zip(&got) {
            let want = f16_to_f32(h);
            assert!(g.to_bits() == want.to_bits() || (g.is_nan() && want.is_nan()));
        }
    }

    fn check_dots<const N: usize>(rng: &mut Rng, w: &Tensor, what: &str) {
        let k = w.cols;
        let x = rng.vec(N * k);
        let xs: [&[f32]; N] = std::array::from_fn(|i| &x[i * k..(i + 1) * k]);
        for r in 0..w.rows {
            let row = plain_rows(w, r, r + 1);
            for simd in [simd(), Simd::Scalar] {
                let d = match w.packed(r, r + 1) {
                    Packed::F16(p) => dispatch!(simd, half_dots(p, &xs)),
                    Packed::Quant(t, p) => dispatch!(simd, quant_dots(t, p, &xs)),
                    Packed::F32(_) => unreachable!(),
                };
                for (d, x) in d.iter().zip(&xs) {
                    assert_close(*d, exact_dot(x, &row), &format!("{simd:?} {what}"));
                }
            }
        }
    }

    #[test]
    fn fused_dots() {
        let mut rng = Rng(3);
        for q in FORMATS {
            let lengths: &[usize] = match q {
                None => &[1, 7, 8, 15, 16, 17, 100, 768],
                Some(_) => &[32, 64, 768],
            };
            for &k in lengths {
                let w = weights(&mut rng, k, 3, q);
                let what = format!("{q:?} k={k}");
                check_dots::<1>(&mut rng, &w, &what);
                check_dots::<2>(&mut rng, &w, &what);
                check_dots::<3>(&mut rng, &w, &what);
                check_dots::<4>(&mut rng, &w, &what);
            }
        }
    }

    #[test]
    fn tiles() {
        let mut rng = Rng(4);
        for k in [0, 1, 5, 8, 13, 64, 100, 768] {
            let (x, w) = (rng.vec(3 * k), rng.vec(4 * k));
            let xs: [&[f32]; 3] = std::array::from_fn(|i| &x[i * k..(i + 1) * k]);
            let ws: [&[f32]; 4] = std::array::from_fn(|j| &w[j * k..(j + 1) * k]);
            for simd in [simd(), Simd::Scalar] {
                let t = dispatch!(simd, tile(&xs, &ws));
                let t1 = dispatch!(simd, tile(&[xs[2]], &[ws[1]]));
                for (i, x) in xs.iter().enumerate() {
                    for (j, w) in ws.iter().enumerate() {
                        assert_close(t[i][j], exact_dot(x, w), &format!("{simd:?} k={k}"));
                    }
                }
                assert_close(t1[0][0], exact_dot(xs[2], ws[1]), "1x1");
            }
        }
    }

    #[test]
    fn linear_matches_reference() {
        let mut rng = Rng(5);
        let shapes = [
            (1, 64, 5),
            (2, 64, 37),
            (3, 96, 16),
            (4, 96, 40),
            (5, 64, 37),
            (7, 128, 9),
            (1, 256, 3000),
            (4, 256, 1100),
            (40, 256, 300),
        ];
        for q in FORMATS {
            for &(n, k, m) in &shapes {
                let w = weights(&mut rng, k, m, q);
                let x = rng.vec(n * k);
                let bias = Tensor::from_f32(m, 1, rng.vec(m));
                let y = linear(&x, n, &w, Some(&bias));
                assert_eq!(y.len(), n * m);
                let rows = plain_rows(&w, 0, m);
                for i in 0..n {
                    for j in 0..m {
                        let (d, s) = exact_dot(&x[i * k..(i + 1) * k], &rows[j * k..(j + 1) * k]);
                        let b = bias.values()[j] as f64;
                        let what = format!("{q:?} n={n} k={k} m={m} at {i},{j}");
                        assert_close(y[i * m + j], (d + b, s + b.abs()), &what);
                    }
                }
            }
        }
        // f32 weights (convolutions) and odd lengths.
        for &(n, k, m) in &[(1, 7, 5), (3, 13, 6), (11, 100, 17)] {
            let w = Tensor::from_f32(k, m, rng.vec(k * m));
            let x = rng.vec(n * k);
            let y = linear(&x, n, &w, None);
            for i in 0..n {
                for j in 0..m {
                    let want = exact_dot(&x[i * k..(i + 1) * k], &w.values()[j * k..(j + 1) * k]);
                    assert_close(y[i * m + j], want, "f32");
                }
            }
        }
        assert!(linear(&[], 0, &Tensor::from_f32(4, 3, vec![0.0; 12]), None).is_empty());
    }

    #[test]
    fn exp_accuracy() {
        let mut x = -87.0f32;
        while x < 88.0 {
            let (got, want) = (exp_lane(x) as f64, (x as f64).exp());
            assert!(
                (got - want).abs() <= 3e-7 * want,
                "exp({x}) = {got}, not {want}"
            );
            x += 0.0137;
        }
        assert_eq!(exp_lane(-100.0), 0.0);
        assert_eq!(exp_lane(f32::NEG_INFINITY), 0.0);
        assert!(exp_lane(f32::NAN).is_nan());
        assert!(exp_lane(1000.0).is_finite());
    }

    #[test]
    fn element_wise() {
        let mut rng = Rng(6);
        let mut x: Vec<f32> = rng.vec(1000).iter().map(|v| v * 8.0).collect();
        x.extend([0.0, -0.0, 30.0, -30.0, 1e6, -1e6, 1e-20]);
        // GELU against the tanh formula: in f64 (written without the
        // cancellation of 1 + tanh z), and as it was computed before.
        for simd in [simd(), Simd::Scalar] {
            let mut g = x.clone();
            dispatch!(simd, gelu(&mut g));
            for (&u, &g) in x.iter().zip(&g) {
                let ud = u as f64;
                let z = 0.797_884_560_8 * (ud + 0.044_715 * ud.powi(3));
                let want = ud / (1.0 + (-2.0 * z).exp());
                // Far out on the negative side the result is about e^(2z),
                // and z's own rounding in f32 adds a relative 2|z| ulp.
                let tolerance = 1e-6 + 2.4e-7 * z.abs().min(1e3);
                assert!(
                    (g as f64 - want).abs() <= tolerance * want.abs() + 1e-30,
                    "{simd:?} gelu({u}) = {g}, not {want}"
                );
                let old = 0.5 * u * (1.0 + (0.797_884_6 * (u + 0.044_715 * u * u * u)).tanh());
                assert!(
                    (g - old).abs() <= 1e-6 * old.abs().max(1.0),
                    "{simd:?} gelu({u})"
                );
            }
        }
        // Softmax.
        for len in [0, 1, 5, 16, 33, 1500] {
            let x: Vec<f32> = rng.vec(len).iter().map(|v| v * 20.0).collect();
            let max = x.iter().fold(f64::MIN, |m, &v| m.max(v as f64));
            let sum: f64 = x.iter().map(|&v| (v as f64 - max).exp()).sum();
            for simd in [simd(), Simd::Scalar] {
                let mut s = x.clone();
                dispatch!(simd, softmax(&mut s));
                for (&v, &p) in x.iter().zip(&s) {
                    let want = (v as f64 - max).exp() / sum;
                    // Rounding v - max in f32 adds a relative |v - max| ulp.
                    let tolerance = 1e-6 + 1.2e-7 * (v as f64 - max).abs();
                    assert!(
                        (p as f64 - want).abs() <= tolerance * want + 1e-37,
                        "{simd:?} softmax: {p} vs {want}"
                    );
                }
            }
        }
        // Layer norm.
        let dim = 77;
        let x = rng.vec(dim * 3);
        let (w, b) = (rng.vec(dim), rng.vec(dim));
        for simd in [simd(), Simd::Scalar] {
            let mut y = x.clone();
            dispatch!(simd, layer_norm(&mut y, dim, &w, &b));
            for (row, out) in x.chunks(dim).zip(y.chunks(dim)) {
                let mean = row.iter().map(|&v| v as f64).sum::<f64>() / dim as f64;
                let var = row.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / dim as f64;
                for (i, (&v, &o)) in row.iter().zip(out).enumerate() {
                    let want = (v as f64 - mean) / (var + 1e-5).sqrt() * w[i] as f64 + b[i] as f64;
                    assert!((o as f64 - want).abs() <= 1e-5, "{simd:?} layer norm");
                }
            }
        }
    }

    /// Times `linear` for encoder- and decoder-like shapes:
    /// `cargo test -p bl-asr --release -- --ignored --nocapture bench_linear`
    #[test]
    #[ignore]
    fn bench_linear() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let shapes = [
            ("encoder", 1500, 768, 768, 10),
            ("encoder 1 thread", 1500, 768, 768, 3),
            ("decoder", 1, 768, 768, 500),
            ("decoder mlp", 1, 768, 3072, 200),
            ("decoder mlp2", 1, 3072, 768, 200),
            ("decoder logits", 1, 768, 51865, 20),
            ("decoder n=4", 4, 768, 3072, 100),
        ];
        let time = |name: &str, fmt: &str, n: usize, w: &Tensor, reps: usize| {
            let x = Rng(7).vec(n * w.cols);
            let _ = linear(&x, n, w, None);
            // The fastest run: the least disturbed by other work.
            let mut best = f64::MAX;
            for _ in 0..reps {
                let t = std::time::Instant::now();
                std::hint::black_box(linear(&x, n, w, None));
                best = best.min(t.elapsed().as_secs_f64());
            }
            let (k, m) = (w.cols, w.rows);
            println!(
                "{fmt:5} {name:16} n={n:4} k={k:4} m={m:5}: {:8.3} ms",
                best * 1e3
            );
        };
        for q in [
            Some(QType::Q5_1),
            None,
            Some(QType::Q8_0),
            Some(QType::Q4_0),
        ] {
            let fmt = q.map_or("f16".to_string(), |t| format!("{t:?}"));
            for &(name, n, k, m, reps) in &shapes {
                let w = weights(&mut rng, k, m, q);
                set_max_threads(if name.ends_with("1 thread") { 1 } else { 0 });
                time(name, &fmt, n, &w, reps);
            }
        }
        set_max_threads(0);
        let w = Tensor::from_f32(768, 768, rng.vec(768 * 768));
        time("encoder", "f32", 1500, &w, 10);
    }
}
