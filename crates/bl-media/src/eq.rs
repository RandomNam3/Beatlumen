//! A graphic equalizer for interleaved stereo: ten peaking filters an octave
//! apart (Audio EQ Cookbook, R. Bristow-Johnson), computed in double
//! precision.
//!
//! Boosts get their headroom from the curve that actually results: the signal
//! is lowered by the highest point of the summed response, not by the sum of
//! the sliders. Switched off nothing is computed at all, so the samples stay
//! exactly as they were. Changes fade over a few milliseconds and do not click.

/// Centre frequencies of the bands in Hz.
pub const EQ_BANDS: [f32; 10] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Largest cut or boost of a band and of the preamp, in dB.
pub const EQ_MAX_DB: f32 = 12.0;

/// About one octave wide.
const Q: f64 = std::f64::consts::SQRT_2;

/// Length of the fade when something changes.
const FADE_SECONDS: f64 = 0.02;

/// What the listener has set.
#[derive(Debug, Clone, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    /// Added on top of the automatic headroom (dB).
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}

impl Default for EqSettings {
    fn default() -> EqSettings {
        EqSettings {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; 10],
        }
    }
}

impl EqSettings {
    /// True when the settings cannot change a sample.
    pub fn is_flat(&self) -> bool {
        self.preamp_db == 0.0 && self.bands_db.iter().all(|b| *b == 0.0)
    }

    /// Whether the output is processed at all.
    pub fn active(&self) -> bool {
        self.enabled && !self.is_flat()
    }

