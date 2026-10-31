//! The melody of a whole song (probabilistic YIN).
//!
//! Each frame gets period candidates: for 100 thresholds, weighted by a
//! beta distribution, YIN picks a trough of d'(τ) under the threshold
//! (earlier troughs more likely). Their summed weight is how likely the
//! frame is voiced. An HMM over pitch bins, each voiced or unvoiced, then
//! finds the most likely path (Viterbi): the pitch moves smoothly, and the
//! voicing changes rarely. The jump cost grows linearly with the interval,
//! so the best predecessor of every bin comes from two passes over the
//! bins (a distance transform) instead of a sum over all pairs.

use crate::dsp;

#[derive(Debug, Clone)]
pub struct PyinOptions {
    /// Lowest and highest pitch (Hz).
    pub fmin: f32,
    pub fmax: f32,
    /// Seconds between frames.
    pub hop: f64,
    /// Length of the compared window (seconds); the frame also reaches one
    /// period of `fmin` further.
    pub window: f64,
    /// Width of the HMM's pitch bins (semitones).
    pub bin: f32,
    /// Probability of switching between voiced and unvoiced per frame.
    pub switch_prob: f32,
    /// Log-probability cost of a pitch move per semitone and frame.
    pub jump_cost: f32,
    /// Frames quieter than this many dB below the loud frames (95th
    /// percentile) of the song have no candidates.
    pub silence_db: f32,
    /// Frames quieter than this (dBFS) have no candidates either.
    pub floor_db: f32,
}

impl Default for PyinOptions {
    fn default() -> Self {
        PyinOptions {
            fmin: 60.0,
            fmax: 1100.0,
            hop: 0.01,
            window: 0.032,
            bin: 0.2,
            switch_prob: 0.01,
            jump_cost: 0.6,
            silence_db: 40.0,
            floor_db: -60.0,
        }
    }
}

/// One frame of the melody.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Centre of the frame (seconds).
    pub time: f64,
    /// Pitch (Hz), `None` where nobody sings.
    pub f0: Option<f32>,
    /// How likely the frame is voiced, from the candidates alone (0…1).
    pub voiced_prob: f32,
}

impl Frame {
    /// The pitch as a fractional MIDI note.
    pub fn midi(&self) -> Option<f32> {
        self.f0.map(crate::hz_to_midi)
    }
}

/// Number of thresholds and the weight of a trough's rank among those
/// under a threshold, as in pYIN.
const THRESHOLDS: usize = 100;
const BOLTZMANN: f32 = 2.0;
/// Share of a threshold's weight given to the lowest trough when none is
/// under it.
const NO_TROUGH: f32 = 0.01;

/// Weights of the thresholds 0.01, 0.02, …, 1.00: a beta(2, 18)
/// distribution (mean 0.1).
fn threshold_weights() -> [f32; THRESHOLDS] {
    let mut w = [0f32; THRESHOLDS];
    // CDF of beta(2, 18): 1 − (1−x)^18 (1 + 18x).
    let cdf = |x: f64| 1.0 - (1.0 - x).powi(18) * (1.0 + 18.0 * x);
    let mut prev = 0.0;
    for (i, v) in w.iter_mut().enumerate() {
        let c = cdf((i + 1) as f64 / THRESHOLDS as f64);
        *v = (c - prev) as f32;
        prev = c;
    }
    w
}

/// Period candidates of one frame, from d'(τ): (τ with sub-sample offset,
/// probability). Their probabilities sum to how likely the frame is voiced.
fn candidates(d: &[f32], tau_min: usize, weights: &[f32; THRESHOLDS], out: &mut Vec<(f32, f32)>) {
    out.clear();
    let tau_max = d.len() - 2;
    // Troughs in the period range, with their depth.
    let mut troughs: Vec<(usize, f32)> = Vec::new();
    for tau in tau_min..=tau_max {
        let left = if tau == tau_min {
            f32::INFINITY
        } else {
            d[tau - 1]
        };
        if d[tau] < left && d[tau] <= d[tau + 1] {
            troughs.push((tau, d[tau]));
        }
    }
    if troughs.is_empty() {
        return;
    }
    let mut probs = vec![0f32; troughs.len()];
    let lowest = troughs
        .iter()
        .enumerate()
        .min_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
        .map_or(0, |(i, _)| i);
    let decay = (-BOLTZMANN).exp();
    let mut under: Vec<usize> = Vec::with_capacity(troughs.len());
    for (i, &w) in weights.iter().enumerate() {
        let s = (i + 1) as f32 / THRESHOLDS as f32;
        under.clear();
        under.extend((0..troughs.len()).filter(|&k| troughs[k].1 < s));
        if under.is_empty() {
            probs[lowest] += NO_TROUGH * w;
            continue;
        }
        // Earlier troughs are more likely: e^(−2·rank), normalized.
        let norm = (1.0 - decay) / (1.0 - decay.powi(under.len() as i32));
        let mut p = norm;
        for &k in &under {
            probs[k] += w * p;
            p *= decay;
        }
    }
    for (k, &(tau, _)) in troughs.iter().enumerate() {
        if probs[k] > 1e-6 {
            out.push((tau as f32 + dsp::parabolic(d, tau), probs[k]));
        }
    }
}

