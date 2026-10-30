//! Beat prediction for live audio (other apps, AirPlay, DLNA, radio): the
//! tempo from the autocorrelation of the onset envelope of the last
//! seconds, the phase from a comb over the same envelope. With both, the
//! next beats are known before they sound, so effects can land on them
//! instead of reacting after them (the analysis window, the frame and the
//! output latency would otherwise all add up).

use std::collections::VecDeque;

/// Seconds of envelope kept for the estimate.
const WINDOW_SECS: f64 = 8.0;
/// Seconds needed before a first estimate.
const MIN_SECS: f64 = 3.0;
/// Tempo range considered (beats per minute).
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
/// How long a predicted pulse lasts (decay time constant, seconds).
const PULSE_DECAY: f64 = 0.07;

pub struct BeatTracker {
    /// Envelope frames per second.
    rate: f64,
    env: VecDeque<f32>,
    /// Stream time of the newest envelope frame.
    t_last: f64,
    /// Seconds per beat (0 while unknown).
    period: f64,
    /// Stream time of a beat.
    anchor: f64,
    confidence: f32,
    since_estimate: usize,
}

impl BeatTracker {
    /// `rate`: envelope frames per second (the analyser's hop rate).
    pub fn new(rate: f64) -> BeatTracker {
        BeatTracker {
            rate: rate.max(1.0),
            env: VecDeque::new(),
            t_last: 0.0,
            period: 0.0,
            anchor: 0.0,
            confidence: 0.0,
            since_estimate: 0,
        }
    }

    pub fn reset(&mut self) {
        self.env.clear();
        self.period = 0.0;
        self.confidence = 0.0;
        self.since_estimate = 0;
    }

    /// The onset strength of the frame ending at stream time `t`.
    pub fn push(&mut self, onset: f32, t: f64) {
        // A jump in time (seek, new stream): start over.
        if !self.env.is_empty() && (t < self.t_last || t - self.t_last > 1.0) {
            self.reset();
        }
        self.env.push_back(onset.max(0.0));
        self.t_last = t;
        let max = (WINDOW_SECS * self.rate) as usize;
        while self.env.len() > max {
            self.env.pop_front();
        }
        self.since_estimate += 1;
        // About four estimates a second.
        if self.since_estimate as f64 >= self.rate / 4.0 {
            self.since_estimate = 0;
            self.estimate();
        }
    }

    /// Beats per minute (None while unsure).
    pub fn bpm(&self) -> Option<f64> {
        (self.period > 0.0 && self.confidence > 0.0).then(|| 60.0 / self.period)
    }

    /// How sure the estimate is (0..1).
    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// The first predicted beat at or after `t`.
    pub fn next_beat(&self, t: f64) -> Option<f64> {
        if self.period <= 0.0 || self.confidence <= 0.0 {
            return None;
        }
        let k = ((t - self.anchor) / self.period).ceil();
        Some(self.anchor + k * self.period)
    }

    /// A pulse at the predicted beats as heard at time `t`: 1 on the beat,
    /// decaying until the next, scaled by the confidence.
    pub fn pulse(&self, t: f64) -> f32 {
        if self.period <= 0.0 || self.confidence <= 0.0 {
            return 0.0;
        }
        // Rounding must not put a time on the beat just before it.
        let phase = (t - self.anchor) / self.period + 1e-6;
        let since = (phase - phase.floor()) * self.period;
        ((-since / PULSE_DECAY).exp() as f32) * self.confidence
    }

