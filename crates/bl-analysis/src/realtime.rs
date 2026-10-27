//! Streaming analysis of the audio that is currently being played.
//!
//! Used for live spectrum displays and as a fallback trigger source while
//! the offline [`crate::track`] analysis is still being computed (or is not
//! available, e.g. for endless streams).

use crate::fft::{blackman_window, RealFft};
use crate::track::{AnalysisConfig, NoveltyConfig};

pub struct RealtimeAnalyzer {
    sample_rate: u32,
    fft_size: usize,
    hop: usize,
    fft: RealFft,
    window: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    since_hop: usize,
    frame: Vec<f32>,
    scratch: Vec<(f32, f32)>,
    spectrum: Vec<f32>,
    prev_spectrum: Vec<f32>,
    bands: Vec<NoveltyConfig>,
    history: Vec<Vec<f32>>,
    novelty: Vec<f32>,
    energy: f32,
}

const HISTORY: usize = 11;

impl RealtimeAnalyzer {
    pub fn new(sample_rate: u32, config: &AnalysisConfig) -> RealtimeAnalyzer {
        let fft_size = config.fft_size.max(64).next_power_of_two();
        let bins = fft_size / 2;
        RealtimeAnalyzer {
            sample_rate,
            fft_size,
            hop: fft_size / 2,
            fft: RealFft::new(fft_size),
            window: blackman_window(fft_size),
            ring: vec![0.0; fft_size],
            ring_pos: 0,
            since_hop: 0,
            frame: vec![0.0; fft_size],
            scratch: Vec::new(),
            spectrum: vec![0.0; bins],
            prev_spectrum: vec![0.0; bins],
            bands: config.novelty.clone(),
            history: vec![vec![0.0; HISTORY]; config.novelty.len()],
            novelty: vec![0.0; config.novelty.len()],
            energy: 0.0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Clears all state (e.g. after seeking or changing tracks).
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.spectrum.fill(0.0);
        self.prev_spectrum.fill(0.0);
        for h in &mut self.history {
            h.fill(0.0);
        }
        self.novelty.fill(0.0);
        self.energy = 0.0;
        self.since_hop = 0;
    }

    /// Feeds mono samples (in playback order).
    pub fn push(&mut self, mono: &[f32]) {
        for &s in mono {
            self.ring[self.ring_pos] = s;
            self.ring_pos = (self.ring_pos + 1) % self.fft_size;
            self.since_hop += 1;
            if self.since_hop >= self.hop {
                self.since_hop = 0;
                self.process_frame();
            }
        }
    }

    fn process_frame(&mut self) {
        let n = self.fft_size;
        let mut sum = 0.0f32;
        for i in 0..n {
            let v = self.ring[(self.ring_pos + i) % n];
            sum += v * v;
            self.frame[i] = v * self.window[i];
        }
        self.energy = (sum / n as f32).sqrt();
        std::mem::swap(&mut self.spectrum, &mut self.prev_spectrum);
        self.fft
            .amplitudes(&self.frame, &mut self.spectrum, &mut self.scratch);
        for v in &mut self.spectrum {
            let db = if *v <= 0.0 { 0.0 } else { 20.0 * v.log10() };
            let lin = if db == 0.0 {
                0.0
            } else {
                ((db + 80.0).max(0.0) / 70.0).min(1.0)
            };
            *v = (lin - 0.5).max(0.0) / 0.5;
        }
        let bins = n / 2;
        let bin_hz = self.sample_rate as f32 / 2.0 / bins as f32;
        for (bi, band) in self.bands.iter().enumerate() {
            let start = ((band.start_freq / bin_hz).floor() as usize).min(bins);
            let end = ((band.end_freq / bin_hz).ceil() as usize).min(bins);
            let mut flux = 0.0;
            for k in start..end {
                flux += self.spectrum[k] - self.prev_spectrum[k];
            }
            let flux = flux.max(0.0);
            let h = &mut self.history[bi];
            let avg = h.iter().sum::<f32>() / HISTORY as f32;
            h.rotate_left(1);
            h[HISTORY - 1] = flux;
            let norm = if band.normalization == 0.0 {
                1.0
            } else {
                band.normalization
            };
            self.novelty[bi] = ((flux - avg) / norm).max(0.0);
        }
    }

    /// Latest novelty value per configured band.
    pub fn novelty(&self) -> &[f32] {
        &self.novelty
    }

    /// Latest RMS energy.
    pub fn energy(&self) -> f32 {
        self.energy
    }

    /// Normalised spectrum (0..1 per bin, `fft_size / 2` bins).
    pub fn spectrum(&self) -> &[f32] {
        &self.spectrum
    }

    /// Spectrum aggregated into `out.len()` logarithmically spaced bands
    /// between 30 Hz and 16 kHz (max per band).
    pub fn log_bands(&self, out: &mut [f32]) {
        let bins = self.spectrum.len();
        let bin_hz = self.sample_rate as f32 / 2.0 / bins as f32;
        let n = out.len();
        let (lo, hi) = (30f32.ln(), 16000f32.min(self.sample_rate as f32 / 2.0).ln());
        for (i, o) in out.iter_mut().enumerate() {
            let f0 = (lo + (hi - lo) * i as f32 / n as f32).exp();
            let f1 = (lo + (hi - lo) * (i + 1) as f32 / n as f32).exp();
            let b0 = ((f0 / bin_hz) as usize).min(bins - 1);
            let b1 = ((f1 / bin_hz).ceil() as usize).clamp(b0 + 1, bins);
            *o = self.spectrum[b0..b1].iter().cloned().fold(0.0, f32::max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_triggers_low_band() {
        let sr = 44100;
        let mut a = RealtimeAnalyzer::new(sr, &AnalysisConfig::default());
        let mut max_low = 0.0f32;
        for i in 0..sr as usize * 2 {
            let t = i as f32 / sr as f32;
            let beat = t % 0.5;
            let s = (std::f32::consts::TAU * 60.0 * t).sin() * (-beat * 30.0).exp() * 0.8;
            a.push(&[s]);
            max_low = max_low.max(a.novelty()[0]);
        }
        assert!(max_low > 0.0);
        let mut bands = [0.0; 16];
        a.log_bands(&mut bands);
    }
}
