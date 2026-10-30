//! FFT of real signals (power-of-two sizes): a complex FFT of half the
//! size (iterative radix-2) on the even and odd samples, then one pass that
//! splits the two spectra.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    #[inline(always)]
    pub const fn new(re: f32, im: f32) -> Complex {
        Complex { re, im }
    }

    #[inline(always)]
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }

    #[inline(always)]
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }

    #[inline(always)]
    fn mul(self, o: Complex) -> Complex {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }

    #[inline(always)]
    fn conj(self) -> Complex {
        Complex::new(self.re, -self.im)
    }

    #[inline(always)]
    pub fn norm(self) -> f32 {
        self.re.hypot(self.im)
    }

    #[inline(always)]
    pub fn scale(self, s: f32) -> Complex {
        Complex::new(self.re * s, self.im * s)
    }
}

/// Real FFT of size `n`: `n / 2 + 1` bins `X[k] = sum x[t] e^(-2 pi i k t / n)`,
/// and its inverse (with the factor `1 / n`).
pub struct RealFft {
    n: usize,
    bitrev: Vec<u32>,
    /// e^(-2 pi i k / (n / 2)), k < n / 4 (for the half-size FFT).
    twiddles: Vec<Complex>,
    /// e^(-2 pi i k / n), k <= n / 2 (for splitting the spectra).
    split: Vec<Complex>,
}

impl RealFft {
    pub fn new(n: usize) -> RealFft {
        assert!(n.is_power_of_two() && n >= 8, "FFT size");
        let half = n / 2;
        let bits = half.trailing_zeros();
        let bitrev = (0..half as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let expi = |a: f64| Complex::new(a.cos() as f32, a.sin() as f32);
        let twiddles = (0..half / 2)
            .map(|k| expi(-2.0 * PI * k as f64 / half as f64))
            .collect();
        let split = (0..=half)
            .map(|k| expi(-2.0 * PI * k as f64 / n as f64))
            .collect();
        RealFft {
            n,
            bitrev,
            twiddles,
            split,
        }
    }

    /// In-place complex FFT of `n / 2` values (forward; the inverse is
    /// done by conjugating before and after).
    fn complex(&self, z: &mut [Complex]) {
        let m = z.len();
        for i in 0..m {
            let j = self.bitrev[i] as usize;
            if j > i {
                z.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= m {
            let step = m / len;
            let h = len / 2;
            for block in z.chunks_exact_mut(len) {
                let (a, b) = block.split_at_mut(h);
                for k in 0..h {
                    let t = b[k].mul(self.twiddles[k * step]);
                    b[k] = a[k].sub(t);
                    a[k] = a[k].add(t);
                }
            }
            len *= 2;
        }
    }

    /// The `n / 2 + 1` bins of the `n` real values `x`; `z` is scratch
    /// space of `n / 2` values.
    pub fn forward(&self, x: &[f32], z: &mut Vec<Complex>, out: &mut [Complex]) {
        let half = self.n / 2;
        z.clear();
        z.extend(
            x[..self.n]
                .chunks_exact(2)
                .map(|p| Complex::new(p[0], p[1])),
        );
        self.complex(z);
        for k in 0..=half {
            let a = z[k % half];
            let b = z[(half - k) % half].conj();
            // Spectra of the even and of the odd samples.
            let even = a.add(b).scale(0.5);
            let odd = a.sub(b).scale(0.5);
            // X = E + w^k O with O = -i * odd.
            let o = Complex::new(odd.im, -odd.re);
            out[k] = even.add(self.split[k].mul(o));
        }
    }

    /// The `n` real values of the bins `spec` (`n / 2 + 1`; the imaginary
    /// parts of the first and last bin are ignored); `z` is scratch space.
    pub fn inverse(&self, spec: &[Complex], z: &mut Vec<Complex>, out: &mut [f32]) {
        let half = self.n / 2;
        let bin = |k: usize| {
            let c = spec[k];
            if k == 0 || k == half {
                Complex::new(c.re, 0.0)
            } else {
                c
            }
        };
        z.clear();
        z.extend((0..half).map(|k| {
            let a = bin(k);
            let b = bin(half - k).conj();
            let even = a.add(b);
            let odd = a.sub(b).mul(self.split[k].conj());
            // Z = E + i O, conjugated for the inverse transform.
            Complex::new(even.re - odd.im, even.im + odd.re).conj()
        }));
        self.complex(z);
        let s = 1.0 / self.n as f32;
        for (p, c) in out[..self.n].chunks_exact_mut(2).zip(z.iter()) {
            p[0] = c.re * s;
            p[1] = -c.im * s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dft(x: &[f32]) -> Vec<(f64, f64)> {
        let n = x.len();
        (0..=n / 2)
            .map(|k| {
                x.iter().enumerate().fold((0.0, 0.0), |(re, im), (t, &v)| {
                    let a = -2.0 * PI * ((k * t) % n) as f64 / n as f64;
                    (re + v as f64 * a.cos(), im + v as f64 * a.sin())
                })
            })
            .collect()
    }

    #[test]
    fn matches_the_dft_and_inverts() {
        for n in [8, 64, 512] {
            let f = RealFft::new(n);
            let x: Vec<f32> = (0..n)
                .map(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0)
                .collect();
            let mut z = Vec::new();
            let mut spec = vec![Complex::default(); n / 2 + 1];
            f.forward(&x, &mut z, &mut spec);
            for (a, b) in spec.iter().zip(dft(&x)) {
                assert!((a.re as f64 - b.0).abs() < 1e-3, "{n}: {a:?} {b:?}");
                assert!((a.im as f64 - b.1).abs() < 1e-3, "{n}: {a:?} {b:?}");
            }
            let mut back = vec![0f32; n];
            f.inverse(&spec, &mut z, &mut back);
            for (a, b) in back.iter().zip(&x) {
                assert!((a - b).abs() < 1e-5, "{n}: {a} {b}");
            }
        }
    }
}
