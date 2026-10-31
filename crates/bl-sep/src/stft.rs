//! Short-time Fourier transform as `torch.stft` / `torch.istft` compute it
//! with `center=True`: the signal reflected by half a window at both ends,
//! a periodic Hann window, frames every `hop` samples; the inverse adds the
//! windowed frames up and divides by the sum of the squared windows.

use crate::fft::{Complex, RealFft};
use crate::parallel;
use std::ops::Range;

pub struct Stft {
    pub n_fft: usize,
    pub hop: usize,
    window: Vec<f32>,
    fft: RealFft,
}

/// Index into a signal of `len` samples for position `i`, reflected at
/// both ends (`[3, 2, 1, 0, 1, 2, 3, ...]`, repeatedly for short signals).
fn reflect(i: isize, len: usize) -> Option<usize> {
    match len {
        0 => None,
        1 => Some(0),
        _ => {
            let period = 2 * (len as isize - 1);
            let m = i.rem_euclid(period) as usize;
            Some(if m >= len { period as usize - m } else { m })
        }
    }
}

impl Stft {
    pub fn new(n_fft: usize, hop: usize) -> Stft {
        let window = (0..n_fft)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / n_fft as f64;
                (0.5 - 0.5 * a.cos()) as f32
            })
            .collect();
        Stft {
            n_fft,
            hop,
            window,
            fft: RealFft::new(n_fft),
        }
    }

    /// Frequency bins per frame.
    pub fn bins(&self) -> usize {
        self.n_fft / 2 + 1
    }

    /// Frames of a signal of `len` samples.
    pub fn frames(&self, len: usize) -> usize {
        len / self.hop + 1
    }

    /// The spectra of `frames` of `signal`, `bins()` values each.
    pub fn forward(&self, signal: &[f32], frames: Range<usize>) -> Vec<Complex> {
        let bins = self.bins();
        let half = (self.n_fft / 2) as isize;
        parallel(frames.len(), |part| {
            let mut out = vec![Complex::default(); part.len() * bins];
            let mut buf = vec![0f32; self.n_fft];
            let mut z = Vec::new();
            for (i, spec) in part.zip(out.chunks_exact_mut(bins)) {
                let start = ((frames.start + i) * self.hop) as isize - half;
                for (j, (b, w)) in buf.iter_mut().zip(&self.window).enumerate() {
                    let s = reflect(start + j as isize, signal.len()).map_or(0.0, |k| signal[k]);
                    *b = s * w;
                }
                self.fft.forward(&buf, &mut z, spec);
            }
            out
        })
    }

    /// The windowed signals of the spectra `spec` (`bins()` values each),
    /// `n_fft` values each, for adding up with [`OverlapAdd`].
    pub fn inverse(&self, spec: &[Complex]) -> Vec<f32> {
        let bins = self.bins();
        let n = self.n_fft;
        parallel(spec.len() / bins, |part| {
            let mut out = vec![0f32; part.len() * n];
            let mut z = Vec::new();
            for (i, frame) in part.zip(out.chunks_exact_mut(n)) {
                self.fft
                    .inverse(&spec[i * bins..(i + 1) * bins], &mut z, frame);
                for (v, w) in frame.iter_mut().zip(&self.window) {
                    *v *= w;
                }
            }
            out
        })
    }
}

/// The inverse transform: windowed frames added up in place.
pub struct OverlapAdd {
    sum: Vec<f32>,
    frames: usize,
}

impl OverlapAdd {
    /// For a signal of `len` samples.
    pub fn new(stft: &Stft, len: usize) -> OverlapAdd {
        let frames = stft.frames(len);
        OverlapAdd {
            sum: vec![0f32; stft.n_fft + stft.hop * (frames - 1)],
            frames,
        }
    }

    /// Adds the windowed signals `frames` (from [`Stft::inverse`]) of the
    /// frames from `first` on.
    pub fn add(&mut self, stft: &Stft, first: usize, frames: &[f32]) {
        for (i, f) in frames.chunks_exact(stft.n_fft).enumerate() {
            let at = (first + i) * stft.hop;
            for (s, v) in self.sum[at..at + stft.n_fft].iter_mut().zip(f) {
                *s += v;
            }
        }
    }

    /// The signal of `len` samples (as `torch.istft` with `length`).
    pub fn finish(self, stft: &Stft, len: usize) -> Vec<f32> {
        let mut envelope = vec![0f32; self.sum.len()];
        for t in 0..self.frames {
            let at = t * stft.hop;
            for (e, w) in envelope[at..at + stft.n_fft].iter_mut().zip(&stft.window) {
                *e += w * w;
            }
        }
        let mut out = self.sum;
        for (v, e) in out.iter_mut().zip(&envelope) {
            *v = if *e > 1e-11 { *v / e } else { 0.0 };
        }
        out.drain(..stft.n_fft / 2);
        out.resize(len, 0.0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflects_like_torch() {
        let r: Vec<usize> = (-3..7).map(|i| reflect(i, 4).unwrap()).collect();
        assert_eq!(r, [3, 2, 1, 0, 1, 2, 3, 2, 1, 0]);
        assert_eq!(reflect(-5, 1), Some(0));
        assert_eq!(reflect(0, 0), None);
    }

    #[test]
    fn round_trip() {
        let stft = Stft::new(64, 16);
        for len in [0, 1, 5, 100, 1000, 1007] {
            let x: Vec<f32> = (0..len)
                .map(|i| ((i * 37) % 23) as f32 / 11.0 - 1.0 + (i as f32 * 0.01).sin())
                .collect();
            let frames = stft.frames(len);
            let spec = stft.forward(&x, 0..frames);
            assert_eq!(spec.len(), frames * stft.bins());
            let mut ola = OverlapAdd::new(&stft, len);
            // In two parts, as the separation does.
            let cut = frames / 2;
            let bins = stft.bins();
            ola.add(&stft, 0, &stft.inverse(&spec[..cut * bins]));
            ola.add(&stft, cut, &stft.inverse(&spec[cut * bins..]));
            let y = ola.finish(&stft, len);
            assert_eq!(y.len(), len);
            for (i, (a, b)) in y.iter().zip(&x).enumerate() {
                assert!((a - b).abs() < 1e-4, "{len} {i}: {a} {b}");
            }
        }
    }

    #[test]
    fn spectrum_of_a_centred_frame() {
        // Frame 1 of a constant signal: the window's sum at bin 0.
        let stft = Stft::new(32, 8);
        let x = vec![1f32; 64];
        let spec = stft.forward(&x, 1..2);
        assert!((spec[0].re - 16.0).abs() < 1e-4);
        assert!(spec[0].im.abs() < 1e-4);
        assert!((spec[1].norm() - 8.0).abs() < 1e-4);
        assert!(spec[2..].iter().all(|c| c.norm() < 1e-4));
    }
}
