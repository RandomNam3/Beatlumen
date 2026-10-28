//! Offline whole-track analysis.
//!
//! The pipeline mirrors the behaviour of the original Hyperchroma analyser so
//! that existing `.hfx` presets (whose trigger sensitivities/thresholds were
//! tuned against it) behave the same:
//!
//! 1. Spectrogram: Blackman-windowed FFT frames with 50 % overlap, amplitude
//!    in dB mapped to [0, 1] with a soft floor ("raw" spectrum).
//! 2. Harmonic/percussive separation by median filtering along time and
//!    frequency with a binary mask.
//! 3. Energy curve: RMS per spectral frame.
//! 4. Novelty curves: positive spectral flux within a frequency band, minus a
//!    local average, divided by a normalisation factor.
//! 5. Chroma vectors for raw/harmonic/percussive spectra, median smoothed and
//!    down-sampled; self-similarity matrices; checkerboard kernel novelty
//!    (Foote) for "major" (long window) and "minor" (short window) structure
//!    changes; peak picking and merging into a transition curve whose values
//!    are 1.0 (major), 0.5 (minor) or 0.0.

use crate::fft::{blackman_window, RealFft};

/// Which spectrogram a novelty curve is computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spectrum {
    Raw,
    Harmonic,
    Percussive,
}

