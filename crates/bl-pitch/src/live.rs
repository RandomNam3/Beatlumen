//! The pitch of a microphone while someone sings (YIN, frame by frame).
//!
//! Samples go in as they arrive; every hop a pitch comes out for a short
//! window. A guard against octave errors keeps the pitch on the octave it
//! had a moment ago when the other octave's period fits nearly as well,
//! unless the jump stays.

use crate::dsp::{self, Decimator, DECIMATOR_DELAY};

#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// Lowest and highest pitch (Hz).
    pub fmin: f32,
    pub fmax: f32,
    /// Compared window (seconds); a frame reaches one period of `fmin`
    /// further.
    pub window: f64,
    /// Seconds between results.
    pub hop: f64,
    /// YIN threshold on d'(τ): the first trough under it is the period.
    pub threshold: f32,
    /// Frames whose best trough is higher than this are unvoiced.
    pub unvoiced: f32,
    /// Frames quieter than this (RMS) are unvoiced.
    pub silence: f32,
    /// How much worse (in d') the period on the previous octave may fit
    /// and still be taken.
    pub octave_tolerance: f32,
    /// After this many frames in a row on the other octave the jump is
    /// believed.
    pub octave_hold: u32,
}

impl Default for LiveOptions {
    fn default() -> Self {
        LiveOptions {
            fmin: 70.0,
            fmax: 1100.0,
            window: 0.025,
            hop: 0.01,
            threshold: 0.15,
            unvoiced: 0.35,
            silence: 0.003,
            octave_tolerance: 0.12,
            octave_hold: 6,
        }
    }
}

/// One result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LivePitch {
    /// Centre of the frame, seconds since the first sample pushed.
    pub time: f64,
    /// Pitch (Hz), `None` when nobody sings.
    pub f0: Option<f32>,
    /// How periodic the frame is (1 − d'(τ), 0…1).
    pub confidence: f32,
    /// Level of the frame (RMS).
    pub rms: f32,
}

pub struct LiveYin {
    opts: LiveOptions,
    dec: Decimator,
    sr: f64,
    w: usize,
    hop: usize,
    tau_min: usize,
    tau_max: usize,
    /// Downsampled samples; `buf[0]` is sample number `buf_start`.
    buf: Vec<f32>,
    buf_start: usize,
    /// Start (in downsampled samples) of the next frame's window.
    next: usize,
    d: Vec<f32>,
    scratch: Vec<f32>,
    /// Last voiced pitch, the frame it was in, and frames in a row the
    /// guard moved the octave.
    last: Option<(f32, usize)>,
    frame: usize,
    flipped: u32,
}

impl LiveYin {
    pub fn new(sample_rate: u32, opts: LiveOptions) -> LiveYin {
        let dec = Decimator::new(dsp::decimation_factor(sample_rate));
        let sr = sample_rate as f64 / dec.factor() as f64;
        let fmax = opts.fmax.min(sr as f32 * 0.25);
        let tau_min = ((sr as f32 / fmax).floor() as usize).max(2);
        let tau_max = ((sr as f32 / opts.fmin).ceil() as usize).max(tau_min + 2);
        let w = ((opts.window * sr).round() as usize).max(16);
        let hop = ((opts.hop * sr).round() as usize).max(1);
        LiveYin {
            dec,
            sr,
            w,
            hop,
            tau_min,
            tau_max,
            buf: Vec::new(),
            buf_start: 0,
            next: 0,
            d: vec![0.0; tau_max + 2],
            scratch: Vec::new(),
            last: None,
            frame: 0,
            flipped: 0,
            opts,
        }
    }

    /// Forgets everything, as if new.
    pub fn reset(&mut self) {
        self.dec.reset();
        self.buf.clear();
        self.buf_start = 0;
        self.next = 0;
        self.last = None;
        self.frame = 0;
        self.flipped = 0;
    }

    /// Seconds from a sound to the result that covers it (centre of the
    /// window to the newest sample the frame needs).
    pub fn latency(&self) -> f64 {
        let delay = if self.dec.factor() > 1 {
            DECIMATOR_DELAY
        } else {
            0
        };
        (self.w / 2 + self.tau_max + 2 + delay) as f64 / self.sr
    }