/// The melody of mono `samples` at `rate` Hz, one frame per `opts.hop`
/// from time 0.
pub fn track(samples: &[f32], rate: u32, opts: &PyinOptions) -> Vec<Frame> {
    let (x, sr) = dsp::downsample(samples, rate);
    let hop = ((opts.hop * sr).round() as usize).max(1);
    let w = ((opts.window * sr).round() as usize).max(16);
    let fmax = opts.fmax.min(sr as f32 * 0.25);
    let tau_min = ((sr as f32 / fmax).floor() as usize).max(2);
    let tau_max = ((sr as f32 / opts.fmin).ceil() as usize).max(tau_min + 2);
    let n_frames = x.len().div_ceil(hop);
    if n_frames == 0 {
        return Vec::new();
    }
    // Frame i compares the window centred on i·hop.
    let mut padded = vec![0f32; w / 2];
    padded.extend_from_slice(&x);
    padded.resize(n_frames * hop + w + tau_max + 2, 0.0);

    // Loudness gate.
    let levels: Vec<f32> = (0..n_frames)
        .map(|i| dsp::rms(&padded[i * hop..i * hop + w]))
        .collect();
    let mut sorted: Vec<f32> = levels.iter().copied().filter(|&l| l > 0.0).collect();
    sorted.sort_by(f32::total_cmp);
    let loud = sorted
        .get((sorted.len() as f32 * 0.95) as usize)
        .or(sorted.last())
        .copied()
        .unwrap_or(0.0);
    let gate = (loud * 10f32.powf(-opts.silence_db / 20.0)).max(10f32.powf(opts.floor_db / 20.0));

    // Candidates per frame, as (pitch bin, probability, Hz), one per bin.
    let n_bins = ((12.0 * (fmax / opts.fmin).log2() / opts.bin).ceil() as usize + 1).max(2);
    let bin_of = |hz: f32| -> usize {
        ((12.0 * (hz / opts.fmin).log2() / opts.bin).round().max(0.0) as usize).min(n_bins - 1)
    };
    let weights = threshold_weights();
    let mut d = vec![0f32; tau_max + 2];
    let mut cands: Vec<(f32, f32)> = Vec::new();
    let mut obs: Vec<(u16, f32, f32)> = Vec::new();
    let mut obs_start: Vec<u32> = Vec::with_capacity(n_frames + 1);
    let mut voiced_mass: Vec<f32> = Vec::with_capacity(n_frames);
    for (i, &level) in levels.iter().enumerate() {
        obs_start.push(obs.len() as u32);
        if level < gate {
            voiced_mass.push(0.0);
            continue;
        }
        dsp::cmnd(&padded[i * hop..], w, &mut d);
        candidates(&d, tau_min, &weights, &mut cands);
        let first = obs.len();
        let mut mass = 0.0;
        for &(tau, p) in &cands {
            let hz = sr as f32 / tau;
            if hz < opts.fmin * 0.97 || hz > fmax * 1.03 {
                continue;
            }
            mass += p;
            let b = bin_of(hz) as u16;
            match obs[first..].iter_mut().find(|o| o.0 == b) {
                Some(o) => {
                    if p > o.1 {
                        o.2 = hz;
                    }
                    o.1 += p;
                }
                None => obs.push((b, p, hz)),
            }
        }
        voiced_mass.push(mass.min(1.0));
    }
    obs_start.push(obs.len() as u32);

    let path = viterbi(&obs, &obs_start, &voiced_mass, n_bins, opts);
    (0..n_frames)
        .map(|i| {
            let f0 = path[i].and_then(|b| {
                obs[obs_start[i] as usize..obs_start[i + 1] as usize]
                    .iter()
                    .find(|o| o.0 as usize == b)
                    .map(|o| o.2)
            });
            Frame {
                time: (i * hop) as f64 / sr,
                f0,
                voiced_prob: voiced_mass[i],
            }
        })
        .collect()
}

/// max_j (s[j] − cost·|k − j|) for every k, and the j it comes from.
fn distance_transform(s: &[f32], cost: f32, best: &mut [f32], from: &mut [u16]) {
    best.copy_from_slice(s);
    for (k, f) in from.iter_mut().enumerate() {
        *f = k as u16;
    }
    for k in 1..s.len() {
        if best[k - 1] - cost > best[k] {
            best[k] = best[k - 1] - cost;
            from[k] = from[k - 1];
        }
    }
    for k in (0..s.len() - 1).rev() {
        if best[k + 1] - cost > best[k] {
            best[k] = best[k + 1] - cost;
            from[k] = from[k + 1];
        }
    }
}

