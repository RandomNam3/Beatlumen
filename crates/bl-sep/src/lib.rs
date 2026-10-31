//! Separating the singing from the music of a song, so that speech
//! recognition and word timing can listen to the vocals alone. Uses the
//! vocals model of Open-Unmix UMX-HQ (Stöter et al., MIT license),
//! implemented here without external libraries: the short-time Fourier
//! transform, the network (see [`umx`]) and optionally one step of
//! multichannel Wiener filtering.
//!
//! The model works on stereo audio at 44.1 kHz (mono is duplicated, other
//! rates are converted). The vocals come back at 44.1 kHz; [`Vocals::mono`]
//! converts them for speech recognition (16 kHz mono).
//!
//! ```no_run
//! let sep = bl_sep::Separator::load("models/umxhq-vocals/vocals-b62c91ce.pth").unwrap();
//! # let (left, right) = (vec![0f32; 44100], vec![0f32; 44100]);
//! let opts = bl_sep::Options {
//!     wiener: false,
//!     progress: &mut |_| true,
//! };
//! let vocals = sep.separate(&[&left, &right], 44100, opts).unwrap();
//! let for_whisper = vocals.mono(16000);
//! ```

#![forbid(unsafe_code)]

pub mod fft;
pub mod stft;
pub mod umx;
pub mod wiener;

use bl_asr::models::FileSpec;
use fft::Complex;
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use stft::{OverlapAdd, Stft};
use umx::{Umx, BINS, CHANNELS, HIDDEN, MAX_BIN};

/// The model's id in the models folder.
pub const MODEL_ID: &str = "umxhq-vocals";
/// The weights (Open-Unmix UMX-HQ, vocals; Zenodo record 3370489, MIT
/// license): file name, address, size and SHA-256.
pub const MODEL_FILE: &str = "vocals-b62c91ce.pth";
pub const MODEL_URL: &str = "https://zenodo.org/records/3370489/files/vocals-b62c91ce.pth";
pub const MODEL_SIZE: u64 = 35_637_796;
pub const MODEL_SHA256: &str = "b62c91cedbc7a066f1778ead5b5cecb377aa3a46a31af1cce7c5c8769339d083";

/// The model's sample rate.
pub const SAMPLE_RATE: u32 = 44100;
const N_FFT: usize = 4096;
const HOP: usize = 1024;
/// Frames encoded together.
const ENCODE_BLOCK: usize = 256;
/// Frames decoded together; also the window of the Wiener filter (as in
/// Open-Unmix).
const DECODE_BLOCK: usize = 300;

/// The files to download for the model (see `bl_asr::models::Models::
/// download_files`).
pub fn model_files() -> Vec<FileSpec> {
    vec![FileSpec {
        name: MODEL_FILE.into(),
        url: MODEL_URL.into(),
        size: MODEL_SIZE,
        sha256: MODEL_SHA256.into(),
    }]
}

pub struct Options<'a> {
    /// One step of multichannel Wiener filtering after the network, as
    /// Open-Unmix offers (with the rest of the mixture as second source).
    /// It did not help on the MUSDB18 test excerpts (median vocals SDR
    /// 6.44 dB with it, 6.47 dB without), so it is off by default.
    pub wiener: bool,
    /// Called with the progress (0..1); false cancels.
    pub progress: &'a mut dyn FnMut(f32) -> bool,
}

/// Separated vocals, stereo at 44.1 kHz.
pub struct Vocals {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Vocals {
    /// Both channels averaged, at `rate` Hz (16000 for speech recognition).
    pub fn mono(&self, rate: u32) -> Vec<f32> {
        let mono: Vec<f32> = self
            .left
            .iter()
            .zip(&self.right)
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        resample(&mono, SAMPLE_RATE, rate)
    }
}

/// Converts mono `samples` from `from` Hz to `to` Hz (the converter's
/// delay removed, so that times stay the same).
pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let mut rs = bl_media::Resampler::new(from, to, 1);
    let delay = rs.latency();
    let len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    let mut out = Vec::with_capacity(len + delay);
    rs.process(samples, &mut out);
    // Flush with silence until the delayed end is out.
    let silence = vec![0f32; 4096];
    while out.len() < len + delay {
        rs.process(&silence, &mut out);
    }
    out.drain(..delay);
    out.truncate(len);
    out
}

