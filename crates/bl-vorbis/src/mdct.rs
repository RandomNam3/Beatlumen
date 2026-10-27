//! Inverse MDCT in O(n log n).
//!
//! The IMDCT of `N` outputs from `N/2` coefficients is computed through a
//! DCT-IV of size `M = N/2`, which in turn is evaluated with an `M/2`-point
//! complex FFT plus pre- and post-twiddling:
//!
//! ```text
//! v[k] = (X[2k] + i·X[M-1-2k]) · t[k],      t[k] = exp(-iπ(k + 1/8)/M)
//! V    = FFT_{M/2}(v)
//! W[m] = V[m] · t[m]
//! u[2m] = Re W[m],  u[M-1-2m] = -Im W[m]
//! ```
//!
//! `u` is the DCT-IV `u[j] = Σ X[k] cos(π/M (j+½)(k+½))`. The IMDCT output
//! `y[i] = Σ X[k] cos(2π/N (i + ½ + N/4)(k + ½))` follows by symmetry:
//! `y[i] = u[i+N/4]` for `i < N/4`, `-u[3N/4-1-i]` for `N/4 <= i < 3N/4` and
//! `-u[i-3N/4]` above. The decoder only needs the two halves of `y`, which
//! [`Imdct::left_half`] and [`Imdct::right_half`] derive from `u`.

use std::f64::consts::PI;

/// Twiddles of one radix-4 stage (sub-transform size `h`, block `4h`):
/// `t^j`, `t^2j`, `t^3j` with `t = exp(-2πi/(4h))`, for `j < h`.
struct Radix4Stage {
    h: usize,
    w1: (Vec<f32>, Vec<f32>),
    w2: (Vec<f32>, Vec<f32>),
    w3: (Vec<f32>, Vec<f32>),
}

fn twiddles(count: usize, size: usize, mult: usize) -> (Vec<f32>, Vec<f32>) {
    (0..count)
        .map(|j| {
            let a = -2.0 * PI * (j * mult) as f64 / size as f64;
            (a.cos() as f32, a.sin() as f32)
        })
        .unzip()
}

/// Complex FFT (forward, `exp(-2πi jk/n)`) of a fixed power-of-two size
/// `n >= 4`, decimation in time on bit-reversed input. Pairs of radix-2
/// stages are merged into radix-4 ("radix-2²") passes needing three complex
/// multiplications per four points; an odd stage count ends with one
/// radix-2 pass. Data is kept as separate real/imaginary arrays.
struct Fft {
    n: usize,
    bitrev: Vec<u32>,
    stages: Vec<Radix4Stage>,
    /// Twiddles `exp(-2πi j/n)` (`j < n/2`) of a final radix-2 stage.
    last: Option<(Vec<f32>, Vec<f32>)>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 4);
        let bits = n.trailing_zeros();
        let bitrev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let mut stages = Vec::new();
        let mut h = 4;
        while 4 * h <= n {
            stages.push(Radix4Stage {
                h,
                w1: twiddles(h, 4 * h, 1),
                w2: twiddles(h, 4 * h, 2),
                w3: twiddles(h, 4 * h, 3),
            });
            h *= 4;
        }
        let last = if h < n {
            Some(twiddles(n / 2, n, 1))
        } else {
            None
        };
        Fft {
            n,
            bitrev,
            stages,
            last,
        }
    }

    /// In-place forward FFT of bit-reversed-ordered input.
    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        let re = &mut re[..n];
        let im = &mut im[..n];
        // First radix-4 pass (sub-size 1): no multiplications.
        for (r, i) in re.chunks_exact_mut(4).zip(im.chunks_exact_mut(4)) {
            let (ar, ai) = (r[0] + r[1], i[0] + i[1]);
            let (br, bi) = (r[0] - r[1], i[0] - i[1]);
            let (cr, ci) = (r[2] + r[3], i[2] + i[3]);
            let (dr, di) = (r[2] - r[3], i[2] - i[3]);
            r[0] = ar + cr;
            i[0] = ai + ci;
            r[2] = ar - cr;
            i[2] = ai - ci;
            r[1] = br + di;
            i[1] = bi - dr;
            r[3] = br - di;
            i[3] = bi + dr;
        }
        for st in &self.stages {
            let h = st.h;
            let (w1r, w1i) = (&st.w1.0[..h], &st.w1.1[..h]);
            let (w2r, w2i) = (&st.w2.0[..h], &st.w2.1[..h]);
            let (w3r, w3i) = (&st.w3.0[..h], &st.w3.1[..h]);
            for (rb, ib) in re.chunks_exact_mut(4 * h).zip(im.chunks_exact_mut(4 * h)) {
                let (r0, rest) = rb.split_at_mut(h);
                let (r1, rest) = rest.split_at_mut(h);
                let (r2, r3) = rest.split_at_mut(h);
                let (i0, rest) = ib.split_at_mut(h);
                let (i1, rest) = rest.split_at_mut(h);
                let (i2, i3) = rest.split_at_mut(h);
                let r3 = &mut r3[..h];
                let i3 = &mut i3[..h];
                for j in 0..h {
                    // b·t², c·t, d·t³
                    let br = r1[j] * w2r[j] - i1[j] * w2i[j];
                    let bi = r1[j] * w2i[j] + i1[j] * w2r[j];
                    let cr = r2[j] * w1r[j] - i2[j] * w1i[j];
                    let ci = r2[j] * w1i[j] + i2[j] * w1r[j];
                    let dr = r3[j] * w3r[j] - i3[j] * w3i[j];
                    let di = r3[j] * w3i[j] + i3[j] * w3r[j];
                    let (ar, ai) = (r0[j], i0[j]);
                    let (sr, si) = (ar + br, ai + bi);
                    let (tr, ti) = (ar - br, ai - bi);
                    let (ur, ui) = (cr + dr, ci + di);
                    let (vr, vi) = (cr - dr, ci - di);
                    r0[j] = sr + ur;
                    i0[j] = si + ui;
                    r2[j] = sr - ur;
                    i2[j] = si - ui;
                    r1[j] = tr + vi;
                    i1[j] = ti - vr;
                    r3[j] = tr - vi;
                    i3[j] = ti + vr;
                }
            }
        }
        if let Some((wr, wi)) = &self.last {
            let h = n / 2;
            let (lr, hr) = re.split_at_mut(h);
            let (li, hi) = im.split_at_mut(h);
            let (wr, wi) = (&wr[..h], &wi[..h]);
            let (hr, hi) = (&mut hr[..h], &mut hi[..h]);
            for j in 0..h {
                let tr = hr[j] * wr[j] - hi[j] * wi[j];
                let ti = hr[j] * wi[j] + hi[j] * wr[j];
                let (ar, ai) = (lr[j], li[j]);
                lr[j] = ar + tr;
                li[j] = ai + ti;
                hr[j] = ar - tr;
                hi[j] = ai - ti;
            }
        }
    }
}

