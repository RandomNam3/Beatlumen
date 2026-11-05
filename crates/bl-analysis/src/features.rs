//! What a whole song sounds like, for labels and recommendations: tempo,
//! key, energy, how clear its beat is and how bright it sounds.
//!
//! * Tempo: autocorrelation of the spectral flux, with a preference for
//!   tempos around 120 BPM (half and double tempo fit almost as well).
//! * Key: mean chroma correlated with the Krumhansl-Kessler key profiles.

use crate::fft::{blackman_window, RealFft};

/// Seconds looked at, from the middle of the song (intros and fade-outs
/// say little about it).
const WINDOW_SECONDS: f64 = 120.0;
/// Shorter than this tells nothing (seconds).
const MIN_SECONDS: f64 = 4.0;
const MIN_BPM: f64 = 50.0;
const MAX_BPM: f64 = 210.0;

/// Krumhansl-Kessler key profiles, starting at the tonic.
const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongFeatures {
    pub bpm: f32,
    /// How clearly the beat stands out (0..1).
    pub beat: f32,
    /// Pitch class of the key (0 = C).
    pub key: u8,
    pub minor: bool,
    /// How sure the key is (0..1).
    pub key_confidence: f32,
    /// Loud and busy (0..1).
    pub energy: f32,
    /// Spectral centroid on a log scale (0..1).
    pub brightness: f32,
    /// Difference between quiet and loud parts (0..1).
    pub dynamics: f32,
}

/// Analyses mono samples; `None` for silence or a clip too short to tell.
pub fn song_features(samples: &[f32], rate: u32) -> Option<SongFeatures> {
    if rate == 0 || (samples.len() as f64) < MIN_SECONDS * rate as f64 {
        return None;
    }
    let window = ((WINDOW_SECONDS * rate as f64) as usize).min(samples.len());
    let start = (samples.len() - window) / 2;
    let samples = &samples[start..start + window];

    // About 46 ms per frame at any rate, hops of a quarter of that.
    let n = ((rate as f64 * 0.046) as usize)
        .next_power_of_two()
        .max(256);
    let hop = n / 4;
    let frames = (samples.len().saturating_sub(n)) / hop;
    if frames < 16 {
        return None;
    }
    let bin_hz = rate as f32 / n as f32;
    let win = blackman_window(n);
    let mut fft = RealFft::new(n);
    let mut amps = vec![0f32; n / 2];
    let mut prev = vec![0f32; n / 2];
    let mut scratch = Vec::new();
    let mut buf = vec![0f32; n];

    let mut flux = Vec::with_capacity(frames);
    let mut rms = Vec::with_capacity(frames);
    let mut centroid_sum = 0f64;
    let mut centroid_weight = 0f64;
    for i in 0..frames {
        let frame = &samples[i * hop..i * hop + n];
        let mut e = 0f32;
        for (b, (s, w)) in buf.iter_mut().zip(frame.iter().zip(&win)) {
            *b = s * w;
            e += s * s;
        }
        rms.push((e / n as f32).sqrt());
        fft.amplitudes(&buf, &mut amps, &mut scratch);
        let mut onset = 0f32;
        for b in 1..amps.len() {
            let v = (1.0 + 1000.0 * amps[b]).ln();
            if v > prev[b] {
                onset += v - prev[b];
            }
            prev[b] = v;
        }
        flux.push(onset);
        // Brightness changes slowly: every fourth frame is enough.
        if i % 4 == 0 {
            for (b, a) in amps.iter().enumerate() {
                centroid_sum += (*a as f64) * (b as f64 * bin_hz as f64);
                centroid_weight += *a as f64;
            }
        }
    }
    let peak_rms = rms.iter().cloned().fold(0f32, f32::max);
    if peak_rms < 1e-4 {
        return None;
    }

    let frame_rate = rate as f64 / hop as f64;
    let (bpm, beat) = tempo(&flux, frame_rate);
    let (key, minor, key_confidence) = key_of(&chroma(samples, rate));

    let mean_rms = (rms.iter().map(|r| r * r).sum::<f32>() / rms.len() as f32).sqrt();
    let loud = ((20.0 * mean_rms.max(1e-6).log10() + 32.0) / 24.0).clamp(0.0, 1.0);
    let mean_flux = flux.iter().sum::<f32>() / flux.len() as f32;
    let busy = (mean_flux / (n as f32 / 2.0) * 8.0).clamp(0.0, 1.0);
    let energy = 0.65 * loud + 0.35 * busy;

    let centroid = (centroid_sum / centroid_weight.max(1e-9)) as f32;
    let brightness =
        ((centroid.max(1.0) / 300.0).log2() / (6000.0f32 / 300.0).log2()).clamp(0.0, 1.0);

    let mut sorted: Vec<f32> = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let low = sorted[sorted.len() / 10].max(1e-5);
    let high = sorted[sorted.len() * 95 / 100].max(1e-5);
    let dynamics = (20.0 * (high / low).log10() / 24.0).clamp(0.0, 1.0);

    Some(SongFeatures {
        bpm: bpm as f32,
        beat,
        key,
        minor,
        key_confidence,
        energy,
        brightness,
        dynamics,
    })
}

