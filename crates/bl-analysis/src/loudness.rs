//! Integrated loudness after ITU-R BS.1770-4 / EBU R128 (K-weighting,
//! 400 ms blocks with 75 % overlap, absolute and relative gating) and
//! sample peak, used to play all tracks at a similar volume.

use std::f64::consts::PI;

/// Absolute gate in LUFS.
const ABSOLUTE_GATE: f64 = -70.0;
/// Relative gate in LU below the ungated loudness.
const RELATIVE_GATE: f64 = -10.0;

/// Result of a measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Integrated loudness in LUFS (None for silence).
    pub integrated: Option<f64>,
    /// Largest absolute sample value (1.0 = full scale).
    pub peak: f32,
}

impl Loudness {
    /// Linear gain that brings the track to `target` LUFS without letting
    /// its peaks exceed `max_peak` (boosts of quiet tracks are limited).
    pub fn gain_for(&self, target: f64, max_peak: f32) -> f32 {
        let Some(l) = self.integrated else {
            return 1.0;
        };
        let mut g = 10f64.powf((target - l) / 20.0) as f32;
        if self.peak > 0.0 && g * self.peak > max_peak {
            g = (max_peak / self.peak).max(g.min(1.0));
        }
        g.clamp(0.05, 8.0)
    }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
    z: [f64; 2],
}

impl Biquad {
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[1] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// K-weighting filters for one channel (high shelf + high pass), designed
/// for any sample rate (coefficients as in libebur128).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let f0 = 1681.974450955533;
    let g = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (PI * f0 / rate).tan();
    let d = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / d, (1.0 - k / q + k * k) / d],
        z: [0.0; 2],
    };
    [shelf, high_pass]
}

/// Streaming loudness meter for interleaved audio.
pub struct LoudnessMeter {
    channels: usize,
    filters: Vec<[Biquad; 2]>,
    weights: Vec<f64>,
    /// Frames per 100 ms sub-block.
    step: usize,
    frames_in_step: usize,
    acc: f64,
    /// Mean square of every 100 ms sub-block.
    steps: Vec<f64>,
    peak: f32,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: u16) -> LoudnessMeter {
        let channels = channels.max(1) as usize;
        // Channel weights: mono counts as dual mono (played on both
        // speakers), 5.1 (L R C LFE Ls Rs) skips the LFE and weights the
        // surrounds by 1.41.
        let weights = match channels {
            1 => vec![2.0],
            6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
            n => vec![1.0; n],
        };
        LoudnessMeter {
            channels,
            filters: vec![k_weighting(sample_rate.max(1) as f64); channels],
            weights,
            step: (sample_rate.max(10) / 10) as usize,
            frames_in_step: 0,
            acc: 0.0,
            steps: Vec::new(),
            peak: 0.0,
        }
    }

    /// Feeds interleaved samples.
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            let mut sum = 0.0;
            for (c, &s) in frame.iter().enumerate() {
                self.peak = self.peak.max(s.abs());
                let w = self.weights[c];
                if w == 0.0 {
                    continue;
                }
                let [f1, f2] = &mut self.filters[c];
                let y = f2.run(f1.run(s as f64));
                sum += w * y * y;
            }
            self.acc += sum;
            self.frames_in_step += 1;
            if self.frames_in_step == self.step {
                self.steps.push(self.acc / self.step as f64);
                self.acc = 0.0;
                self.frames_in_step = 0;
            }
        }
    }

    pub fn finish(&self) -> Loudness {
        // 400 ms blocks = four consecutive 100 ms sub-blocks.
        let blocks: Vec<f64> = self
            .steps
            .windows(4)
            .map(|w| w.iter().sum::<f64>() / 4.0)
            .collect();
        let lufs = |ms: f64| -0.691 + 10.0 * ms.log10();
        let gated = |min: f64| {
            let sel: Vec<f64> = blocks
                .iter()
                .copied()
                .filter(|&b| b > 0.0 && lufs(b) > min)
                .collect();
            (!sel.is_empty()).then(|| sel.iter().sum::<f64>() / sel.len() as f64)
        };
        let integrated = gated(ABSOLUTE_GATE)
            .and_then(|ungated| gated(lufs(ungated) + RELATIVE_GATE))
            .map(lufs);
        Loudness {
            integrated,
            peak: self.peak,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, channels: u16, secs: f64, freq: f64, amp: f32) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        let mut v = Vec::with_capacity(n * channels as usize);
        for i in 0..n {
            let s = amp * (i as f64 * freq * 2.0 * PI / rate as f64).sin() as f32;
            v.extend(std::iter::repeat_n(s, channels as usize));
        }
        v
    }

    #[test]
    fn ebu_reference_sine() {
        // EBU Tech 3341 case 1: stereo 1 kHz sine at -23 dBFS -> -23 LUFS.
        for rate in [44100, 48000, 96000] {
            let mut m = LoudnessMeter::new(rate, 2);
            m.push(&sine(rate, 2, 20.0, 1000.0, 10f32.powf(-23.0 / 20.0)));
            let l = m.finish().integrated.unwrap();
            assert!((l + 23.0).abs() < 0.1, "{rate} Hz: {l}");
        }
    }

    #[test]
    fn gating_and_mono() {
        // Silence around the tone does not lower the result (gates).
        let mut m = LoudnessMeter::new(48000, 2);
        m.push(&vec![0.0; 48000 * 2 * 10]);
        m.push(&sine(48000, 2, 10.0, 1000.0, 0.1));
        let l = m.finish();
        assert!((l.integrated.unwrap() + 20.0).abs() < 0.1);
        assert!((l.peak - 0.1).abs() < 1e-3);
        // Mono is measured like the same signal on both speakers.
        let mut mono = LoudnessMeter::new(48000, 1);
        mono.push(&sine(48000, 1, 10.0, 1000.0, 0.1));
        assert!((mono.finish().integrated.unwrap() + 20.0).abs() < 0.1);
        assert_eq!(LoudnessMeter::new(48000, 2).finish().integrated, None);
    }

    #[test]
    fn gain_limits() {
        let loud = Loudness {
            integrated: Some(-8.0),
            peak: 1.0,
        };
        assert!((loud.gain_for(-16.0, 1.0) - 10f32.powf(-8.0 / 20.0)).abs() < 1e-4);
        // A quiet track is boosted only up to its peak.
        let quiet = Loudness {
            integrated: Some(-28.0),
            peak: 0.5,
        };
        assert!((quiet.gain_for(-16.0, 1.0) - 2.0).abs() < 1e-4);
        assert_eq!(
            Loudness {
                integrated: None,
                peak: 0.0
            }
            .gain_for(-16.0, 1.0),
            1.0
        );
    }
}