/// Runs `f` on parts of `0..n` on all cores and joins the results in
/// order.
pub(crate) fn parallel<T: Send>(n: usize, f: impl Fn(Range<usize>) -> Vec<T> + Sync) -> Vec<T> {
    let threads = bl_asr::ops::threads().min(n).max(1);
    if threads == 1 {
        return f(0..n);
    }
    let chunk = n.div_ceil(threads);
    std::thread::scope(|s| {
        let f = &f;
        let parts: Vec<_> = (0..threads)
            .map(|t| {
                let r = (t * chunk).min(n)..((t + 1) * chunk).min(n);
                s.spawn(move || f(r))
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|h| h.join().expect("worker thread"))
            .collect()
    })
}

/// The vocals model, loaded.
pub struct Separator {
    model: Umx,
    stft: Stft,
}

impl Separator {
    /// Loads the weights (the PyTorch checkpoint `MODEL_FILE`).
    pub fn load(path: impl AsRef<Path>) -> Result<Separator, String> {
        Ok(Separator {
            model: Umx::load(path)?,
            stft: Stft::new(N_FFT, HOP),
        })
    }

    /// The vocals of a song given as one (mono) or two (stereo) channels
    /// at `rate` Hz.
    pub fn separate<'a>(
        &self,
        channels: &[&'a [f32]],
        rate: u32,
        opts: Options,
    ) -> Result<Vocals, String> {
        // Converted to 44.1 kHz (borrowed if they are at that rate).
        let at_rate = |c: &'a [f32]| -> Cow<'a, [f32]> {
            match rate {
                SAMPLE_RATE => Cow::Borrowed(c),
                _ => Cow::Owned(resample(c, rate, SAMPLE_RATE)),
            }
        };
        let [left, right] = match *channels {
            [] => return Err("no audio".into()),
            [mono] => {
                let m = at_rate(mono);
                self.separate_stereo(&m, &m, opts)
            }
            [l, r] if l.len() == r.len() => self.separate_stereo(&at_rate(l), &at_rate(r), opts),
            [_, _] => return Err("channels of different lengths".into()),
            _ => return Err("more than two channels".into()),
        }
        .ok_or("cancelled")?;
        Ok(Vocals { left, right })
    }

    /// The model's estimate for the frames `range`: the mixture's spectra
    /// and the vocals' (`CHANNELS * BINS` per frame, channel after
    /// channel).
    fn spectra(&self, channels: [&[f32]; 2], range: Range<usize>) -> Vec<Complex> {
        let n = range.len();
        let spec = channels.map(|c| self.stft.forward(c, range.clone()));
        let mut mix = Vec::with_capacity(n * CHANNELS * BINS);
        for t in 0..n {
            for s in &spec {
                mix.extend_from_slice(&s[t * BINS..(t + 1) * BINS]);
            }
        }
        mix
    }

    /// The vocals of stereo audio at 44.1 kHz; `None` if cancelled.
    pub fn separate_stereo(
        &self,
        left: &[f32],
        right: &[f32],
        opts: Options,
    ) -> Option<[Vec<f32>; 2]> {
        let len = left.len().min(right.len());
        let channels = [&left[..len], &right[..len]];
        let frames = self.stft.frames(len);
        let progress = opts.progress;
        // Shares of the time the steps take (about).
        let (encoding, recurrent) = (0.25, 0.4);

        // Encode the magnitudes, a block of frames at a time.
        let mut encoded = Vec::with_capacity(frames * HIDDEN);
        for t0 in (0..frames).step_by(ENCODE_BLOCK) {
            let t1 = (t0 + ENCODE_BLOCK).min(frames);
            let mix = self.spectra(channels, t0..t1);
            let mag: Vec<f32> = mix
                .chunks_exact(BINS)
                .flat_map(|c| c[..MAX_BIN].iter().map(|v| v.norm()))
                .collect();
            encoded.extend(self.model.encode(&mag, t1 - t0));
            if !progress(encoding * t1 as f32 / frames as f32) {
                return None;
            }
        }

        let lstm = self.model.lstm(&encoded, frames, &mut |layer| {
            progress(encoding + recurrent * (layer + 1) as f32 / umx::LAYERS as f32)
        })?;

        // Decode the masks, apply them and transform back.
        let mut out = [
            OverlapAdd::new(&self.stft, len),
            OverlapAdd::new(&self.stft, len),
        ];
        for t0 in (0..frames).step_by(DECODE_BLOCK) {
            let t1 = (t0 + DECODE_BLOCK).min(frames);
            let n = t1 - t0;
            let mix = self.spectra(channels, t0..t1);
            let mask = self.model.decode(
                &encoded[t0 * HIDDEN..t1 * HIDDEN],
                &lstm[t0 * HIDDEN..t1 * HIDDEN],
                n,
            );
            let mut vocals: Vec<Complex> =
                mix.iter().zip(&mask).map(|(c, m)| c.scale(*m)).collect();
            if opts.wiener {
                wiener::filter(&mix, &mut vocals, n, BINS);
            }

            for (ch, o) in out.iter_mut().enumerate() {
                let spec: Vec<Complex> = vocals
                    .chunks_exact(BINS)
                    .skip(ch)
                    .step_by(CHANNELS)
                    .flatten()
                    .copied()
                    .collect();
                o.add(&self.stft, t0, &self.stft.inverse(&spec));
            }
            let done =
                encoding + recurrent + (1.0 - encoding - recurrent) * t1 as f32 / frames as f32;
            if !progress(done) {
                return None;
            }
        }
        let [l, r] = out;
        Some([l.finish(&self.stft, len), r.finish(&self.stft, len)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_timing() {
        // A 1 kHz tone from 44.1 kHz to 16 kHz: same phase at the same time.
        let tone = |rate: u32, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin() as f32
                })
                .collect()
        };
        let x = tone(44100, 44100);
        let y = resample(&x, 44100, 16000);
        assert_eq!(y.len(), 16000);
        let want = tone(16000, 16000);
        // Away from the ends (the filter sees silence there).
        for i in 1000..15000 {
            assert!((y[i] - want[i]).abs() < 0.01, "{i}: {} {}", y[i], want[i]);
        }
        assert_eq!(resample(&x[..10], 44100, 44100), &x[..10]);
    }

    #[test]
    fn model_file_spec() {
        let f = &model_files()[0];
        assert!(f.url.ends_with(&f.name));
        assert_eq!(f.sha256.len(), 64);
    }
}