/// How much of each pitch class sounds. Long frames (about 190 ms) tell low
/// notes a semitone apart.
fn chroma(samples: &[f32], rate: u32) -> [f32; 12] {
    let n = ((rate as f64 * 0.186) as usize)
        .next_power_of_two()
        .max(1024);
    let bin_hz = rate as f32 / n as f32;
    let pitch_class: Vec<Option<(usize, f32)>> = (0..n / 2)
        .map(|b| {
            let f = b as f32 * bin_hz;
            if !(65.0..=2000.0).contains(&f) {
                return None;
            }
            // Bins close to a semitone count more than those between two.
            let note = 69.0 + 12.0 * (f / 440.0).log2();
            let nearest = note.round();
            let closeness = 1.0 - (note - nearest).abs();
            Some((nearest.rem_euclid(12.0) as usize, closeness * closeness))
        })
        .collect();
    let win = blackman_window(n);
    let mut fft = RealFft::new(n);
    let mut amps = vec![0f32; n / 2];
    let mut scratch = Vec::new();
    let mut buf = vec![0f32; n];
    let mut chroma = [0f32; 12];
    for frame in samples.chunks_exact(n) {
        for (b, (s, w)) in buf.iter_mut().zip(frame.iter().zip(&win)) {
            *b = s * w;
        }
        fft.amplitudes(&buf, &mut amps, &mut scratch);
        for (b, a) in amps.iter().enumerate() {
            if let Some((pc, weight)) = pitch_class[b] {
                chroma[pc] += a * weight;
            }
        }
    }
    chroma
}

/// Tempo in BPM and how clear the beat is, from an onset curve.
fn tempo(flux: &[f32], frame_rate: f64) -> (f64, f32) {
    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let curve: Vec<f32> = flux.iter().map(|v| v - mean).collect();
    let lag_min = (frame_rate * 60.0 / MAX_BPM).floor().max(1.0) as usize;
    let lag_max = ((frame_rate * 60.0 / MIN_BPM).ceil() as usize).min(curve.len() / 2);
    if lag_max <= lag_min {
        return (120.0, 0.0);
    }
    let ac = |lag: usize| -> f32 {
        curve[..curve.len() - lag]
            .iter()
            .zip(&curve[lag..])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / (curve.len() - lag) as f32
    };
    let zero = curve.iter().map(|v| v * v).sum::<f32>() / curve.len() as f32;
    if zero <= 0.0 {
        return (120.0, 0.0);
    }
    let values: Vec<f32> = (lag_min..=lag_max).map(ac).collect();
    let at = |lag: usize| -> f32 {
        if (lag_min..=lag_max).contains(&lag) {
            values[lag - lag_min]
        } else {
            0.0
        }
    };
    let mut best = (lag_min, f32::MIN);
    for lag in lag_min..=lag_max {
        let bpm = 60.0 * frame_rate / lag as f64;
        // Tempos far from 120 BPM need a clearly better fit.
        let octaves = (bpm / 120.0).log2();
        let prior = (-octaves * octaves / (2.0 * 0.8 * 0.8)).exp() as f32;
        let score = (at(lag) + 0.5 * at(lag * 2) + 0.25 * at(lag * 4)) * prior;
        if score > best.1 {
            best = (lag, score);
        }
    }
    // Between the frames: a parabola through the peak.
    let lag = best.0;
    let (l, c, r) = (at(lag.saturating_sub(1)), at(lag), at(lag + 1));
    let denom = l - 2.0 * c + r;
    let exact = if denom.abs() > 1e-9 && lag > lag_min && lag < lag_max {
        lag as f64 + (0.5 * (l - r) / denom).clamp(-0.5, 0.5) as f64
    } else {
        lag as f64
    };
    let bpm = 60.0 * frame_rate / exact;
    (bpm, (c / zero).clamp(0.0, 1.0))
}

