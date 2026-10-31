//! Shared signal pieces: bringing the input down to about 16 kHz and the
//! YIN difference function.

/// Output samples the [`Decimator`] lags behind its input.
pub(crate) const DECIMATOR_DELAY: usize = 12;

/// The integer factor that brings `rate` down to about 16 kHz (pitch up to
/// 1.1 kHz and a few harmonics fit well below 8 kHz; fewer samples keep the
/// difference function cheap).
pub(crate) fn decimation_factor(rate: u32) -> usize {
    ((rate as f64 / 16000.0).round() as usize).max(1)
}

/// Low-pass filter and integer downsampling, sample by sample (for the
/// microphone) or a whole song at once. The output lags
/// [`DECIMATOR_DELAY`] output samples behind (zero for factor 1).
pub(crate) struct Decimator {
    factor: usize,
    taps: Vec<f32>,
    /// Input not used up yet, after the last `taps.len() - 1` samples.
    buf: Vec<f32>,
    /// Index in `buf` of the newest sample of the next output.
    next: usize,
}

impl Decimator {
    pub(crate) fn new(factor: usize) -> Decimator {
        let factor = factor.max(1);
        let n = 2 * DECIMATOR_DELAY * factor + 1;
        let taps = if factor == 1 {
            Vec::new()
        } else {
            // Windowed sinc (Blackman), cut at 0.4 of the new rate.
            let fc = 0.4 / factor as f64;
            let m = (n - 1) as f64 / 2.0;
            let mut t: Vec<f64> = (0..n)
                .map(|i| {
                    let x = i as f64 - m;
                    let sinc = if x == 0.0 {
                        2.0 * fc
                    } else {
                        (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                    };
                    let w = 2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64;
                    sinc * (0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos())
                })
                .collect();
            let sum: f64 = t.iter().sum();
            t.iter_mut().for_each(|v| *v /= sum);
            t.into_iter().map(|v| v as f32).collect()
        };
        let mut d = Decimator {
            factor,
            taps,
            buf: Vec::new(),
            next: 0,
        };
        d.reset();
        d
    }

    pub(crate) fn factor(&self) -> usize {
        self.factor
    }

    pub(crate) fn reset(&mut self) {
        self.buf.clear();
        if self.factor > 1 {
            // Silence before the first sample.
            self.buf.resize(self.taps.len() - 1, 0.0);
            self.next = self.taps.len() - 1;
        }
    }

    pub(crate) fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.factor == 1 {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        let n = self.taps.len();
        while self.next < self.buf.len() {
            out.push(dot(&self.buf[self.next + 1 - n..=self.next], &self.taps));
            self.next += self.factor;
        }
        // Keep the history the next output needs.
        let keep_from = self.next + 1 - n;
        self.buf.drain(..keep_from);
        self.next -= keep_from;
    }
}

/// A whole signal at `rate` brought to the analysis rate, without the
/// filter's delay. Returns the samples and their rate.
pub(crate) fn downsample(samples: &[f32], rate: u32) -> (Vec<f32>, f64) {
    let factor = decimation_factor(rate);
    let out_rate = rate as f64 / factor as f64;
    if factor == 1 {
        return (samples.to_vec(), out_rate);
    }
    let mut d = Decimator::new(factor);
    let mut out = Vec::with_capacity(samples.len() / factor + DECIMATOR_DELAY + 1);
    d.push(samples, &mut out);
    d.push(&vec![0.0; DECIMATOR_DELAY * factor + factor], &mut out);
    out.drain(..DECIMATOR_DELAY.min(out.len()));
    out.truncate(samples.len().div_ceil(factor));
    (out, out_rate)
}

/// Sum of products; eight running sums so that it vectorizes.
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in ca.zip(cb) {
        for ((s, x), y) in acc.iter_mut().zip(x).zip(y) {
            *s += x * y;
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// Sum of squared differences, vectorized like [`dot`].
fn sq_diff(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| (x - y) * (x - y))
        .sum();
    for (x, y) in ca.zip(cb) {
        for ((s, x), y) in acc.iter_mut().zip(x).zip(y) {
            *s += (x - y) * (x - y);
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// The cumulative mean normalized difference d'(τ) of YIN for
/// τ = 0..out.len(), over a window of `w` samples: `x` needs
/// `w + out.len() - 1` samples.
pub(crate) fn cmnd(x: &[f32], w: usize, out: &mut [f32]) {
    let a = &x[..w];
    out[0] = 1.0;
    let mut sum = 0f32;
    for tau in 1..out.len() {
        let d = sq_diff(a, &x[tau..tau + w]);
        sum += d;
        out[tau] = if sum > 0.0 { d * tau as f32 / sum } else { 1.0 };
    }
}

/// Offset (−0.5…0.5) of the true minimum from `i` by a parabola through
/// its neighbours.
pub(crate) fn parabolic(d: &[f32], i: usize) -> f32 {
    if i == 0 || i + 1 >= d.len() {
        return 0.0;
    }
    let (a, b, c) = (d[i - 1], d[i], d[i + 1]);
    let den = a - 2.0 * b + c;
    if den <= 0.0 {
        return 0.0;
    }
    (0.5 * (a - c) / den).clamp(-0.5, 0.5)
}

/// Root mean square.
pub(crate) fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (dot(x, x) / x.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimator_streams_like_whole() {
        let x: Vec<f32> = (0..4000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut whole = Vec::new();
        Decimator::new(3).push(&x, &mut whole);
        let mut parts = Vec::new();
        let mut d = Decimator::new(3);
        for c in x.chunks(37) {
            d.push(c, &mut parts);
        }
        assert_eq!(whole.len(), parts.len());
        assert!(whole.iter().zip(&parts).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn downsample_keeps_low_tones_in_place() {
        // 300 Hz at 48 kHz: after downsampling to 16 kHz still 300 Hz, in
        // phase (the delay is removed) and at the same level.
        let x: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f32::consts::PI * 300.0 * i as f32 / 48000.0).sin())
            .collect();
        let (y, rate) = downsample(&x, 48000);
        assert_eq!(rate, 16000.0);
        assert_eq!(y.len(), 16000);
        for (i, v) in y.iter().enumerate().skip(100).take(15000) {
            let want = (2.0 * std::f32::consts::PI * 300.0 * i as f32 / 16000.0).sin();
            assert!((v - want).abs() < 0.01, "{i}: {v} vs {want}");
        }
    }

    #[test]
    fn dot_matches_plain_sum() {
        let a: Vec<f32> = (0..29).map(|i| i as f32 * 0.5).collect();
        let b: Vec<f32> = (0..29).map(|i| 3.0 - i as f32).collect();
        let plain: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!((dot(&a, &b) - plain).abs() < 1e-3);
    }
}
