//! Small complex FFT (power-of-two sizes, iterative radix-2 with a radix-4
//! first pass) used by the IMDCT and the SBR/PS filterbanks.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    #[inline(always)]
    pub const fn new(re: f32, im: f32) -> Self {
        Complex { re, im }
    }
    #[inline(always)]
    pub fn mul(self, o: Complex) -> Complex {
        Complex::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    /// e^{i * phase}
    pub fn expi(phase: f64) -> Complex {
        Complex::new(phase.cos() as f32, phase.sin() as f32)
    }
}

/// Forward complex FFT of a fixed power-of-two size:
/// `X[k] = sum x[n] e^{-2 pi i n k / N}`.
pub(crate) struct Fft {
    n: usize,
    bitrev: Vec<u32>,
    /// Twiddles e^{-2 pi i k / n}, k < n/2.
    twiddles: Vec<Complex>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n >= 4);
        let bits = n.trailing_zeros();
        let bitrev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let twiddles = (0..n / 2).map(|k| Complex::expi(-2.0 * std::f64::consts::PI * k as f64 / n as f64)).collect();
        Fft { n, bitrev, twiddles }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn forward(&self, x: &mut [Complex]) {
        let n = self.n;
        let x = &mut x[..n];
        for i in 0..n {
            let j = self.bitrev[i] as usize;
            if j > i {
                x.swap(i, j);
            }
        }
        // Radix-4 first pass (sizes 2 and 4 combined, no multiplications).
        for c in x.chunks_exact_mut(4) {
            let a0 = c[0];
            let a1 = c[1];
            let a2 = c[2];
            let a3 = c[3];
            let s0 = Complex::new(a0.re + a1.re, a0.im + a1.im);
            let d0 = Complex::new(a0.re - a1.re, a0.im - a1.im);
            let s1 = Complex::new(a2.re + a3.re, a2.im + a3.im);
            let d1 = Complex::new(a2.re - a3.re, a2.im - a3.im);
            c[0] = Complex::new(s0.re + s1.re, s0.im + s1.im);
            c[2] = Complex::new(s0.re - s1.re, s0.im - s1.im);
            // d1 * (-i) = (d1.im, -d1.re)
            c[1] = Complex::new(d0.re + d1.im, d0.im - d1.re);
            c[3] = Complex::new(d0.re - d1.im, d0.im + d1.re);
        }
        let mut half = 4;
        while half < n {
            let stride = n / (2 * half);
            for block in x.chunks_exact_mut(2 * half) {
                let (lo, hi) = block.split_at_mut(half);
                for k in 0..half {
                    let w = self.twiddles[k * stride];
                    let t = hi[k].mul(w);
                    let a = lo[k];
                    lo[k] = Complex::new(a.re + t.re, a.im + t.im);
                    hi[k] = Complex::new(a.re - t.re, a.im - t.im);
                }
            }
            half *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_dft() {
        for n in [4usize, 8, 16, 64, 512] {
            let fft = Fft::new(n);
            let input: Vec<Complex> =
                (0..n).map(|i| Complex::new(((i * 7 + 3) % 11) as f32 - 5.0, ((i * 5 + 1) % 13) as f32 - 6.0)).collect();
            let mut x = input.clone();
            fft.forward(&mut x);
            for k in 0..n {
                let mut acc = (0.0f64, 0.0f64);
                for (j, v) in input.iter().enumerate() {
                    let ph = -2.0 * std::f64::consts::PI * (j * k % n) as f64 / n as f64;
                    acc.0 += v.re as f64 * ph.cos() - v.im as f64 * ph.sin();
                    acc.1 += v.re as f64 * ph.sin() + v.im as f64 * ph.cos();
                }
                assert!((acc.0 - x[k].re as f64).abs() < 1e-3 * n as f64, "n={n} k={k}");
                assert!((acc.1 - x[k].im as f64).abs() < 1e-3 * n as f64, "n={n} k={k}");
            }
        }
    }
}