/// The best fitting key: (pitch class, minor, confidence).
fn key_of(chroma: &[f32; 12]) -> (u8, bool, f32) {
    let correlation = |profile: &[f32; 12], tonic: usize| -> f32 {
        let pm = profile.iter().sum::<f32>() / 12.0;
        let cm = chroma.iter().sum::<f32>() / 12.0;
        let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
        for i in 0..12 {
            let x = chroma[(tonic + i) % 12] - cm;
            let y = profile[i] - pm;
            xy += x * y;
            xx += x * x;
            yy += y * y;
        }
        if xx <= 0.0 || yy <= 0.0 {
            0.0
        } else {
            xy / (xx * yy).sqrt()
        }
    };
    let mut scored: Vec<(f32, u8, bool)> = Vec::with_capacity(24);
    for tonic in 0..12 {
        scored.push((correlation(&MAJOR, tonic), tonic as u8, false));
        scored.push((correlation(&MINOR, tonic), tonic as u8, true));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let confidence = (scored[0].0 - scored[1].0).clamp(0.0, 1.0) * 4.0;
    (scored[0].1, scored[0].2, confidence.min(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 22050;

    fn clicks(bpm: f64, secs: f64) -> Vec<f32> {
        let n = (secs * RATE as f64) as usize;
        let period = (60.0 / bpm * RATE as f64) as usize;
        let mut v = vec![0f32; n];
        for start in (0..n).step_by(period) {
            for i in 0..600.min(n - start) {
                let t = i as f32 / RATE as f32;
                v[start + i] = (t * 800.0 * std::f32::consts::TAU).sin() * (-t * 60.0).exp() * 0.8;
            }
        }
        v
    }

    fn chord(notes: &[f32], secs: f64, out: &mut Vec<f32>) {
        let n = (secs * RATE as f64) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let s: f32 = notes
                .iter()
                .map(|m| (t * 440.0 * 2f32.powf((m - 69.0) / 12.0) * std::f32::consts::TAU).sin())
                .sum();
            out.push(s * 0.15);
        }
    }

    #[test]
    fn finds_the_tempo() {
        for bpm in [90.0, 120.0, 140.0] {
            let f = song_features(&clicks(bpm, 30.0), RATE).unwrap();
            assert!((f.bpm as f64 - bpm).abs() < 2.0, "{bpm}: {f:?}");
            assert!(f.beat > 0.3, "{f:?}");
        }
    }

    #[test]
    fn finds_major_and_minor_keys() {
        // I IV V I in C major.
        let mut c = Vec::new();
        for _ in 0..4 {
            chord(&[60.0, 64.0, 67.0], 1.0, &mut c);
            chord(&[65.0, 69.0, 72.0], 1.0, &mut c);
            chord(&[67.0, 71.0, 74.0], 1.0, &mut c);
            chord(&[60.0, 64.0, 67.0], 1.0, &mut c);
        }
        let f = song_features(&c, RATE).unwrap();
        assert_eq!((f.key, f.minor), (0, false), "{f:?}");
        // i iv V i in A minor.
        let mut a = Vec::new();
        for _ in 0..4 {
            chord(&[57.0, 60.0, 64.0], 1.0, &mut a);
            chord(&[62.0, 65.0, 69.0], 1.0, &mut a);
            chord(&[64.0, 68.0, 71.0], 1.0, &mut a);
            chord(&[57.0, 60.0, 64.0], 1.0, &mut a);
        }
        let f = song_features(&a, RATE).unwrap();
        assert_eq!((f.key, f.minor), (9, true), "{f:?}");
    }

    #[test]
    fn bright_and_loud_differ_from_dark_and_quiet() {
        let mut dark = Vec::new();
        chord(&[40.0], 20.0, &mut dark);
        for s in &mut dark {
            *s *= 0.2;
        }
        let mut seed = 1u32;
        let bright: Vec<f32> = (0..RATE as usize * 20)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 16) as f32 / 32768.0 - 1.0
            })
            .collect();
        let d = song_features(&dark, RATE).unwrap();
        let b = song_features(&bright, RATE).unwrap();
        assert!(b.brightness > d.brightness + 0.3, "{d:?} {b:?}");
        assert!(b.energy > d.energy + 0.2, "{d:?} {b:?}");
    }

    #[test]
    fn nothing_to_tell() {
        assert!(song_features(&[], RATE).is_none());
        assert!(song_features(&vec![0.0; RATE as usize * 10], RATE).is_none());
        assert!(song_features(&vec![0.5; 100], RATE).is_none());
    }
}