/// IMDCT for one block size `N` (window length).
pub(crate) struct Imdct {
    n: usize,
    fft: Fft,
    /// `exp(-iπ(k + 1/8)/M)` for `k < N/4`.
    t_re: Vec<f32>,
    t_im: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Imdct {
    pub fn new(n: usize) -> Self {
        let m = n / 2;
        let q = n / 4;
        let (t_re, t_im) = (0..q)
            .map(|k| {
                let a = -PI * (k as f64 + 0.125) / m as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .unzip();
        Imdct {
            n,
            fft: Fft::new(q),
            t_re,
            t_im,
            re: vec![0.0; q],
            im: vec![0.0; q],
        }
    }

    /// Computes the DCT-IV of `x` (length `N/2`) in place.
    pub fn dct4(&mut self, x: &mut [f32]) {
        let m = self.n / 2;
        let q = self.n / 4;
        let x = &mut x[..m];
        for k in 0..q {
            let a = x[2 * k];
            let b = x[m - 1 - 2 * k];
            let (c, s) = (self.t_re[k], self.t_im[k]);
            let p = self.fft.bitrev[k] as usize;
            self.re[p] = a * c - b * s;
            self.im[p] = a * s + b * c;
        }
        self.fft.run(&mut self.re, &mut self.im);
        for k in 0..q {
            let (vr, vi) = (self.re[k], self.im[k]);
            let (c, s) = (self.t_re[k], self.t_im[k]);
            x[2 * k] = vr * c - vi * s;
            x[m - 1 - 2 * k] = -(vr * s + vi * c);
        }
    }

    /// Writes the first half of the IMDCT output (`y[0..N/2]`) given the
    /// DCT-IV result `u`.
    pub fn left_half(u: &[f32], out: &mut [f32]) {
        let m = u.len();
        let h = m / 2;
        let (a, b) = out[..m].split_at_mut(h);
        a.copy_from_slice(&u[h..m]);
        // b[i] = y[h + i] = -u[3M/2 - 1 - (h + i)] = -u[M - 1 - i]
        for (i, o) in b.iter_mut().enumerate() {
            *o = -u[m - 1 - i];
        }
    }

    /// Writes the second half of the IMDCT output (`y[N/2..N]`).
    pub fn right_half(u: &[f32], out: &mut [f32]) {
        let m = u.len();
        let h = m / 2;
        let (a, b) = out[..m].split_at_mut(h);
        // y[M + i] = -u[M/2 - 1 - i] for i < M/2, else -u[i - M/2]
        for (i, o) in a.iter_mut().enumerate() {
            *o = -u[h - 1 - i];
        }
        for (o, &v) in b.iter_mut().zip(&u[..h]) {
            *o = -v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_imdct(x: &[f32]) -> Vec<f64> {
        let m = x.len();
        let n = 2 * m;
        (0..n)
            .map(|i| {
                x.iter()
                    .enumerate()
                    .map(|(k, &v)| {
                        v as f64
                            * (2.0 * PI / n as f64
                                * (i as f64 + 0.5 + n as f64 / 4.0)
                                * (k as f64 + 0.5))
                                .cos()
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn matches_naive() {
        for &n in &[16usize, 32, 64, 256, 2048] {
            let m = n / 2;
            let mut seed = 12345u32;
            let x: Vec<f32> = (0..m)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .collect();
            let reference = naive_imdct(&x);
            let mut u = x.clone();
            let mut imdct = Imdct::new(n);
            imdct.dct4(&mut u);
            let mut y = vec![0f32; n];
            Imdct::left_half(&u, &mut y[..m]);
            Imdct::right_half(&u, &mut y[m..]);
            let peak = reference.iter().fold(0f64, |a, &b| a.max(b.abs()));
            for (i, (&a, &b)) in y.iter().zip(&reference).enumerate() {
                assert!(
                    (a as f64 - b).abs() < 1e-5 * peak.max(1.0) * (n as f64).sqrt(),
                    "n={n} i={i}: {a} vs {b}"
                );
            }
        }
    }
}
