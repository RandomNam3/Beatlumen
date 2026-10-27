//! Radix-2 FFT for real input (power of two sizes).

/// Precomputed real-input FFT of a fixed power-of-two size `n`.
///
/// Internally a complex FFT of size `n / 2` is used, followed by the usual
/// split step to obtain the spectrum of the real sequence.
pub struct RealFft {
    n: usize,
    half: usize,
    /// Twiddles for the half size complex FFT: exp(-2πik/half), k < half/2.
    tw: Vec<(f32, f32)>,
    /// Twiddles for the split step: exp(-2πik/n), k < half.
    split: Vec<(f32, f32)>,
    bitrev: Vec<u32>,
    buf: Vec<(f32, f32)>,
}

impl RealFft {
    pub fn new(n: usize) -> RealFft {
        assert!(n >= 4 && n.is_power_of_two(), "fft size must be a power of two >= 4");
        let half = n / 2;
        let tw = (0..half / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / half as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let split = (0..half)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let bits = half.trailing_zeros();
        let bitrev = (0..half as u32).map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) }).collect();
        RealFft { n, half, tw, split, bitrev, buf: vec![(0.0, 0.0); half] }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Computes the complex spectrum bins `0..=n/2` of the real `input`
    /// (length `n`). `out` receives `n/2 + 1` (re, im) pairs.
    pub fn forward(&mut self, input: &[f32], out: &mut Vec<(f32, f32)>) {
        assert_eq!(input.len(), self.n);
        let half = self.half;
        for i in 0..half {
            let j = self.bitrev[i] as usize;
            self.buf[j] = (input[2 * i], input[2 * i + 1]);
        }
        // Iterative radix-2 decimation in time on the packed sequence.
        let mut size = 2;
        while size <= half {
            let step = half / size;
            let h = size / 2;
            for start in (0..half).step_by(size) {
                for k in 0..h {
                    let (wr, wi) = self.tw[k * step];
                    let a = self.buf[start + k];
                    let b = self.buf[start + k + h];
                    let tr = b.0 * wr - b.1 * wi;
                    let ti = b.0 * wi + b.1 * wr;
                    self.buf[start + k] = (a.0 + tr, a.1 + ti);
                    self.buf[start + k + h] = (a.0 - tr, a.1 - ti);
                }
            }
            size *= 2;
        }
        out.clear();
        out.resize(half + 1, (0.0, 0.0));
        // Split step: X[k] = (Z[k] + conj(Z[N/2-k]))/2 + W^k (Z[k] - conj(Z[N/2-k]))/(2i)
        let z0 = self.buf[0];
        out[0] = (z0.0 + z0.1, 0.0);
        out[half] = (z0.0 - z0.1, 0.0);
        for k in 1..half {
            let a = self.buf[k];
            let b = self.buf[half - k];
            let er = 0.5 * (a.0 + b.0);
            let ei = 0.5 * (a.1 - b.1);
            let or = 0.5 * (a.1 + b.1);
            let oi = -0.5 * (a.0 - b.0);
            let (wr, wi) = self.split[k];
            let tr = or * wr - oi * wi;
            let ti = or * wi + oi * wr;
            out[k] = (er + tr, ei + ti);
        }
    }

    /// Computes amplitude spectrum bins `0..n/2` scaled by `2/n` (so that a
    /// full-scale sine of amplitude A yields ≈ A in its bin before
    /// windowing). Bin 0 is the absolute DC value scaled the same way.
    pub fn amplitudes(&mut self, input: &[f32], out: &mut [f32], scratch: &mut Vec<(f32, f32)>) {
        self.forward(input, scratch);
        let p = 2.0 / self.n as f32;
        let half = self.half;
        out[0] = (p * scratch[0].0).abs();
        for k in 1..half.min(out.len()) {
            let (re, im) = scratch[k];
            out[k] = p * (re * re + im * im).sqrt();
        }
    }
}

/// Applies a Blackman window in place.
pub fn blackman(buf: &mut [f32]) {
    let n = buf.len() as f32;
    for (i, v) in buf.iter_mut().enumerate() {
        let x = std::f32::consts::TAU * i as f32 / n;
        *v *= 0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos();
    }
}

/// Precomputed Blackman window coefficients.
pub fn blackman_window(n: usize) -> Vec<f32> {
    let mut w = vec![1.0; n];
    blackman(&mut w);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(input: &[f32]) -> Vec<(f64, f64)> {
        let n = input.len();
        (0..=n / 2)
            .map(|k| {
                let mut re = 0.0;
                let mut im = 0.0;
                for (t, &x) in input.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                    re += x as f64 * a.cos();
                    im += x as f64 * a.sin();
                }
                (re, im)
            })
            .collect()
    }

    #[test]
    fn matches_naive_dft() {
        for &n in &[4usize, 8, 64, 512] {
            let input: Vec<f32> = (0..n).map(|i| ((i * 7919 % 101) as f32 / 50.0 - 1.0) + (i as f32 * 0.3).sin()).collect();
            let mut fft = RealFft::new(n);
            let mut out = Vec::new();
            fft.forward(&input, &mut out);
            let reference = naive(&input);
            for (a, b) in out.iter().zip(&reference) {
                assert!((a.0 as f64 - b.0).abs() < 1e-3 * n as f64, "n={n} {a:?} {b:?}");
                assert!((a.1 as f64 - b.1).abs() < 1e-3 * n as f64, "n={n} {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn sine_amplitude() {
        let n = 1024;
        let input: Vec<f32> = (0..n).map(|i| (std::f32::consts::TAU * 32.0 * i as f32 / n as f32).sin()).collect();
        let mut fft = RealFft::new(n);
        let mut amps = vec![0.0; n / 2];
        let mut scratch = Vec::new();
        fft.amplitudes(&input, &mut amps, &mut scratch);
        assert!((amps[32] - 1.0).abs() < 1e-3);
        assert!(amps[10] < 1e-3);
    }
}