    fn estimate(&mut self) {
        let n = self.env.len();
        if (n as f64) < MIN_SECS * self.rate {
            return;
        }
        // Onsets fall between frames: a slight blur lets beats that are not
        // a whole number of frames apart still line up.
        let raw: Vec<f32> = self.env.iter().copied().collect();
        let blurred: Vec<f32> = (0..n)
            .map(|i| {
                let at = |d: isize| raw.get((i as isize + d) as usize).copied().unwrap_or(0.0);
                at(-2) * 0.25 + at(-1) * 0.6 + at(0) + at(1) * 0.6 + at(2) * 0.25
            })
            .collect();
        let mean = blurred.iter().sum::<f32>() / n as f32;
        let x: Vec<f32> = blurred.iter().map(|v| v - mean).collect();
        let energy: f32 = x.iter().map(|v| v * v).sum::<f32>() / n as f32;
        if energy <= 1e-9 {
            self.confidence = 0.0;
            return;
        }
        let lag_min = ((60.0 / MAX_BPM) * self.rate).floor().max(1.0) as usize;
        let lag_max = (((60.0 / MIN_BPM) * self.rate).ceil() as usize).min(n / 2);
        if lag_min >= lag_max {
            return;
        }
        let acf = |lag: usize| -> f32 {
            let mut s = 0.0f32;
            for i in lag..n {
                s += x[i] * x[i - lag];
            }
            s / (n - lag) as f32
        };
        let values: Vec<f32> = (lag_min..=lag_max).map(acf).collect();
        // A mild preference for common tempos (around 120 BPM) against
        // half and double tempo.
        let weight = |lag: f64| -> f32 {
            let bpm = 60.0 * self.rate / lag;
            let d = (bpm / 120.0).log2();
            (-0.5 * (d / 1.2) * (d / 1.2)).exp() as f32
        };
        let (best_i, best) = values
            .iter()
            .enumerate()
            .map(|(i, v)| (i, v * weight((lag_min + i) as f64)))
            .fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
        if best <= 0.0 {
            self.confidence = 0.0;
            return;
        }
        // Sub-frame lag by a parabola through the neighbours.
        let lag = {
            let (l, c, r) = (
                values.get(best_i.wrapping_sub(1)).copied().unwrap_or(0.0),
                values[best_i],
                values.get(best_i + 1).copied().unwrap_or(0.0),
            );
            let den = l - 2.0 * c + r;
            let off = if den.abs() > 1e-12 {
                0.5 * (l - r) / den
            } else {
                0.0
            };
            (lag_min + best_i) as f64 + off.clamp(-0.5, 0.5) as f64
        };
        let period = lag / self.rate;
        // Phase: the offset whose comb (every period back) collects most.
        let steps = lag.round().max(1.0) as usize;
        let mut best_off = 0usize;
        let mut best_sum = f32::MIN;
        for off in 0..steps {
            let mut s = 0.0f32;
            let mut k = 0.0f64;
            loop {
                let back = off as f64 + k * lag;
                let idx = n as f64 - 1.0 - back;
                if idx < 0.0 {
                    break;
                }
                // Recent beats count more.
                s += self.env[idx.round() as usize] * (0.85f32).powf(k as f32);
                k += 1.0;
            }
            if s > best_sum {
                best_sum = s;
                best_off = off;
            }
        }
        let anchor = self.t_last - best_off as f64 / self.rate;
        let confidence = (values[best_i] / energy).clamp(0.0, 1.0);
        // Follow gently while the tempo holds; jump when it changed.
        if self.period > 0.0 && (period / self.period - 1.0).abs() < 0.04 {
            self.period = self.period * 0.7 + period * 0.3;
        } else {
            self.period = period;
        }
        self.anchor = anchor;
        self.confidence = self.confidence * 0.5 + confidence * 0.5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An envelope with onsets at `bpm` from time `start`, a bit noisy.
    fn feed(t: &mut BeatTracker, rate: f64, bpm: f64, secs: f64, start: f64) {
        let period = 60.0 / bpm;
        let mut seed = 7u32;
        for i in 0..(secs * rate) as usize {
            let time = i as f64 / rate;
            let since = (time - start).rem_euclid(period);
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let noise = (seed >> 16) as f32 / 65536.0 * 0.15;
            let onset = if since < 1.0 / rate { 1.0 } else { 0.0 } + noise;
            t.push(onset, time);
        }
    }

    #[test]
    fn finds_tempo_and_phase() {
        for bpm in [92.0, 120.0, 128.0, 174.0] {
            let rate = 47.0;
            let mut t = BeatTracker::new(rate);
            feed(&mut t, rate, bpm, 10.0, 0.13);
            let got = t.bpm().expect("a tempo");
            assert!((got - bpm).abs() < bpm * 0.03, "{bpm}: {got}");
            assert!(t.confidence() > 0.3, "{bpm}: {}", t.confidence());
            // The next beat lies on the grid of the input.
            let period = 60.0 / bpm;
            let next = t.next_beat(10.0).unwrap();
            let off = (next - 0.13).rem_euclid(period);
            let err = off.min(period - off);
            assert!(err < 0.045, "{bpm}: phase off by {err}");
            assert!(t.pulse(next) > t.pulse(next + period * 0.5));
        }
    }

    #[test]
    fn silence_and_jumps() {
        let mut t = BeatTracker::new(47.0);
        for i in 0..470 {
            t.push(0.0, i as f64 / 47.0);
        }
        assert!(t.bpm().is_none());
        assert_eq!(t.pulse(5.0), 0.0);
        feed(&mut t, 47.0, 120.0, 6.0, 0.0);
        // A seek starts over.
        t.push(1.0, 100.0);
        assert!(t.bpm().is_none());
    }
}
