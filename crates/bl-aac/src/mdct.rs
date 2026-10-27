//! FFT-based inverse MDCT (via a DCT-IV of size N/2 computed with an N/4
//! point complex FFT).

use crate::fft::{Complex, Fft};
use std::f64::consts::PI;

pub(crate) struct Imdct {
    /// Number of input coefficients (N/2).
    m: usize,
    fft: Fft,
    pre: Vec<Complex>,
    post: Vec<Complex>,
    buf: Vec<Complex>,
    dct: Vec<f32>,
}

impl Imdct {
    /// IMDCT producing `2 * m` outputs from `m` coefficients, computing
    /// `y[n] = scale * sum_k X[k] cos(pi/m (n + n0)(k + 1/2))`,
    /// `n0 = m/2 + 1/2`.
    pub fn new(m: usize, scale: f64) -> Imdct {
        let q = m / 2;
        let pre = (0..q)
            .map(|n| Complex::expi(-PI * (4 * n + 1) as f64 / (4 * m) as f64))
            .collect();
        let post = (0..q)
            .map(|k| {
                let c = Complex::expi(-PI * k as f64 / m as f64);
                Complex::new((c.re as f64 * scale) as f32, (c.im as f64 * scale) as f32)
            })
            .collect();
        Imdct {
            m,
            fft: Fft::new(q),
            pre,
            post,
            buf: vec![Complex::default(); q],
            dct: vec![0.0; m],
        }
    }

    /// DCT-IV of `input` (length m) into `self.dct` (scaled).
    fn dct4(&mut self, input: &[f32]) {
        let m = self.m;
        let q = m / 2;
        for n in 0..q {
            let v = Complex::new(input[2 * n], input[m - 1 - 2 * n]);
            self.buf[n] = v.mul(self.pre[n]);
        }
        self.fft.forward(&mut self.buf);
        for k in 0..q {
            let w = self.buf[k].mul(self.post[k]);
            self.dct[2 * k] = w.re;
            self.dct[m - 1 - 2 * k] = -w.im;
        }
    }

    /// Full IMDCT: `output` has length 2m.
    pub fn transform(&mut self, input: &[f32], output: &mut [f32]) {
        let m = self.m;
        debug_assert_eq!(self.fft.len(), m / 2);
        self.dct4(&input[..m]);
        let h = m / 2;
        let u = &self.dct;
        for n in 0..h {
            output[n] = u[h + n];
        }
        for n in h..3 * h {
            output[n] = -u[3 * h - 1 - n];
        }
        for n in 3 * h..2 * m {
            output[n] = -u[n - 3 * h];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdct_matches_direct() {
        for m in [16usize, 128, 1024] {
            let mut t = Imdct::new(m, 1.0);
            let input: Vec<f32> = (0..m)
                .map(|i| (((i * 37 + 11) % 23) as f32 - 11.0) / 7.0)
                .collect();
            let mut out = vec![0.0; 2 * m];
            t.transform(&input, &mut out);
            let n0 = m as f64 / 2.0 + 0.5;
            for n in (0..2 * m).step_by(if m > 128 { 37 } else { 1 }) {
                let mut acc = 0.0f64;
                for (k, &x) in input.iter().enumerate() {
                    acc += x as f64 * (PI / m as f64 * (n as f64 + n0) * (k as f64 + 0.5)).cos();
                }
                assert!(
                    (acc - out[n] as f64).abs() < 2e-3 * (m as f64).sqrt(),
                    "m={m} n={n} {acc} {}",
                    out[n]
                );
            }
        }
    }
}
