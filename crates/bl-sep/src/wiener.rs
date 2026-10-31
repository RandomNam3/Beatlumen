//! Multichannel Wiener filtering (`openunmix.filtering.wiener` with one
//! expectation-maximisation step and a residual source): the vocals and
//! the rest of the mixture (mixture minus vocals) are modelled per
//! frequency bin by a spatial covariance matrix and a power per frame;
//! the vocals are then the mixture filtered with
//! `v_j R_j (sum_k v_k R_k)^-1`. This keeps the stereo picture of the
//! vocals and lets the phase differ from the mixture's.

use crate::fft::Complex;
use crate::parallel;

/// Regularisation of the powers and covariance matrices.
const EPS: f64 = 1e-10;
/// The mixture is scaled down to a largest magnitude of about this before
/// the step (for the numbers to stay in range).
const SCALE: f64 = 10.0;

#[derive(Clone, Copy, Default, Debug)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn of(c: Complex) -> C {
        C {
            re: c.re as f64,
            im: c.im as f64,
        }
    }
    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
    fn sub(self, o: C) -> C {
        C {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn conj(self) -> C {
        C {
            re: self.re,
            im: -self.im,
        }
    }
    fn scale(self, s: f64) -> C {
        C {
            re: self.re * s,
            im: self.im * s,
        }
    }
    fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

/// A 2x2 complex matrix, row major.
type M2 = [C; 4];

fn mat_mul(a: &M2, b: &M2) -> M2 {
    [
        a[0].mul(b[0]).add(a[1].mul(b[2])),
        a[0].mul(b[1]).add(a[1].mul(b[3])),
        a[2].mul(b[0]).add(a[3].mul(b[2])),
        a[2].mul(b[1]).add(a[3].mul(b[3])),
    ]
}

fn inverse(m: &M2) -> M2 {
    let det = m[0].mul(m[3]).sub(m[1].mul(m[2]));
    let n = det.norm2();
    let inv = C {
        re: det.re / n,
        im: -det.im / n,
    };
    [
        inv.mul(m[3]),
        inv.mul(m[1]).scale(-1.0),
        inv.mul(m[2]).scale(-1.0),
        inv.mul(m[0]),
    ]
}

/// Filters the vocals of a window of `frames` frames: `mix` holds the
/// mixture's spectra (`2 * bins` per frame, the left then the right
/// channel) and `vocals` the first estimate (the mask times the mixture),
/// which is replaced by the filtered one.
pub fn filter(mix: &[Complex], vocals: &mut [Complex], frames: usize, bins: usize) {
    let width = 2 * bins;
    debug_assert_eq!(mix.len(), frames * width);
    let largest = mix.iter().map(|c| C::of(*c).norm2()).fold(0.0, f64::max);
    let scale = (largest.sqrt() / SCALE).max(1.0);
    let at = |t: usize, ch: usize, f: usize| t * width + ch * bins + f;
    let parts = parallel(bins, |range| {
        let mut out = Vec::with_capacity(range.len() * frames * 2);
        let mut x = vec![[C::default(); 2]; frames];
        // The two sources: vocals and the rest.
        let mut y = [
            vec![[C::default(); 2]; frames],
            vec![[C::default(); 2]; frames],
        ];
        let mut v = [vec![0f64; frames], vec![0f64; frames]];
        for f in range {
            for t in 0..frames {
                for ch in 0..2 {
                    let m = C::of(mix[at(t, ch, f)]).scale(1.0 / scale);
                    let s = C::of(vocals[at(t, ch, f)]).scale(1.0 / scale);
                    x[t][ch] = m;
                    y[0][t][ch] = s;
                    y[1][t][ch] = m.sub(s);
                }
            }
            // Powers per frame and spatial covariances.
            let mut r = [[C::default(); 4]; 2];
            for j in 0..2 {
                let mut weight = EPS;
                for t in 0..frames {
                    let [a, b] = y[j][t];
                    v[j][t] = (a.norm2() + b.norm2()) / 2.0;
                    weight += v[j][t];
                    let cov = [
                        a.mul(a.conj()),
                        a.mul(b.conj()),
                        b.mul(a.conj()),
                        b.mul(b.conj()),
                    ];
                    for (r, c) in r[j].iter_mut().zip(cov) {
                        *r = r.add(c);
                    }
                }
                for r in &mut r[j] {
                    *r = r.scale(1.0 / weight);
                }
            }
            let reg = EPS.sqrt();
            for t in 0..frames {
                let mut cxx = [
                    C { re: reg, im: 0.0 },
                    C::default(),
                    C::default(),
                    C { re: reg, im: 0.0 },
                ];
                for j in 0..2 {
                    for (c, r) in cxx.iter_mut().zip(&r[j]) {
                        *c = c.add(r.scale(v[j][t]));
                    }
                }
                let gain = mat_mul(&r[0], &inverse(&cxx));
                let [a, b] = x[t];
                for ch in 0..2 {
                    let g = gain[2 * ch].mul(a).add(gain[2 * ch + 1].mul(b));
                    let g = g.scale(v[0][t] * scale);
                    out.push(Complex::new(g.re as f32, g.im as f32));
                }
            }
        }
        out
    });
    // `parts` holds bin after bin, frame after frame, channel after channel.
    for (k, c) in parts.into_iter().enumerate() {
        let (f, rest) = (k / (2 * frames), k % (2 * frames));
        vocals[at(rest / 2, rest % 2, f)] = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverts_2x2() {
        let m: M2 = [
            C { re: 2.0, im: 1.0 },
            C { re: 0.5, im: -1.0 },
            C { re: 0.5, im: 1.0 },
            C { re: 3.0, im: 0.0 },
        ];
        let p = mat_mul(&m, &inverse(&m));
        for (i, c) in p.iter().enumerate() {
            let want = if i == 0 || i == 3 { 1.0 } else { 0.0 };
            assert!((c.re - want).abs() < 1e-12 && c.im.abs() < 1e-12);
        }
    }

    #[test]
    fn keeps_a_clean_source() {
        // Vocals alone in the left channel, the rest alone in the right,
        // and a first estimate that says so: the filter keeps it.
        let (frames, bins) = (5, 3);
        let mut mix = vec![Complex::default(); frames * 2 * bins];
        let mut vocals = mix.clone();
        for t in 0..frames {
            for f in 0..bins {
                let l = Complex::new(1.0 + t as f32, f as f32 - 1.0);
                let r = Complex::new(0.5, 2.0 - t as f32);
                mix[t * 2 * bins + f] = l;
                mix[t * 2 * bins + bins + f] = r;
                vocals[t * 2 * bins + f] = l;
            }
        }
        let want: Vec<Complex> = (0..frames * 2 * bins)
            .map(|i| {
                if i % (2 * bins) < bins {
                    mix[i]
                } else {
                    Complex::default()
                }
            })
            .collect();
        filter(&mix, &mut vocals, frames, bins);
        for (a, b) in vocals.iter().zip(&want) {
            assert!(
                (a.re - b.re).abs() < 1e-3 && (a.im - b.im).abs() < 1e-3,
                "{a:?} {b:?}"
            );
        }
    }
}
