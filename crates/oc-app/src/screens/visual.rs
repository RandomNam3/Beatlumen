//! Live spectrum / waveform visualizer of the "now playing" overlay.

use crate::app::App;
use crate::settings::VisualizerKind;
use oc_analysis::fft::{blackman_window, RealFft};
use oc_ui::{Color, Rect, Ui};

const FFT: usize = 8192;

fn oddify(x: usize) -> usize {
    2 * (x / 2) + 1
}

pub fn draw(app: &mut App, ui: &mut Ui, r: Rect, alpha: f32) {
    match app.settings.visualizer {
        VisualizerKind::None => {}
        VisualizerKind::Spectrum => spectrum(app, ui, r, alpha),
        VisualizerKind::Waveform => waveform(app, ui, r, alpha),
    }
}

fn spectrum(app: &mut App, ui: &mut Ui, r: Rect, alpha: f32) {
    let s = &app.settings;
    let bw = s.bar_width.max(1.0);
    let gap = s.bar_gap.max(0.0);
    let n = 1 + ((r.w - bw).max(0.0) / (bw + gap)) as usize;
    let mut samples = vec![0.0f32; FFT];
    app.player.recent_samples(&mut samples);
    let window = blackman_window(FFT);
    for (v, w) in samples.iter_mut().zip(&window) {
        *v *= w;
    }
    let mut fft = RealFft::new(FFT);
    let mut amps = vec![0.0f32; FFT / 2];
    let mut scratch = Vec::new();
    fft.amplitudes(&samples, &mut amps, &mut scratch);
    // Like an AnalyserNode: magnitude in dB mapped from [-100, -10] to [0, 1].
    let rate = app.player.device_rate().max(1) as f32;
    let bin_hz = rate / 2.0 / amps.len() as f32;
    let (f0, f1) = (s.spectrum_min_freq.max(0.0), s.spectrum_max_freq.max(s.spectrum_min_freq + 10.0));
    let mut bars = vec![0.0f32; n];
    for (i, b) in bars.iter_mut().enumerate() {
        let lo = f0 + (f1 - f0) * i as f32 / n as f32;
        let hi = f0 + (f1 - f0) * (i + 1) as f32 / n as f32;
        let b0 = ((lo / bin_hz) as usize).min(amps.len() - 1);
        let b1 = ((hi / bin_hz).ceil() as usize).clamp(b0 + 1, amps.len());
        let mut sum = 0.0;
        for a in &amps[b0..b1] {
            let db = 20.0 * (a / 2.0).max(1e-9).log10();
            sum += ((db + 100.0) / 90.0).clamp(0.0, 1.0);
        }
        let v = sum / (b1 - b0) as f32;
        *b = if v > 0.4 { v.powf(v + 0.6) } else { v * (v + 0.6) * 0.25 }.min(1.0);
    }
    // Local mean gate.
    let win = oddify(n / 8).min(n).max(1);
    let half = win / 2;
    let gated: Vec<f32> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            let mean = bars[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            if bars[i] < mean {
                0.0
            } else {
                bars[i]
            }
        })
        .collect();
    // Gaussian blur across bars.
    let sigma = 1.5f32;
    let k: Vec<f32> = (0..win).map(|j| (-((j as f32 - half as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let ksum: f32 = k.iter().sum();
    let blurred: Vec<f32> = (0..n)
        .map(|i| {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let idx = i as isize + j as isize - half as isize;
                if idx >= 0 && (idx as usize) < n {
                    acc += gated[idx as usize] * kv;
                }
            }
            acc / ksum
        })
        .collect();
    let smooth = s.spectrum_smoothing.clamp(0.0, 0.99);
    if app.spectrum.len() != n {
        app.spectrum = vec![0.0; n];
    }
    let frames = (app.last_dt * 60.0) as f32;
    let sm = smooth.powf(frames.max(0.1));
    for (cur, v) in app.spectrum.iter_mut().zip(&blurred) {
        *cur = *cur * sm + v * (1.0 - sm);
    }
    let paused = app.player.state() != crate::player::PlayState::Playing;
    for (i, v) in app.spectrum.iter().enumerate() {
        let h = (0.99 * v + 0.01) * r.h;
        let x = r.x + i as f32 * (bw + gap);
        let a = if paused { 0.6 } else { 0.92 };
        ui.rect(Rect::new(x, r.bottom() - h, bw, h), Color(1.0, 1.0, 1.0, a * alpha), bw.min(2.0) / 2.0);
    }
}

fn waveform(app: &mut App, ui: &mut Ui, r: Rect, alpha: f32) {
    let points = (r.w as usize / 2).clamp(32, 1024);
    let mut samples = vec![0.0f32; 2048];
    app.player.recent_samples(&mut samples);
    let thick = app.settings.waveform_thickness.clamp(0.0, 0.99);
    if app.waveform.len() != points {
        app.waveform = vec![0.0; points];
    }
    for i in 0..points {
        let s = samples[i * samples.len() / points];
        let v = 0.5 + 0.5 * s.clamp(-1.0, 1.0);
        if i % 2 == 0 {
            app.waveform[i] = app.waveform[i] * thick + v * (1.0 - thick);
        } else {
            app.waveform[i] = v;
        }
    }
    let step = r.w / points as f32;
    for i in 1..points {
        let y0 = r.bottom() - app.waveform[i - 1] * r.h;
        let y1 = r.bottom() - app.waveform[i] * r.h;
        let (top, bot) = (y0.min(y1), y0.max(y1));
        // Fade at the edges.
        let edge = ((i as f32 / points as f32).min(1.0 - i as f32 / points as f32) * 12.0).min(1.0);
        ui.rect(Rect::new(r.x + i as f32 * step, top - 1.0, step.max(1.5), (bot - top).max(2.0)), Color(1.0, 1.0, 1.0, 0.9 * alpha * edge), 0.0);
    }
}