/// The most likely pitch bin per frame (`None`: unvoiced).
fn viterbi(
    obs: &[(u16, f32, f32)],
    obs_start: &[u32],
    voiced_mass: &[f32],
    n: usize,
    opts: &PyinOptions,
) -> Vec<Option<usize>> {
    let frames = voiced_mass.len();
    const UNVOICED: u16 = 0x8000;
    let cost = opts.jump_cost * opts.bin;
    let stay = (1.0 - opts.switch_prob).ln();
    let switch = opts.switch_prob.ln();
    let unvoiced_obs = |t: usize| ((1.0 - voiced_mass[t]).max(1e-6) / n as f32).ln();
    let voiced_obs = |t: usize, v: &mut [f32]| {
        v.fill(f32::NEG_INFINITY);
        for o in &obs[obs_start[t] as usize..obs_start[t + 1] as usize] {
            v[o.0 as usize] = o.1.max(1e-9).ln();
        }
    };
    // Scores of the voiced (v) and unvoiced (u) states.
    let mut v = vec![0f32; n];
    let mut u = vec![unvoiced_obs(0); n];
    voiced_obs(0, &mut v);
    // Where each state came from: bin, plus UNVOICED if from an unvoiced
    // state. Voiced states first, then the unvoiced ones.
    let mut back = vec![0u16; frames * 2 * n];
    let (mut dv, mut du) = (vec![0f32; n], vec![0f32; n]);
    let (mut fv, mut fu) = (vec![0u16; n], vec![0u16; n]);
    let mut ov = vec![0f32; n];
    for t in 1..frames {
        distance_transform(&v, cost, &mut dv, &mut fv);
        distance_transform(&u, cost, &mut du, &mut fu);
        voiced_obs(t, &mut ov);
        let ou = unvoiced_obs(t);
        let b = &mut back[t * 2 * n..(t + 1) * 2 * n];
        let mut top = f32::NEG_INFINITY;
        for k in 0..n {
            let (from_v, from_u) = (dv[k] + stay, du[k] + switch);
            if from_v >= from_u {
                v[k] = from_v + ov[k];
                b[k] = fv[k];
            } else {
                v[k] = from_u + ov[k];
                b[k] = fu[k] | UNVOICED;
            }
            let (from_u, from_v) = (du[k] + stay, dv[k] + switch);
            if from_u >= from_v {
                u[k] = from_u + ou;
                b[n + k] = fu[k] | UNVOICED;
            } else {
                u[k] = from_v + ou;
                b[n + k] = fv[k];
            }
            top = top.max(v[k]).max(u[k]);
        }
        // Keep the scores near zero (f32 precision).
        for s in v.iter_mut().chain(u.iter_mut()) {
            *s -= top;
        }
    }
    // Best last state, then back.
    let (mut bin, mut voiced) = (0usize, false);
    let mut top = f32::NEG_INFINITY;
    for k in 0..n {
        if u[k] > top {
            (top, bin, voiced) = (u[k], k, false);
        }
        if v[k] > top {
            (top, bin, voiced) = (v[k], k, true);
        }
    }
    let mut path = vec![None; frames];
    for t in (0..frames).rev() {
        path[t] = voiced.then_some(bin);
        if t == 0 {
            break;
        }
        let from = back[t * 2 * n + if voiced { bin } else { n + bin }];
        voiced = from & UNVOICED == 0;
        bin = (from & !UNVOICED) as usize;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: u32 = 16000;

    /// A tone with `harmonics` (amplitude per harmonic) following `f(t)`.
    fn tone(secs: f32, rate: u32, f: impl Fn(f32) -> f32, harmonics: &[f32]) -> Vec<f32> {
        let mut phase = 0f32;
        (0..(secs * rate as f32) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                phase += 2.0 * PI * f(t) / rate as f32;
                harmonics
                    .iter()
                    .enumerate()
                    .map(|(h, a)| a * ((h + 1) as f32 * phase).sin())
                    .sum::<f32>()
                    * 0.3
            })
            .collect()
    }

    fn saw() -> Vec<f32> {
        (1..=12).map(|h| 1.0 / h as f32).collect()
    }

    /// Mean absolute error in cents of the voiced frames in `range`
    /// against `f`, and the share of those frames that are voiced.
    fn check(frames: &[Frame], range: (f64, f64), f: impl Fn(f32) -> f32) -> (f32, f32) {
        let sel: Vec<&Frame> = frames
            .iter()
            .filter(|fr| fr.time >= range.0 && fr.time <= range.1)
            .collect();
        let voiced: Vec<f32> = sel
            .iter()
            .filter_map(|fr| fr.f0.map(|hz| 1200.0 * (hz / f(fr.time as f32)).log2()))
            .collect();
        let err = voiced.iter().map(|c| c.abs()).sum::<f32>() / voiced.len().max(1) as f32;
        (err, voiced.len() as f32 / sel.len().max(1) as f32)
    }

    #[test]
    fn steady_sine_and_saw() {
        for (hz, h) in [
            (220.0, vec![1.0]),
            (110.0, saw()),
            (880.0, vec![1.0]),
            (65.0, saw()),
        ] {
            let x = tone(1.0, SR, |_| hz, &h);
            let fr = track(&x, SR, &PyinOptions::default());
            let (err, voiced) = check(&fr, (0.1, 0.9), |_| hz);
            assert!(
                err < 10.0 && voiced > 0.95,
                "{hz} Hz: {err} cents, {voiced} voiced"
            );
        }
    }

    #[test]
    fn vibrato_and_glide() {
        // Saw at 300 Hz with ±50 cents vibrato at 5.5 Hz.
        let vib = |t: f32| 300.0 * (0.5 / 12.0 * (2.0 * PI * 5.5 * t).sin()).exp2();
        let fr = track(&tone(2.0, SR, vib, &saw()), SR, &PyinOptions::default());
        let (err, voiced) = check(&fr, (0.1, 1.9), vib);
        assert!(
            err < 15.0 && voiced > 0.95,
            "vibrato: {err} cents, {voiced}"
        );
        // Glide over an octave and a half in a second, at 44.1 kHz.
        let glide = |t: f32| 150.0 * (1.5 * t).exp2();
        let fr = track(
            &tone(1.0, 44100, glide, &saw()),
            44100,
            &PyinOptions::default(),
        );
        let (err, voiced) = check(&fr, (0.05, 0.95), glide);
        assert!(err < 15.0 && voiced > 0.95, "glide: {err} cents, {voiced}");
    }

    #[test]
    fn silence_and_noise_are_unvoiced() {
        let fr = track(&vec![0.0; 16000], SR, &PyinOptions::default());
        assert!(fr.iter().all(|f| f.f0.is_none()));
        // White noise (a simple LCG).
        let mut s = 12345u32;
        let noise: Vec<f32> = (0..32000)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let fr = track(&noise, SR, &PyinOptions::default());
        let voiced = fr.iter().filter(|f| f.f0.is_some()).count();
        assert!(voiced * 10 < fr.len(), "{voiced} of {} voiced", fr.len());
    }

    #[test]
    fn tone_after_silence_and_frame_times() {
        let mut x = vec![0f32; 8000];
        x.extend(tone(1.0, SR, |_| 440.0, &[1.0]));
        x.extend(vec![0f32; 8000]);
        let fr = track(&x, SR, &PyinOptions::default());
        assert!((fr[100].time - 1.0).abs() < 1e-9);
        let on = fr.iter().position(|f| f.f0.is_some()).unwrap();
        let off = fr.iter().rposition(|f| f.f0.is_some()).unwrap();
        assert!(
            (fr[on].time - 0.5).abs() < 0.04,
            "starts at {}",
            fr[on].time
        );
        assert!(
            (fr[off].time - 1.5).abs() < 0.04,
            "ends at {}",
            fr[off].time
        );
    }

    #[test]
    fn octave_ambiguous_signals() {
        // Missing fundamental: harmonics 2…6 of 150 Hz still sound at 150.
        let x = tone(1.0, SR, |_| 150.0, &[0.0, 1.0, 0.8, 0.6, 0.5, 0.4]);
        let (err, voiced) = check(&track(&x, SR, &PyinOptions::default()), (0.1, 0.9), |_| {
            150.0
        });
        assert!(
            err < 10.0 && voiced > 0.9,
            "missing fundamental: {err}, {voiced}"
        );
        // Strong second harmonic: still the fundamental, not the octave.
        let x = tone(1.0, SR, |_| 200.0, &[0.3, 1.0]);
        let (err, voiced) = check(&track(&x, SR, &PyinOptions::default()), (0.1, 0.9), |_| {
            200.0
        });
        assert!(err < 10.0 && voiced > 0.9, "strong octave: {err}, {voiced}");
        // Weak subharmonic (every other period a bit different): the
        // HMM stays on 330 Hz rather than flipping between octaves.
        let x = tone(1.0, SR, |_| 165.0, &[0.08, 1.0, 0.0, 0.5]);
        let fr = track(&x, SR, &PyinOptions::default());
        let hz: Vec<f32> = fr[10..90].iter().filter_map(|f| f.f0).collect();
        let low = hz.iter().filter(|&&f| f < 240.0).count();
        assert!(
            low == 0 || low == hz.len(),
            "flips between octaves: {low} of {}",
            hz.len()
        );
    }
}
