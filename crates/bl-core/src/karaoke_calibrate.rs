//! Latency calibration of a microphone: the app plays a short track of
//! clicks; the microphone hears them through the speakers (or the singer
//! claps or sings "ta" on each). The onsets heard, on the song's clock,
//! against the clicks played give how late the input is; that becomes its
//! latency offset (`mic.setOffset`).

use crate::core::Core;
use crate::host::Host;
use crate::mic;
use bl_json::Value;

/// The clicks: the first after a second, then every `CLICK_EVERY` s.
const FIRST_CLICK: f64 = 1.0;
const CLICK_EVERY: f64 = 0.6;
const CLICKS: usize = 12;
const TRACK_RATE: u32 = 48000;
/// Name of the click track among the app's in-memory files.
pub(crate) const TRACK_NAME: &str = "Beatlumen click track.wav";
/// An onset counts for the click within this distance (s).
const MATCH: f64 = 0.3;
/// Clicks that must be heard.
const MIN_HEARD: usize = 5;

/// The click track as a 16-bit mono WAV.
pub(crate) fn click_track() -> Vec<u8> {
    let secs = FIRST_CLICK + CLICK_EVERY * CLICKS as f64 + 1.0;
    let frames = (secs * TRACK_RATE as f64) as usize;
    let mut s = vec![0i16; frames];
    for k in 0..CLICKS {
        let at = ((FIRST_CLICK + k as f64 * CLICK_EVERY) * TRACK_RATE as f64) as usize;
        // 20 ms of a decaying 1.6 kHz tone, the first of each four higher.
        let f = if k % 4 == 0 { 2400.0 } else { 1600.0 };
        for i in 0..(TRACK_RATE as usize / 50) {
            let t = i as f64 / TRACK_RATE as f64;
            let v = (t * f * std::f64::consts::TAU).sin() * (-t * 180.0).exp();
            if let Some(x) = s.get_mut(at + i) {
                *x = (v * 26000.0) as i16;
            }
        }
    }
    let data = (frames * 2) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&TRACK_RATE.to_le_bytes());
    w.extend_from_slice(&(TRACK_RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for v in s {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// Song times (s) of the clicks.
pub(crate) fn click_times() -> Vec<f64> {
    (0..CLICKS)
        .map(|k| FIRST_CLICK + k as f64 * CLICK_EVERY)
        .collect()
}

/// Finds onsets in a stream of samples: a frame's energy jumping well above
/// the level before it.
#[derive(Debug, Clone)]
pub(crate) struct Onsets {
    rate: u32,
    hop: usize,
    acc: f64,
    n: usize,
    /// Slowly following background level (mean square).
    floor: f64,
    /// Samples since the last onset.
    since: usize,
    /// Sample index of the next sample.
    pub index: u64,
}

impl Onsets {
    pub(crate) fn new(rate: u32) -> Onsets {
        Onsets {
            rate,
            hop: (rate as usize / 400).max(8),
            acc: 0.0,
            n: 0,
            floor: 1e-6,
            since: usize::MAX / 2,
            index: 0,
        }
    }

    /// Feeds samples; returns the sample indices where onsets start.
    pub(crate) fn push(&mut self, samples: &[f32]) -> Vec<u64> {
        let mut found = Vec::new();
        for &x in samples {
            self.acc += (x as f64) * (x as f64);
            self.n += 1;
            self.index += 1;
            self.since = self.since.saturating_add(1);
            if self.n < self.hop {
                continue;
            }
            let e = self.acc / self.n as f64;
            self.acc = 0.0;
            self.n = 0;
            let refractory = self.rate as usize / 5;
            if e > self.floor * 12.0 && e > 1e-5 && self.since > refractory {
                found.push(self.index - self.hop as u64);
                self.since = 0;
            }
            // The background follows quiet frames quickly, loud ones slowly.
            let k = if e < self.floor { 0.2 } else { 0.01 };
            self.floor += (e - self.floor) * k;
        }
        found
    }
}

/// How late the input is (s) from onsets and clicks on the same clock:
/// the median difference of the onsets matched to clicks, and how many.
pub(crate) fn latency(onsets: &[f64], clicks: &[f64]) -> Option<(f64, usize, f64)> {
    let mut d: Vec<f64> = clicks
        .iter()
        .filter_map(|c| {
            onsets
                .iter()
                .map(|o| o - c)
                .filter(|x| x.abs() <= MATCH)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        })
        .collect();
    if d.len() < MIN_HEARD {
        return None;
    }
    d.sort_by(f64::total_cmp);
    let median = d[d.len() / 2];
    let mut spread: Vec<f64> = d.iter().map(|x| (x - median).abs()).collect();
    spread.sort_by(f64::total_cmp);
    Some((median, d.len(), spread[spread.len() / 2]))
}

/// A calibration under way.
pub(crate) struct Calibration {
    pub stream: u32,
    pub device: String,
    onsets: Onsets,
    rate: u32,
    next: u64,
    heard: Vec<f64>,
    /// "running", "done", "failed".
    pub state: String,
    pub result: Value,
}

impl Core {
    /// `karaoke.calibrate {stream}`: plays the click track and listens.
    pub(crate) fn karaoke_calibrate(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let Some(stream) = c.get("stream").as_u64().map(|s| s as u32) else {
            return Value::object().with("error", "no stream");
        };
        let device = self
            .mic_streams_json()
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|s| s.get("stream").as_u64() == Some(stream as u64))
                    .map(|s| s.str_or("device", "").to_string())
            })
            .unwrap_or_default();
        if device.is_empty() {
            return Value::object().with("error", self.tr("The microphone is not open"));
        }
        let next = mic::latest(stream, 0).map_or(0, |b| b.index);
        self.karaoke_calibration = Some(Calibration {
            stream,
            device,
            onsets: Onsets::new(48000),
            rate: 0,
            next,
            heard: Vec::new(),
            state: "running".into(),
            result: Value::Null,
        });
        self.open_data(host, TRACK_NAME.to_string(), click_track());
        Value::object().with("ok", true)
    }

    /// Listens for the clicks while the click track plays (from `tick`).
    pub(crate) fn poll_karaoke_calibration(&mut self, host: &mut dyn Host) {
        let current_is_clicks = self
            .queue
            .current()
            .is_some_and(|id| id.ends_with(TRACK_NAME));
        let position = self.position(host);
        let playing = self.is_playing();
        let Some(cal) = &mut self.karaoke_calibration else {
            return;
        };
        if cal.state != "running" {
            return;
        }
        while let Some(b) = mic::read(cal.stream, cal.next, 8192) {
            if b.samples.is_empty() {
                break;
            }
            if b.rate != cal.rate || b.index != cal.next {
                cal.rate = b.rate;
                cal.onsets = Onsets::new(b.rate);
                cal.onsets.index = b.index;
            }
            cal.next = b.index + b.samples.len() as u64;
            for idx in cal.onsets.push(&b.samples) {
                let at = b.time_us + (idx as f64 - b.index as f64) * 1e6 / b.rate as f64;
                if playing && current_is_clicks {
                    if let Some(t) = mic::song_time(at) {
                        cal.heard.push(t);
                    }
                }
            }
        }
        let clicks = click_times();
        let end = clicks.last().copied().unwrap_or(0.0) + 0.6;
        let over = current_is_clicks && position >= end || !current_is_clicks && position > 0.5;
        if !over {
            return;
        }
        let device = cal.device.clone();
        match latency(&cal.heard, &clicks) {
            Some((late, heard, spread)) => {
                let before = self.mic_offset(&device) as f64;
                let ms = (before + late * 1000.0).clamp(-1000.0, 1000.0).round();
                let cal = self.karaoke_calibration.as_mut().expect("checked");
                cal.state = "done".into();
                cal.result = Value::object()
                    .with("offsetMs", ms)
                    .with("changeMs", (late * 1000.0).round())
                    .with("heard", heard as u64)
                    .with("spreadMs", (spread * 1000.0).round());
                self.mic_command(
                    host,
                    "mic.setOffset",
                    &Value::object().with("device", device).with("ms", ms),
                );
            }
            None => {
                let heard = cal.heard.len() as u64;
                cal.state = "failed".into();
                cal.result = Value::object().with("heard", heard);
            }
        }
        self.karaoke_session.rev += 1;
        self.player.pause(host);
    }

    pub(crate) fn karaoke_calibration_json(&self) -> Value {
        match &self.karaoke_calibration {
            None => Value::object().with("state", "idle"),
            Some(c) => Value::object()
                .with("state", c.state.as_str())
                .with("stream", c.stream as u64)
                .with("heard", c.heard.len() as u64)
                .with("result", c.result.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_are_found_and_timed() {
        // The click track as a microphone 85 ms late would hear it, with noise.
        let wav = click_track();
        let pcm: Vec<f32> = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect();
        let late = (0.085 * TRACK_RATE as f64) as usize;
        let mut heard = vec![0f32; late];
        let mut seed = 1u32;
        for (i, x) in pcm.iter().enumerate() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
            let _ = i;
            heard.push(x * 0.3 + noise * 0.01);
        }
        let mut o = Onsets::new(TRACK_RATE);
        let times: Vec<f64> = o
            .push(&heard)
            .into_iter()
            .map(|i| i as f64 / TRACK_RATE as f64)
            .collect();
        let (lat, n, spread) = latency(&times, &click_times()).expect("heard");
        assert_eq!(n, CLICKS);
        assert!((lat - 0.085).abs() < 0.006, "{lat}");
        assert!(spread < 0.004);
        // Too few clicks heard: nothing.
        assert!(latency(&times[..3], &click_times()).is_none());
    }
}