    /// Feeds mono samples; appends a result for every frame that is
    /// complete now.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<LivePitch>) {
        self.scratch.clear();
        self.dec.push(samples, &mut self.scratch);
        self.buf.extend_from_slice(&self.scratch);
        let span = self.w + self.tau_max + 2;
        let delay = if self.dec.factor() > 1 {
            DECIMATOR_DELAY
        } else {
            0
        };
        while self.next + span <= self.buf_start + self.buf.len() {
            let at = self.next - self.buf_start;
            let p = self.analyse(at);
            out.push(LivePitch {
                time: (self.next as f64 + self.w as f64 / 2.0 - delay as f64) / self.sr,
                ..p
            });
            self.next += self.hop;
            self.frame += 1;
        }
        let drop = (self.next - self.buf_start).min(self.buf.len());
        self.buf.drain(..drop);
        self.buf_start += drop;
    }

    fn analyse(&mut self, at: usize) -> LivePitch {
        let x = &self.buf[at..at + self.w + self.tau_max + 2];
        let rms = dsp::rms(&x[..self.w]);
        let unvoiced = LivePitch {
            time: 0.0,
            f0: None,
            confidence: 0.0,
            rms,
        };
        if rms < self.opts.silence {
            return unvoiced;
        }
        dsp::cmnd(x, self.w, &mut self.d);
        let d = &self.d;
        let Some(mut tau) = self.period(self.tau_min, self.tau_max) else {
            return unvoiced;
        };
        let confidence = (1.0 - d[tau]).clamp(0.0, 1.0);
        if d[tau] > self.opts.unvoiced {
            return LivePitch {
                confidence,
                ..unvoiced
            };
        }
        // Octave guard: near the previous pitch's octave?
        let recent = self.last.filter(|&(_, f)| self.frame - f <= 15);
        if let Some((prev, _)) = recent {
            let hz = (self.sr / tau as f64) as f32;
            let semis = 12.0 * (hz / prev).log2();
            let other = if (semis - 12.0).abs() < 1.0 {
                // An octave up: the true period may be twice as long.
                self.trough_near(2 * tau)
            } else if (semis + 12.0).abs() < 1.0 {
                self.trough_near(tau / 2)
            } else {
                None
            };
            match other {
                Some(t)
                    if d[t] <= d[tau] + self.opts.octave_tolerance
                        && self.flipped < self.opts.octave_hold =>
                {
                    tau = t;
                    self.flipped += 1;
                }
                _ => self.flipped = 0,
            }
        }
        let f0 = (self.sr / (tau as f64 + dsp::parabolic(d, tau) as f64)) as f32;
        if self.flipped == 0 || recent.is_none() {
            self.last = Some((f0, self.frame));
        } else {
            self.last = recent.map(|(p, _)| (p, self.frame));
        }
        LivePitch {
            time: 0.0,
            f0: Some(f0),
            confidence: (1.0 - d[tau]).clamp(0.0, 1.0),
            rms,
        }
    }

    /// YIN's choice in `lo..=hi`: the first trough under the threshold,
    /// else the lowest point.
    fn period(&self, lo: usize, hi: usize) -> Option<usize> {
        let d = &self.d;
        let hi = hi.min(d.len() - 2);
        if lo > hi {
            return None;
        }
        let mut tau = lo;
        while tau <= hi {
            if d[tau] < self.opts.threshold {
                while tau < hi && d[tau + 1] < d[tau] {
                    tau += 1;
                }
                return Some(tau);
            }
            tau += 1;
        }
        (lo..=hi).min_by(|&a, &b| d[a].total_cmp(&d[b]))
    }

    /// The lowest point of d' within a semitone of `tau`.
    fn trough_near(&self, tau: usize) -> Option<usize> {
        let lo = ((tau as f32 * 0.944) as usize).max(self.tau_min);
        let hi = ((tau as f32 * 1.06).ceil() as usize).min(self.tau_max);
        (lo..=hi).min_by(|&a, &b| self.d[a].total_cmp(&self.d[b]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn run(x: &[f32], rate: u32, chunk: usize) -> Vec<LivePitch> {
        let mut y = LiveYin::new(rate, LiveOptions::default());
        let mut out = Vec::new();
        for c in x.chunks(chunk) {
            y.push(c, &mut out);
        }
        out
    }

    fn sine(hz: impl Fn(f32) -> f32, secs: f32, rate: u32, h2: f32) -> Vec<f32> {
        let mut ph = 0f32;
        (0..(secs * rate as f32) as usize)
            .map(|i| {
                ph += 2.0 * PI * hz(i as f32 / rate as f32) / rate as f32;
                0.3 * (ph.sin() + h2 * (2.0 * ph).sin())
            })
            .collect()
    }

    #[test]
    fn follows_a_tone_in_small_chunks() {
        for rate in [16000, 44100, 48000] {
            let x = sine(|_| 330.0, 1.0, rate, 0.3);
            let out = run(&x, rate, 128);
            assert!(out.len() >= 90, "{} frames at {rate}", out.len());
            for p in &out[5..out.len() - 5] {
                let hz = p.f0.expect("voiced");
                assert!(
                    (1200.0 * (hz / 330.0).log2()).abs() < 10.0,
                    "{hz} at {rate}"
                );
                assert!(p.confidence > 0.8);
            }
            // Times step by the hop from about the first window's centre.
            assert!((out[1].time - out[0].time - 0.01).abs() < 0.001);
        }
        let y = LiveYin::new(48000, LiveOptions::default());
        assert!(y.latency() > 0.02 && y.latency() < 0.05, "{}", y.latency());
    }

    #[test]
    fn silence_and_times() {
        let mut x = vec![0f32; 16000];
        x.extend(sine(|_| 200.0, 0.5, 16000, 0.0));
        let out = run(&x, 16000, 1000);
        assert!(out.iter().filter(|p| p.time < 0.95).all(|p| p.f0.is_none()));
        let on = out.iter().find(|p| p.f0.is_some()).unwrap();
        assert!((on.time - 1.0).abs() < 0.03, "starts at {}", on.time);
    }

    #[test]
    fn octave_guard_holds_then_follows() {
        // 220 Hz, then a stretch where the period of 440 Hz fits about as
        // well (strong second harmonic, weak fundamental): the guard keeps
        // 220. A real jump to 440 is followed after a few frames.
        let mut x = sine(|_| 220.0, 0.3, 16000, 0.2);
        x.extend(sine(|_| 220.0, 0.05, 16000, 6.0));
        x.extend(sine(|_| 220.0, 0.2, 16000, 0.2));
        x.extend(sine(|_| 440.0, 0.4, 16000, 0.0));
        let out = run(&x, 16000, 160);
        let near =
            |p: &LivePitch, hz: f32| p.f0.is_some_and(|f| (12.0 * (f / hz).log2()).abs() < 0.5);
        let mid: Vec<&LivePitch> = out
            .iter()
            .filter(|p| p.time > 0.31 && p.time < 0.34)
            .collect();
        assert!(mid.iter().all(|p| near(p, 220.0)), "{mid:?}");
        let end: Vec<&LivePitch> = out
            .iter()
            .filter(|p| p.time > 0.7 && p.time < 0.9)
            .collect();
        assert!(end.iter().all(|p| near(p, 440.0)), "{end:?}");
    }
}