impl Spectrum {
    pub fn name(self) -> &'static str {
        match self {
            Spectrum::Raw => "Raw",
            Spectrum::Harmonic => "Harmonic",
            Spectrum::Percussive => "Percussive",
        }
    }
    pub fn from_name(s: &str) -> Option<Spectrum> {
        match s {
            "Raw" => Some(Spectrum::Raw),
            "Harmonic" => Some(Spectrum::Harmonic),
            "Percussive" => Some(Spectrum::Percussive),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoveltyConfig {
    pub spectrum: Spectrum,
    pub normalization: f32,
    pub start_freq: f32,
    pub end_freq: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisConfig {
    pub fft_size: usize,
    /// Median filter radius (frames / bins) for harmonic/percussive separation.
    pub spectral_smoothing: usize,
    /// Separation factor for the harmonic/percussive mask.
    pub spectral_separation: f32,
    /// Median filter radius (frames) for chroma smoothing.
    pub chroma_smoothing: usize,
    /// Chroma down-sampling factor (spectral frames per chroma frame).
    pub chroma_down_sampling: usize,
    /// Checkerboard kernel radius (chroma frames) for major transitions.
    pub major_transition_window: usize,
    /// Checkerboard kernel radius (chroma frames) for minor transitions.
    pub minor_transition_window: usize,
    /// Novelty curves to compute (typically Low, Mid, High bands).
    pub novelty: Vec<NoveltyConfig>,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        AnalysisConfig {
            fft_size: 4096,
            spectral_smoothing: 4,
            spectral_separation: 3.0,
            chroma_smoothing: 30,
            chroma_down_sampling: 10,
            major_transition_window: 30,
            minor_transition_window: 5,
            novelty: vec![
                NoveltyConfig {
                    spectrum: Spectrum::Percussive,
                    normalization: 10.0,
                    start_freq: 0.0,
                    end_freq: 300.0,
                },
                NoveltyConfig {
                    spectrum: Spectrum::Percussive,
                    normalization: 3.0,
                    start_freq: 300.0,
                    end_freq: 800.0,
                },
                NoveltyConfig {
                    spectrum: Spectrum::Percussive,
                    normalization: 2.0,
                    start_freq: 800.0,
                    end_freq: 2000.0,
                },
            ],
        }
    }
}

impl AnalysisConfig {
    /// A stable 64-bit hash of the configuration, used as cache key.
    pub fn cache_key(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        mix(crate::ANALYSIS_VERSION as u64);
        mix(self.fft_size as u64);
        mix(self.spectral_smoothing as u64);
        mix(self.spectral_separation.to_bits() as u64);
        mix(self.chroma_smoothing as u64);
        mix(self.chroma_down_sampling as u64);
        mix(self.major_transition_window as u64);
        mix(self.minor_transition_window as u64);
        for n in &self.novelty {
            mix(n.spectrum as u64);
            mix(n.normalization.to_bits() as u64);
            mix(n.start_freq.to_bits() as u64);
            mix(n.end_freq.to_bits() as u64);
        }
        h
    }
}

/// Transition classification values used in [`TrackAnalysis::transitions`].
pub const TRANSITION_MAJOR: f32 = 1.0;
pub const TRANSITION_MINOR: f32 = 0.5;
pub const TRANSITION_NONE: f32 = 0.0;

/// Result of analysing a complete track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAnalysis {
    pub sample_rate: u32,
    /// Samples between consecutive spectral frames.
    pub hop_size: usize,
    /// RMS energy per spectral frame.
    pub energy: Vec<f32>,
    /// One curve per configured novelty band, one value per spectral frame.
    pub novelty: Vec<Vec<f32>>,
    /// One value per chroma frame (TRANSITION_*).
    pub transitions: Vec<f32>,
    /// Spectral frames per chroma frame.
    pub transition_down_sampling: usize,
}

impl TrackAnalysis {
    pub fn frame_count(&self) -> usize {
        self.energy.len()
    }
    /// Spectral frame rate in frames per second.
    pub fn frame_rate(&self) -> f64 {
        self.sample_rate as f64 / self.hop_size as f64
    }
    /// Spectral frame index for a playback time in seconds (clamped).
    pub fn frame_at(&self, seconds: f64) -> usize {
        let n = self.frame_count();
        if n == 0 {
            return 0;
        }
        ((seconds.max(0.0) * self.frame_rate()) as usize).min(n - 1)
    }
    /// Novelty frame describing the sound at playback time `seconds`.
    ///
    /// A novelty value is the change from one spectral frame to the next
    /// and frames are labelled by the start of their window, so an onset
    /// shows up about 1.5 hops before it is heard (measured on real music:
    /// median 69 ms early at 4096/48 kHz). This compensates for that.
    pub fn novelty_frame_at(&self, seconds: f64) -> usize {
        self.frame_at(seconds - 1.5 * self.hop_size as f64 / self.sample_rate.max(1) as f64)
    }

    /// Per novelty curve, the factor that maps the 75th percentile of its
    /// peaks to 0.75: trigger thresholds then mean roughly "the strongest
    /// quarter of this track's hits" whatever the mix and loudness.
    pub fn novelty_scales(&self) -> Vec<f32> {
        self.novelty
            .iter()
            .map(|c| {
                let mut peaks: Vec<f32> = (1..c.len().saturating_sub(1))
                    .filter(|&i| c[i] > 0.0 && c[i] >= c[i - 1] && c[i] > c[i + 1])
                    .map(|i| c[i])
                    .collect();
                if peaks.len() < 8 {
                    return 1.0;
                }
                peaks.sort_by(|a, b| a.total_cmp(b));
                let p75 = peaks[peaks.len() * 3 / 4];
                if p75 > 0.0 {
                    (0.75 / p75).clamp(0.2, 8.0)
                } else {
                    1.0
                }
            })
            .collect()
    }
    /// Time in seconds of chroma frame `i` of the transition curve.
    pub fn transition_time(&self, i: usize) -> f64 {
        (i * self.transition_down_sampling * self.hop_size) as f64 / self.sample_rate as f64
    }
    /// Index of the transition frame for a playback time.
    pub fn transition_frame_at(&self, seconds: f64) -> usize {
        let n = self.transitions.len();
        if n == 0 {
            return 0;
        }
        ((seconds.max(0.0) * self.frame_rate() / self.transition_down_sampling as f64) as usize)
            .min(n - 1)
    }
}

/// Analyses mono samples. Progress (0..1) is reported through `progress`
/// (return `false` from it to cancel; `None` is returned then).
pub fn analyze(
    samples: &[f32],
    sample_rate: u32,
    config: &AnalysisConfig,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Option<TrackAnalysis> {
    let fft_size = config.fft_size.max(64).next_power_of_two();
    let hop = ((fft_size as f64) * 0.5).round() as usize;
    let spec = spectral_analysis(
        samples,
        fft_size,
        hop,
        config.spectral_smoothing,
        config.spectral_separation,
        progress,
    )?;
    let frames = spec.frames;
    let bins = spec.bins;
    let energy = energy_curve(samples, frames);
    if !progress(0.6) {
        return None;
    }
    let novelty = config
        .novelty
        .iter()
        .map(|n| {
            let s = match n.spectrum {
                Spectrum::Raw => &spec.raw,
                Spectrum::Harmonic => &spec.harmonic,
                Spectrum::Percussive => &spec.percussive,
            };
            novelty_curve(s, frames, bins, sample_rate, n)
        })
        .collect();
    if !progress(0.7) {
        return None;
    }
    let chroma_raw = chroma(
        &spec.raw,
        frames,
        bins,
        sample_rate,
        config.chroma_smoothing,
        config.chroma_down_sampling,
    );
    let chroma_harm = chroma(
        &spec.harmonic,
        frames,
        bins,
        sample_rate,
        config.chroma_smoothing,
        config.chroma_down_sampling,
    );
    let chroma_perc = chroma(
        &spec.percussive,
        frames,
        bins,
        sample_rate,
        config.chroma_smoothing,
        config.chroma_down_sampling,
    );
    if !progress(0.8) {
        return None;
    }
    let sims = [
        similarity(&chroma_raw),
        similarity(&chroma_harm),
        similarity(&chroma_perc),
    ];
    let n = chroma_raw.len() / 12;
    let major_kernel = checkerboard_kernel(config.major_transition_window, 0.5);
    let minor_kernel = checkerboard_kernel(config.minor_transition_window, 0.5);
    let major: Vec<Vec<f32>> = sims
        .iter()
        .map(|s| kernel_novelty(s, n, &major_kernel, config.major_transition_window))
        .collect();
    let minor: Vec<Vec<f32>> = sims
        .iter()
        .map(|s| kernel_novelty(s, n, &minor_kernel, config.minor_transition_window))
        .collect();
    if !progress(0.95) {
        return None;
    }
    let sens = [1.0, 0.5, 1.0];
    let major_peaks = resolve_peaks(
        &[
            (&major[0], sens[0]),
            (&major[1], sens[1]),
            (&major[2], sens[2]),
        ],
        5,
        15,
        1.5,
    );
    let minor_peaks = resolve_peaks(
        &[
            (&minor[0], sens[0]),
            (&minor[1], sens[1]),
            (&minor[2], sens[2]),
        ],
        5,
        15,
        0.5,
    );
    let transitions = merge_peaks(&major_peaks, &minor_peaks);
    progress(1.0);
    Some(TrackAnalysis {
        sample_rate,
        hop_size: hop,
        energy,
        novelty,
        transitions,
        transition_down_sampling: config.chroma_down_sampling.max(1),
    })
}

struct Spectrogram {
    frames: usize,
    bins: usize,
    raw: Vec<f32>,
    harmonic: Vec<f32>,
    percussive: Vec<f32>,
}

const DB_FLOOR: f32 = -80.0;
const DB_RANGE: f32 = 70.0;

/// Maps an amplitude to the normalised spectrum value used throughout.
fn normalize_amplitude(a: f32) -> f32 {
    let db = if a <= 0.0 { 0.0 } else { 20.0 * a.log10() };
    let lin = if db == 0.0 {
        0.0
    } else {
        ((db - DB_FLOOR).max(0.0) / DB_RANGE).min(1.0)
    };
    (lin - 0.5).max(0.0) / 0.5
}

fn spectral_analysis(
    samples: &[f32],
    fft_size: usize,
    hop: usize,
    smoothing: usize,
    separation: f32,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Option<Spectrogram> {
    let frames = samples.len().div_ceil(hop).max(1);
    let bins = fft_size / 2;
    let window = blackman_window(fft_size);
    let mut fft = RealFft::new(fft_size);
    let mut buf = vec![0.0f32; fft_size];
    let mut scratch = Vec::new();
    let mut raw = vec![0.0f32; frames * bins];
    for f in 0..frames {
        let start = f * hop;
        buf.fill(0.0);
        if start < samples.len() {
            let end = (start + fft_size).min(samples.len());
            buf[..end - start].copy_from_slice(&samples[start..end]);
        }
        for (v, w) in buf.iter_mut().zip(&window) {
            *v *= w;
        }
        let row = &mut raw[f * bins..(f + 1) * bins];
        fft.amplitudes(&buf, row, &mut scratch);
        for v in row.iter_mut() {
            *v = normalize_amplitude(*v);
        }
        if f % 512 == 0 && !progress(0.4 * f as f32 / frames as f32) {
            return None;
        }
    }

    // Harmonic/percussive separation with median filters. The harmonic
    // estimate is the median along time, the percussive estimate the median
    // along frequency (both zero padded at the borders).
    let r = smoothing;
    let len = 2 * r + 1;
    let mid = len / 2;
    let mut harmonic = vec![0.0f32; frames * bins];
    let mut percussive = vec![0.0f32; frames * bins];
    let mut wt = vec![0.0f32; len];
    let mut wf = vec![0.0f32; len];
    for f in 0..frames {
        let base = f * bins;
        for b in 0..bins {
            for (t, slot) in wt.iter_mut().enumerate() {
                let ff = f as isize + t as isize - r as isize;
                *slot = if ff >= 0 && (ff as usize) < frames {
                    raw[ff as usize * bins + b]
                } else {
                    0.0
                };
            }
            for (t, slot) in wf.iter_mut().enumerate() {
                let bb = b as isize + t as isize - r as isize;
                *slot = if bb >= 0 && (bb as usize) < bins {
                    raw[base + bb as usize]
                } else {
                    0.0
                };
            }
            let h = *wt.select_nth_unstable_by(mid, f32::total_cmp).1;
            let p = *wf.select_nth_unstable_by(mid, f32::total_cmp).1;
            let v = raw[base + b];
            // Binary mask; note the percussive test compares against the
            // already masked harmonic value (matching the reference).
            let hv = if h > p * separation { v } else { 0.0 };
            let pv = if p >= hv * separation { v } else { 0.0 };
            harmonic[base + b] = hv;
            percussive[base + b] = pv;
        }
        if f % 512 == 0 && !progress(0.4 + 0.2 * f as f32 / frames as f32) {
            return None;
        }
    }
    Some(Spectrogram {
        frames,
        bins,
        raw,
        harmonic,
        percussive,
    })
}

fn energy_curve(samples: &[f32], frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames];
    if frames == 0 {
        return out;
    }
    let block = samples.len() / frames;
    if block == 0 {
        return out;
    }
    for (f, e) in out.iter_mut().enumerate() {
        let s = &samples[f * block..(f + 1) * block];
        let sum: f64 = s.iter().map(|&x| (x as f64) * (x as f64)).sum();
        *e = (sum / block as f64).sqrt() as f32;
    }
    out
}

fn novelty_curve(
    spec: &[f32],
    frames: usize,
    bins: usize,
    sample_rate: u32,
    cfg: &NoveltyConfig,
) -> Vec<f32> {
    let bin_hz = sample_rate as f32 / 2.0 / bins as f32;
    let start = ((cfg.start_freq / bin_hz).floor().max(0.0) as usize).min(bins);
    let end = ((cfg.end_freq / bin_hz).ceil().max(0.0) as usize).min(bins);
    let mut y = vec![0.0f32; frames];
    if end > start {
        for (f, v) in y.iter_mut().enumerate() {
            let cur = &spec[f * bins + start..f * bins + end];
            let mut sum = 0.0f32;
            if f + 1 < frames {
                let next = &spec[(f + 1) * bins + start..(f + 1) * bins + end];
                for (a, b) in cur.iter().zip(next) {
                    sum += b - a;
                }
            } else {
                for a in cur {
                    sum -= a;
                }
            }
            *v = sum.max(0.0);
        }
    }
    // Local average subtraction. This reproduces the reference behaviour
    // exactly: the running value is divided by the window length after each
    // accumulated element (a decaying average dominated by the newest
    // values), index 0 is excluded, and earlier entries have already been
    // replaced by their normalised values when later frames are processed.
    let radius = 5usize;
    let d = (2 * radius + 1) as f32;
    let norm = if cfg.normalization == 0.0 {
        1.0
    } else {
        cfg.normalization
    };
    for m in 0..frames {
        let mut g = 0.0f32;
        for w in 0..2 * radius + 1 {
            let x = m as isize - radius as isize + w as isize;
            if x > 0 && (x as usize) < frames {
                g += y[x as usize];
            }
            g /= d;
        }
        y[m] = ((y[m] - g) / norm).max(0.0);
    }
    y
}

fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Computes smoothed and down-sampled 12-bin chroma vectors (row major,
/// 12 values per chroma frame).
fn chroma(
    spec: &[f32],
    frames: usize,
    bins: usize,
    sample_rate: u32,
    smoothing: usize,
    down: usize,
) -> Vec<f32> {
    let bin_hz = sample_rate as f32 / 2.0 / bins as f32;
    // Precompute bin ranges per MIDI note.
    let ranges: Vec<(usize, usize)> = (0..128)
        .map(|c| {
            let lo = midi_to_hz((c as f32 - 0.5).max(0.0));
            let hi = midi_to_hz((c as f32 + 0.5).min(127.0));
            (
                ((lo / bin_hz).floor() as usize).min(bins),
                ((hi / bin_hz).floor() as usize).min(bins),
            )
        })
        .collect();
    let mut c = vec![0.0f32; frames * 12];
    for f in 0..frames {
        let row = &spec[f * bins..(f + 1) * bins];
        for (note, &(lo, hi)) in ranges.iter().enumerate() {
            let mut s = 0.0;
            for v in &row[lo..hi.max(lo)] {
                s += v;
            }
            c[f * 12 + note % 12] += s;
        }
        let v = &mut c[f * 12..f * 12 + 12];
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x = if norm > 0.01 { *x / norm } else { 0.0 };
        }
    }
    // Median smoothing along time per pitch class.
    let len = 2 * smoothing + 1;
    let mut smoothed = vec![0.0f32; c.len()];
    let mut w = vec![0.0f32; len];
    for f in 0..frames {
        for k in 0..12 {
            for (j, slot) in w.iter_mut().enumerate() {
                let ff = f as isize + j as isize - smoothing as isize;
                *slot = if ff >= 0 && (ff as usize) < frames {
                    c[ff as usize * 12 + k]
                } else {
                    0.0
                };
            }
            smoothed[f * 12 + k] = *w.select_nth_unstable_by(len / 2, f32::total_cmp).1;
        }
    }
    let down = down.max(1);
    let out_frames = frames.div_ceil(down);
    let mut out = vec![0.0f32; out_frames * 12];
    for e in 0..out_frames {
        let src = e * down;
        if src < frames {
            out[e * 12..e * 12 + 12].copy_from_slice(&smoothed[src * 12..src * 12 + 12]);
        }
    }
    out
}

fn similarity(chroma: &[f32]) -> Vec<f32> {
    let n = chroma.len() / 12;
    let mut s = vec![0.0f32; n * n];
    for i in 0..n {
        let a = &chroma[i * 12..i * 12 + 12];
        for j in i..n {
            let b = &chroma[j * 12..j * 12 + 12];
            let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            s[i * n + j] = d;
            s[j * n + i] = d;
        }
    }
    s
}

/// Gaussian tapered checkerboard kernel of size (2r+1)², normalised so that
/// the absolute values sum to one.
fn checkerboard_kernel(r: usize, sigma: f32) -> Vec<f32> {
    let e = 2 * r + 1;
    let r_f = r.max(1) as f32;
    let a = 0.5f32.sqrt() / (r_f * sigma);
    let mut k = vec![0.0f32; e * e];
    let mut total = 0.0;
    for i in 0..e {
        for j in 0..e {
            let di = i as f32 - r as f32;
            let dj = j as f32 - r as f32;
            let g = (-(a * a) * (di * di + dj * dj)).exp();
            let v = if (i < r && j < r) || (i > r && j > r) {
                g
            } else if (i < r && j > r) || (i > r && j < r) {
                -g
            } else {
                0.0
            };
            k[i * e + j] = v;
            total += v.abs();
        }
    }
    if total > 0.0 {
        for v in &mut k {
            *v /= total;
        }
    }
    k
}

fn kernel_novelty(sim: &[f32], n: usize, kernel: &[f32], r: usize) -> Vec<f32> {
    let e = 2 * r + 1;
    let mut out = vec![0.0f32; n];
    for (f, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for c in 0..e {
            let v = f as isize + c as isize - r as isize;
            if v < 0 || v as usize >= n {
                continue;
            }
            let row = &sim[v as usize * n..(v as usize + 1) * n];
            let krow = &kernel[c * e..(c + 1) * e];
            for (h, &kv) in krow.iter().enumerate() {
                let m = f as isize + h as isize - r as isize;
                if m >= 0 && (m as usize) < n {
                    acc += kv * row[m as usize];
                }
            }
        }
        *o = acc;
    }
    out
}

fn is_peak(a: &[f32], i: usize) -> bool {
    let prev = a[i.saturating_sub(1)];
    let next = a[(i + 1).min(a.len() - 1)];
    prev < a[i] && a[i] > next
}

/// Picks peaks across several weighted curves. A candidate at frame `e` is
/// the strongest weighted local maximum among the curves; it is rejected if a
/// stronger peak follows within `window` frames, otherwise those following
/// weaker peaks are suppressed and the candidate is accepted if it exceeds
/// `threshold` times the local average of its curve.
fn resolve_peaks(
    curves: &[(&Vec<f32>, f32)],
    window: usize,
    avg_radius: usize,
    threshold: f32,
) -> Vec<f32> {
    let n = curves.first().map(|c| c.0.len()).unwrap_or(0);
    let mut out = vec![0.0f32; n];
    let mut suppressed = vec![false; n];
    for e in 0..n {
        if suppressed[e] {
            out[e] = TRANSITION_NONE;
            continue;
        }
        let mut best = -1.0f32;
        let mut best_idx = usize::MAX;
        for (ci, (curve, sens)) in curves.iter().enumerate() {
            if is_peak(curve, e) {
                let v = curve[e] * sens;
                if v > best {
                    best = v;
                    best_idx = ci;
                }
            }
        }
        if best_idx == usize::MAX {
            continue;
        }
        let mut accepted = true;
        let mut later = Vec::new();
        for m in 1..=window {
            let idx = (e + m).min(n - 1);
            let mut o = -1.0f32;
            for (curve, sens) in curves {
                let v = if is_peak(curve, idx) {
                    curve[idx] * sens
                } else {
                    -1.0
                };
                if v > o {
                    o = v;
                }
            }
            if o > best {
                accepted = false;
                out[e] = TRANSITION_NONE;
                break;
            }
            if o != -1.0 {
                later.push(idx);
            }
        }
        if accepted {
            for idx in later {
                suppressed[idx] = true;
            }
            let (curve, sens) = curves[best_idx];
            let count = 2 * avg_radius;
            let mut avg = 0.0;
            for b in 0..count {
                let idx = (e as isize - avg_radius as isize + b as isize).clamp(0, n as isize - 1)
                    as usize;
                avg += curve[idx] * sens;
            }
            avg /= count as f32;
            out[e] = if curve[e] * sens > avg * threshold {
                TRANSITION_MAJOR
            } else {
                TRANSITION_NONE
            };
        }
    }
    out
}

fn merge_peaks(major: &[f32], minor: &[f32]) -> Vec<f32> {
    let n = major.len();
    let mut out = vec![0.0f32; n];
    for a in 0..n {
        if major[a] != TRANSITION_NONE {
            out[a] = TRANSITION_MAJOR;
        } else if minor[a] != TRANSITION_NONE {
            let lo = a.saturating_sub(10);
            let hi = (a + 10).min(n);
            let near_major = (lo..hi).any(|o| major[o] != TRANSITION_NONE);
            out[a] = if near_major {
                TRANSITION_NONE
            } else {
                TRANSITION_MINOR
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic "song": a section with one chord and kicks, then a section
    /// with a different chord and hi-hats.
    fn synth(sr: u32, secs: f32) -> Vec<f32> {
        let n = (sr as f32 * secs) as usize;
        let mut out = vec![0.0f32; n];
        let mut rng = 12345u32;
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f32 / sr as f32;
            let first = t < secs / 2.0;
            let freqs: [f32; 3] = if first {
                [261.6, 329.6, 392.0]
            } else {
                [293.7, 370.0, 440.0]
            };
            let mut v = 0.0;
            for f in freqs {
                v += (std::f32::consts::TAU * f * t).sin() * 0.15;
            }
            let beat = t % 0.5;
            if first {
                v += (std::f32::consts::TAU * 60.0 * t).sin() * (-beat * 30.0).exp() * 0.6;
            } else {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (rng >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
                v += noise * (-beat * 80.0).exp() * 0.3;
            }
            *o = v;
        }
        out
    }

    #[test]
    fn analysis_detects_beats_and_section_change() {
        let sr = 22050;
        let samples = synth(sr, 40.0);
        let cfg = AnalysisConfig::default();
        let a = analyze(&samples, sr, &cfg, &mut |_| true).unwrap();
        assert_eq!(a.novelty.len(), 3);
        assert_eq!(a.energy.len(), a.frame_count());
        // Low band novelty should pulse at the kick rate in the first half.
        let low = &a.novelty[0];
        let half = a.frame_count() / 2;
        let active = low[..half].iter().filter(|&&v| v > 0.0).count();
        assert!(
            active > 10,
            "low novelty should fire on kicks, got {active}"
        );
        // A major transition should be detected near the middle.
        let mid_t = 20.0;
        let found = a
            .transitions
            .iter()
            .enumerate()
            .any(|(i, &v)| v == TRANSITION_MAJOR && (a.transition_time(i) - mid_t).abs() < 3.0);
        assert!(
            found,
            "expected a major transition around {mid_t}s: {:?}",
            a.transitions
                .iter()
                .enumerate()
                .filter(|(_, &v)| v > 0.0)
                .map(|(i, &v)| (a.transition_time(i), v))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn kernel_is_normalised() {
        let k = checkerboard_kernel(5, 0.5);
        let s: f32 = k.iter().map(|v| v.abs()).sum();
        assert!((s - 1.0).abs() < 1e-4);
    }

    #[test]
    fn tiny_inputs_do_not_panic() {
        let cfg = AnalysisConfig::default();
        for len in [0usize, 1, 100, 2047, 2049] {
            let s = vec![0.1f32; len];
            let a = analyze(&s, 44100, &cfg, &mut |_| true).unwrap();
            assert!(a.frame_count() >= 1);
        }
    }
}