    fn sanitized(&self) -> EqSettings {
        let clamp = |v: f32| {
            if v.is_finite() {
                v.clamp(-EQ_MAX_DB, EQ_MAX_DB)
            } else {
                0.0
            }
        };
        EqSettings {
            enabled: self.enabled,
            preamp_db: clamp(self.preamp_db),
            bands_db: self.bands_db.map(clamp),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn peaking(freq: f64, gain_db: f64, rate: f64) -> Biquad {
        let a = 10f64.powf(gain_db / 40.0);
        let (s, c) = (std::f64::consts::TAU * freq / rate).sin_cos();
        let alpha = s / (2.0 * Q);
        let a0 = 1.0 + alpha / a;
        Biquad {
            b: [
                (1.0 + alpha * a) / a0,
                -2.0 * c / a0,
                (1.0 - alpha * a) / a0,
            ],
            a: [-2.0 * c / a0, (1.0 - alpha / a) / a0],
        }
    }

    fn magnitude_db(&self, freq: f64, rate: f64) -> f64 {
        let (s1, c1) = (std::f64::consts::TAU * freq / rate).sin_cos();
        let (s2, c2) = (2.0 * std::f64::consts::TAU * freq / rate).sin_cos();
        let num_re = self.b[0] + self.b[1] * c1 + self.b[2] * c2;
        let num_im = self.b[1] * s1 + self.b[2] * s2;
        let den_re = 1.0 + self.a[0] * c1 + self.a[1] * c2;
        let den_im = self.a[0] * s1 + self.a[1] * s2;
        10.0 * ((num_re * num_re + num_im * num_im).max(1e-30)
            / (den_re * den_re + den_im * den_im).max(1e-30))
        .log10()
    }
}

/// The filters of the settings at a sample rate; bands at or above Nyquist
/// cannot exist and are left out.
fn design(s: &EqSettings, rate: u32) -> Vec<Biquad> {
    let rate = rate.max(1000) as f64;
    EQ_BANDS
        .iter()
        .zip(&s.bands_db)
        .filter(|(f, g)| **g != 0.0 && (**f as f64) < rate * 0.49)
        .map(|(f, g)| Biquad::peaking(*f as f64, *g as f64, rate))
        .collect()
}

fn response(filters: &[Biquad], freq: f64, rate: f64) -> f64 {
    filters.iter().map(|b| b.magnitude_db(freq, rate)).sum()
}

/// The linear gain before the filters: the preamp, lowered by the curve's
/// highest point so that boosts cannot clip.
fn headroom_gain(s: &EqSettings, filters: &[Biquad], rate: u32) -> f64 {
    let rate = rate.max(1000) as f64;
    let top = rate * 0.49;
    let peak = (0..=512)
        .map(|i| 20.0 * (top / 20.0).powf(i as f64 / 512.0))
        .chain(EQ_BANDS.iter().map(|f| *f as f64).filter(|f| *f < top))
        .map(|f| response(filters, f, rate))
        .fold(0.0, f64::max);
    10f64.powf((s.preamp_db as f64 - peak) / 20.0)
}

/// The curve of the settings in dB at the given frequencies (filters only,
/// without the headroom), for display.
pub fn eq_response_db(s: &EqSettings, freqs: &[f32], rate: u32) -> Vec<f32> {
    let s = s.sanitized();
    let filters = design(&s, rate);
    freqs
        .iter()
        .map(|f| response(&filters, *f as f64, rate.max(1000) as f64) as f32)
        .collect()
}

/// Filters with their memory, one pair of values per filter and channel.
#[derive(Clone)]
struct Chain {
    filters: Vec<Biquad>,
    gain: f64,
    state: Vec<[[f64; 2]; 2]>,
}

impl Chain {
    fn new(s: &EqSettings, rate: u32) -> Chain {
        let filters = design(s, rate);
        let gain = headroom_gain(s, &filters, rate);
        let state = vec![[[0.0; 2]; 2]; filters.len()];
        Chain {
            filters,
            gain,
            state,
        }
    }

    #[inline]
    fn tick(&mut self, input: [f64; 2]) -> [f64; 2] {
        let mut out = [input[0] * self.gain, input[1] * self.gain];
        for (f, state) in self.filters.iter().zip(&mut self.state) {
            for (x, z) in out.iter_mut().zip(state.iter_mut()) {
                let y = f.b[0] * *x + z[0];
                z[0] = f.b[1] * *x - f.a[0] * y + z[1];
                z[1] = f.b[2] * *x - f.a[1] * y;
                *x = y;
            }
        }
        out
    }
}

/// The running equalizer.
pub struct Equalizer {
    settings: EqSettings,
    rate: u32,
    /// `None`: nothing to do.
    chain: Option<Chain>,
    /// What was active before the last change, while it fades out, with the
    /// frames done so far and the length of the fade.
    leaving: Option<(Option<Chain>, usize, usize)>,
}

impl Default for Equalizer {
    fn default() -> Equalizer {
        Equalizer::new()
    }
}

impl Equalizer {
    pub fn new() -> Equalizer {
        Equalizer {
            settings: EqSettings::default(),
            rate: 48000,
            chain: None,
            leaving: None,
        }
    }

    pub fn settings(&self) -> &EqSettings {
        &self.settings
    }

    pub fn set(&mut self, s: &EqSettings) {
        let s = s.sanitized();
        if s == self.settings {
            return;
        }
        self.settings = s;
        self.rebuild();
    }

    /// The filters are designed anew for another rate.
    pub fn set_rate(&mut self, rate: u32) {
        if rate != self.rate && rate > 0 {
            self.rate = rate;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let next = self
            .settings
            .active()
            .then(|| Chain::new(&self.settings, self.rate));
        let old = std::mem::replace(&mut self.chain, next);
        if old.is_some() || self.chain.is_some() {
            let len = (self.rate as f64 * FADE_SECONDS) as usize;
            self.leaving = Some((old, 0, len.max(1)));
        }
    }

    /// Filters interleaved stereo in place.
    pub fn process(&mut self, stereo: &mut [f32]) {
        if self.chain.is_none() && self.leaving.is_none() {
            return;
        }
        for frame in stereo.chunks_exact_mut(2) {
            let input = [frame[0] as f64, frame[1] as f64];
            let mut out = match &mut self.chain {
                Some(c) => c.tick(input),
                None => input,
            };
            if let Some((old, done, len)) = &mut self.leaving {
                let previous = match old {
                    Some(c) => c.tick(input),
                    None => input,
                };
                let k = (*done as f64 + 0.5) / *len as f64;
                out[0] = out[0] * k + previous[0] * (1.0 - k);
                out[1] = out[1] * k + previous[1] * (1.0 - k);
                *done += 1;
                if *done >= *len {
                    self.leaving = None;
                }
            }
            // The headroom covers steady tones; a transient can still overshoot.
            frame[0] = out[0].clamp(-1.0, 1.0) as f32;
            frame[1] = out[1].clamp(-1.0, 1.0) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, frames: usize, amp: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin() * amp;
                [v as f32, v as f32]
            })
            .collect()
    }

    /// Level of the last half in dB relative to `amp`.
    fn level_db(stereo: &[f32], amp: f64) -> f64 {
        let tail = &stereo[stereo.len() / 2..];
        let rms =
            (tail.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        20.0 * (rms / (amp * std::f64::consts::FRAC_1_SQRT_2)).log10()
    }

    fn with(bands: [f32; 10]) -> EqSettings {
        EqSettings {
            enabled: true,
            preamp_db: 0.0,
            bands_db: bands,
        }
    }

    #[test]
    fn off_and_flat_leave_every_sample_exactly() {
        let original = tone(997.0, 44100, 4410, 0.5);
        for s in [
            EqSettings::default(),
            with([0.0; 10]),
            EqSettings {
                enabled: false,
                preamp_db: -6.0,
                bands_db: [6.0; 10],
            },
        ] {
            let mut eq = Equalizer::new();
            eq.set_rate(44100);
            eq.set(&s);
            let mut out = original.clone();
            eq.process(&mut out);
            assert_eq!(out, original);
        }
    }

    #[test]
    fn a_band_changes_its_frequency_by_what_the_curve_says() {
        let rate = 48000;
        let mut bands = [0.0; 10];
        bands[5] = -9.0;
        let s = with(bands);
        let mut eq = Equalizer::new();
        eq.set_rate(rate);
        eq.set(&s);
        for freq in [125.0f32, 1000.0, 2000.0, 8000.0] {
            let mut out = tone(freq as f64, rate, rate as usize, 0.25);
            eq.process(&mut out);
            let want = eq_response_db(&s, &[freq], rate)[0] as f64;
            let got = level_db(&out, 0.25);
            assert!(
                (got - want).abs() < 0.05,
                "{freq} Hz: {got} dB, want {want} dB"
            );
        }
        assert!(eq_response_db(&s, &[1000.0], rate)[0] < -8.9);
        assert!(eq_response_db(&s, &[125.0], rate)[0].abs() < 0.2);
    }

    #[test]
    fn boosts_get_headroom_and_do_not_clip() {
        let rate = 44100;
        let s = with([12.0; 10]);
        let mut eq = Equalizer::new();
        eq.set_rate(rate);
        eq.set(&s);
        for freq in [62.5, 440.0, 1000.0, 5000.0, 15000.0] {
            // A full-scale tone stays below full scale after the fade.
            let mut out = tone(freq, rate, rate as usize, 0.999);
            eq.process(&mut out);
            let peak = out[out.len() / 2..]
                .iter()
                .fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak < 1.0, "{freq} Hz peaks at {peak}");
        }
        let mut bands = [0.0; 10];
        bands[5] = 6.0;
        let single = with(bands);
        let mut eq = Equalizer::new();
        eq.set_rate(rate);
        eq.set(&single);
        let mut out = tone(1000.0, rate, rate as usize, 0.25);
        eq.process(&mut out);
        // The boost is taken back exactly at its peak.
        assert!(level_db(&out, 0.25).abs() < 0.05);
    }

    #[test]
    fn the_preamp_is_added_and_bands_above_nyquist_do_not_exist() {
        let s = EqSettings {
            enabled: true,
            preamp_db: -6.0,
            bands_db: [0.0; 10],
        };
        let mut eq = Equalizer::new();
        eq.set_rate(48000);
        eq.set(&s);
        let mut out = tone(440.0, 48000, 48000, 0.5);
        eq.process(&mut out);
        assert!((level_db(&out, 0.5) + 6.0).abs() < 0.05);

        // 16 kHz cannot exist at 22.05 kHz.
        let mut bands = [0.0; 10];
        bands[9] = 12.0;
        assert_eq!(design(&with(bands), 22050).len(), 0);
        assert_eq!(design(&with(bands), 44100).len(), 1);
    }

    #[test]
    fn changes_do_not_click() {
        let rate = 48000;
        let mut eq = Equalizer::new();
        eq.set_rate(rate);
        let mut bands = [0.0; 10];
        bands[5] = 12.0;
        let mut signal = tone(1000.0, rate, rate as usize, 0.1);
        let (a, b) = signal.split_at_mut(rate as usize);
        eq.process(a);
        eq.set(&with(bands));
        eq.process(b);
        // Between two samples of a 1 kHz tone at this level the signal moves
        // by about 0.013; no jump far beyond the largest level reached.
        let jump = signal
            .chunks_exact(2)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).abs())
            .fold(0f32, f32::max);
        assert!(jump < 0.05, "jump of {jump}");
    }

    #[test]
    fn switching_off_ends_with_untouched_samples() {
        let rate = 48000;
        let mut eq = Equalizer::new();
        eq.set_rate(rate);
        eq.set(&with([6.0; 10]));
        let mut warm = tone(440.0, rate, 4800, 0.5);
        eq.process(&mut warm);
        eq.set(&EqSettings::default());
        // Past the fade.
        let mut fade = tone(440.0, rate, 4800, 0.5);
        eq.process(&mut fade);
        let original = tone(440.0, rate, 4800, 0.5);
        let mut out = original.clone();
        eq.process(&mut out);
        assert_eq!(out, original);
    }

    #[test]
    fn nonsense_values_are_harmless() {
        let s = EqSettings {
            enabled: true,
            preamp_db: f32::NAN,
            bands_db: [f32::INFINITY; 10],
        };
        let mut eq = Equalizer::new();
        eq.set(&s);
        assert!(eq.settings().bands_db.iter().all(|b| b.abs() <= EQ_MAX_DB));
        assert_eq!(eq.settings().preamp_db, 0.0);
        let mut out = tone(440.0, 48000, 4800, 0.5);
        eq.process(&mut out);
        assert!(out.iter().all(|v| v.is_finite()));
    }
}
