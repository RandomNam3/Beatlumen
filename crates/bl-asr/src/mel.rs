//! Whisper's log-mel spectrogram: 16 kHz audio, 25 ms Hann windows every
//! 10 ms (400-point DFT, hop 160), mel filters from the model, log10,
//! clamped to 8 decades below the maximum and scaled to about [-1, 1].

pub const SAMPLE_RATE: u32 = 16000;
pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
/// Frames in one 30-second window.
pub const WINDOW_FRAMES: usize = 3000;

/// Mel spectrogram, `n_mels` rows of `frames` values (row major).
pub struct Mel {
    pub n_mels: usize,
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Mel {
    /// `n_mels` x `WINDOW_FRAMES` starting at frame `start` (zero beyond).
    pub fn window(&self, start: usize) -> Vec<f32> {
        let mut out = vec![0f32; self.n_mels * WINDOW_FRAMES];
        for m in 0..self.n_mels {
            let row = &self.data[m * self.frames..(m + 1) * self.frames];
            let end = (start + WINDOW_FRAMES).min(self.frames);
            if start < end {
                out[m * WINDOW_FRAMES..m * WINDOW_FRAMES + end - start]
                    .copy_from_slice(&row[start..end]);
            }
        }
        out
    }
}

/// Computes the spectrogram of mono 16 kHz `samples` (padded with 30 s of
/// silence, as Whisper was trained).
pub fn log_mel(samples: &[f32], filters: &[f32], n_mels: usize, bins: usize) -> Mel {
    let pad = SAMPLE_RATE as usize * 30;
    let n = samples.len() + pad;
    let frames = n / HOP;
    // Centred frames: reflect the start.
    let half = N_FFT / 2;
    let sample = |i: isize| -> f32 {
        let i = if i < 0 { -i } else { i } as usize;
        samples.get(i).copied().unwrap_or(0.0)
    };
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / N_FFT as f32).cos()))
        .collect();
    // DFT tables (bins x N_FFT).
    let mut cos_t = vec![0f32; bins * N_FFT];
    let mut sin_t = vec![0f32; bins * N_FFT];
    for k in 0..bins {
        for t in 0..N_FFT {
            let a = 2.0 * std::f64::consts::PI * ((k * t) % N_FFT) as f64 / N_FFT as f64;
            cos_t[k * N_FFT + t] = a.cos() as f32;
            sin_t[k * N_FFT + t] = a.sin() as f32;
        }
    }
    let mut data = vec![0f32; n_mels * frames];
    let threads = crate::ops::threads().min(frames.max(1));
    let chunk = frames.div_ceil(threads);
    let parts: Vec<(usize, Vec<f32>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|ti| {
                let (window, cos_t, sin_t, sample) = (&window, &cos_t, &sin_t, &sample);
                s.spawn(move || {
                    let f0 = ti * chunk;
                    let f1 = ((ti + 1) * chunk).min(frames);
                    let mut out = vec![0f32; n_mels * (f1.saturating_sub(f0))];
                    let mut frame = vec![0f32; N_FFT];
                    let mut power = vec![0f32; bins];
                    for f in f0..f1 {
                        let start = (f * HOP) as isize - half as isize;
                        for (t, x) in frame.iter_mut().enumerate() {
                            *x = sample(start + t as isize) * window[t];
                        }
                        for k in 0..bins {
                            let c = &cos_t[k * N_FFT..(k + 1) * N_FFT];
                            let s = &sin_t[k * N_FFT..(k + 1) * N_FFT];
                            let (mut re, mut im) = (0f32, 0f32);
                            for t in 0..N_FFT {
                                re += frame[t] * c[t];
                                im += frame[t] * s[t];
                            }
                            power[k] = re * re + im * im;
                        }
                        for m in 0..n_mels {
                            let fl = &filters[m * bins..(m + 1) * bins];
                            let v: f32 = fl.iter().zip(&power).map(|(a, b)| a * b).sum();
                            out[m * (f1 - f0) + f - f0] = v.max(1e-10).log10();
                        }
                    }
                    (f0, out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("mel thread"))
            .collect()
    });
    for (f0, out) in parts {
        let w = out.len() / n_mels.max(1);
        for m in 0..n_mels {
            data[m * frames + f0..m * frames + f0 + w].copy_from_slice(&out[m * w..(m + 1) * w]);
        }
    }
    let max = data.iter().copied().fold(f32::MIN, f32::max);
    for v in &mut data {
        *v = ((*v).max(max - 8.0) + 4.0) / 4.0;
    }
    Mel {
        n_mels,
        frames,
        data,
    }
}
