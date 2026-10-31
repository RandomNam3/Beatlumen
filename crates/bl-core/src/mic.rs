//! Microphones (karaoke): the platform lists its audio inputs, records the
//! ones the user opens and hands their audio over through [`push_pcm`]
//! (`bl_mic_push`). Every opened input is a stream with a ring buffer of
//! mono samples at the device rate, stamped on the core's clock
//! ([`clock_us`]), and a level meter. Scoring reads the samples by index
//! ([`read`]) and places them in the song with [`song_time`].
//!
//! The audio stays on the device: it is only kept for a few seconds in
//! memory and never stored or sent anywhere.

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// Audio kept per stream (seconds).
pub const RING_SECS: f64 = 4.0;
/// A reported time further than this from the stream's timeline starts a
/// new one (dropped audio, a stalled device).
const RESYNC_US: f64 = 40_000.0;
/// Share of the difference between a reported time and the timeline that
/// moves the timeline (follows clock drift, averages out jitter).
const TIMELINE_FOLLOW: f64 = 0.05;
/// Level meter: the level falls by this many decibels per second, the
/// peak is held this long before falling.
const LEVEL_FALL_DB_PER_SEC: f32 = 30.0;
const PEAK_HOLD_US: i64 = 1_000_000;
/// Latency offsets the user can set (milliseconds).
pub const MAX_OFFSET_MS: f32 = 1000.0;

// ---- the clock -----------------------------------------------------------------

/// Microseconds on the core's monotonic clock (process wide). Platforms
/// stamp recorded audio with it (`bl_clock_us`). Starts at one second, so
/// times are never 0 or less (which means "unknown").
pub fn clock_us() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as i64 + 1_000_000
}

// ---- streams (from the platform's recording threads) ---------------------------

/// A section of a stream's audio on the clock: sample `index` was
/// recorded at `us`, the following ones `1e6 / rate` µs apart each.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    index: u64,
    us: f64,
}

struct Stream {
    /// Channel of the device's audio (None: all channels mixed).
    channel: Option<u32>,
    rate: u32,
    /// Mono samples; `ring[0]` is sample `start` of the stream.
    ring: VecDeque<f32>,
    start: u64,
    /// The newest last; the first covers `start`.
    timeline: VecDeque<Segment>,
    /// Moves the samples earlier on the clock (the input's unreported
    /// latency), in µs.
    offset_us: f64,
    /// Meter: RMS level and peak (0..1, linear) with their ballistics.
    level: f32,
    peak: f32,
    peak_at: i64,
    clipped: u64,
    last_push_us: i64,
}

impl Stream {
    fn new(channel: Option<u32>) -> Stream {
        Stream {
            channel,
            rate: 0,
            ring: VecDeque::new(),
            start: 0,
            timeline: VecDeque::new(),
            offset_us: 0.0,
            level: 0.0,
            peak: 0.0,
            peak_at: 0,
            clipped: 0,
            last_push_us: 0,
        }
    }

    fn end(&self) -> u64 {
        self.start + self.ring.len() as u64
    }

    /// Clock time of sample `index` (before the offset).
    fn time_of(&self, index: u64) -> Option<f64> {
        let seg = self
            .timeline
            .iter()
            .rev()
            .find(|s| s.index <= index)
            .or_else(|| self.timeline.front())?;
        Some(seg.us + (index as f64 - seg.index as f64) * 1e6 / self.rate.max(1) as f64)
    }

    fn push(&mut self, pcm: &[i16], channels: usize, rate: u32, time_us: i64) {
        if rate != self.rate {
            // Another format: a new stream of samples.
            self.ring.clear();
            self.timeline.clear();
            self.rate = rate;
        }
        let frames = pcm.len() / channels;
        let first = self.end();
        // The timeline: follows the reported times slowly, starts anew when
        // they jump.
        let reported = time_us as f64;
        match (self.time_of(first), self.timeline.back_mut()) {
            (Some(predicted), Some(seg)) if (reported - predicted).abs() < RESYNC_US => {
                seg.us += (reported - predicted) * TIMELINE_FOLLOW;
            }
            _ => self.timeline.push_back(Segment {
                index: first,
                us: reported,
            }),
        }
        let pick = self.channel.map(|c| (c as usize).min(channels - 1));
        let mut sum = 0.0f64;
        let mut peak = 0.0f32;
        for f in pcm.chunks_exact(channels) {
            let s = match pick {
                Some(c) => f[c] as f32 / 32768.0,
                None => f.iter().map(|&x| x as f32).sum::<f32>() / (32768.0 * channels as f32),
            };
            sum += (s * s) as f64;
            let a = s.abs();
            peak = peak.max(a);
            if a >= 0.999 {
                self.clipped += 1;
            }
            self.ring.push_back(s);
        }
        let max = (rate as f64 * RING_SECS) as usize;
        if self.ring.len() > max {
            let drop = self.ring.len() - max;
            self.ring.drain(..drop);
            self.start += drop as u64;
            while self.timeline.len() > 1 && self.timeline[1].index <= self.start {
                self.timeline.pop_front();
            }
        }
        // Meter.
        let now = clock_us();
        let dt = if self.last_push_us > 0 {
            ((now - self.last_push_us) as f32 / 1e6).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.last_push_us = now;
        let rms = if frames > 0 {
            (sum / frames as f64).sqrt() as f32
        } else {
            0.0
        };
        let fall = 10f32.powf(-LEVEL_FALL_DB_PER_SEC * dt / 20.0);
        self.level = rms.max(self.level * fall);
        if peak >= self.peak {
            self.peak = peak;
            self.peak_at = now;
        } else if now - self.peak_at > PEAK_HOLD_US {
            self.peak = peak.max(self.peak * fall);
        }
    }
}

#[derive(Default)]
struct Streams {
    map: HashMap<u32, Stream>,
}

fn streams() -> MutexGuard<'static, Streams> {
    static S: OnceLock<Mutex<Streams>> = OnceLock::new();
    S.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Hands recorded audio of stream `stream` to the core (interleaved
/// 16-bit PCM). `time_us` is when its first frame was recorded on
/// [`clock_us`]; 0 or less: unknown (it arrived just now). Ignored unless
/// the stream is open. Called from any thread.
pub fn push_pcm(stream: u32, pcm: &[i16], channels: u32, rate: u32, time_us: i64) {
    if channels == 0 || !(8000..=192_000).contains(&rate) || pcm.len() < channels as usize {
        return;
    }
    let ch = channels as usize;
    let time_us = if time_us > 0 {
        time_us
    } else {
        clock_us() - (pcm.len() / ch) as i64 * 1_000_000 / rate as i64
    };
    if let Some(s) = streams().map.get_mut(&stream) {
        s.push(pcm, ch, rate, time_us);
    }
}

/// A stream id not used before in this process (the buffers are process
/// wide).
fn new_stream_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn open_stream(stream: u32, channel: Option<u32>, offset_ms: f32) {
    let mut s = Stream::new(channel);
    s.offset_us = offset_ms as f64 * 1000.0;
    streams().map.insert(stream, s);
}

fn close_stream(stream: u32) {
    streams().map.remove(&stream);
}

fn set_stream_offset(stream: u32, offset_ms: f32) {
    if let Some(s) = streams().map.get_mut(&stream) {
        s.offset_us = offset_ms as f64 * 1000.0;
    }
}

/// Recorded audio of a stream, as [`read`] returns it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub rate: u32,
    /// Index of the first sample since the stream opened (or changed its
    /// rate); the next read continues at `index + samples.len()`.
    pub index: u64,
    /// When the first sample was sung, on [`clock_us`] (latency offset
    /// applied); the others follow `1e6 / rate` µs apart.
    pub time_us: f64,
    pub samples: Vec<f32>,
}

/// Up to `max` samples of `stream` from sample `index` on (from the oldest
/// kept one when `index` is older). None when the stream is not open or
/// has no audio yet.
pub fn read(stream: u32, index: u64, max: usize) -> Option<Block> {
    let st = streams();
    let s = st.map.get(&stream)?;
    if s.ring.is_empty() {
        return None;
    }
    let from = index.clamp(s.start, s.end());
    let n = ((s.end() - from) as usize).min(max);
    let skip = (from - s.start) as usize;
    Some(Block {
        rate: s.rate,
        index: from,
        time_us: s.time_of(from)? - s.offset_us,
        samples: s.ring.range(skip..skip + n).copied().collect(),
    })
}

/// The newest `n` samples of `stream`.
pub fn latest(stream: u32, n: usize) -> Option<Block> {
    let end = {
        let st = streams();
        let s = st.map.get(&stream)?;
        s.end()
    };
    read(stream, end.saturating_sub(n as u64), n)
}

/// The open streams.
pub fn stream_ids() -> Vec<u32> {
    let mut v: Vec<u32> = streams().map.keys().copied().collect();
    v.sort_unstable();
    v
}

/// A stream's meter: (level, peak, clipped samples, rate, last audio µs).
fn meter(stream: u32) -> Option<(f32, f32, u64, u32, i64)> {
    let st = streams();
    let s = st.map.get(&stream)?;
    // The meter falls while no audio arrives.
    let idle = ((clock_us() - s.last_push_us) as f32 / 1e6 - 0.1).max(0.0);
    let fall = 10f32.powf(-LEVEL_FALL_DB_PER_SEC * idle / 20.0);
    let level = if s.last_push_us > 0 {
        s.level * fall
    } else {
        0.0
    };
    let peak = if s.last_push_us > 0 {
        s.peak * fall
    } else {
        0.0
    };
    Some((level, peak, s.clipped, s.rate, s.last_push_us))
}

// ---- the song's clock ---------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SongClock {
    /// Position heard at `at_us` (seconds).
    position: f64,
    at_us: f64,
    playing: bool,
    valid: bool,
}

fn song_clock() -> MutexGuard<'static, SongClock> {
    static C: OnceLock<Mutex<SongClock>> = OnceLock::new();
    C.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Notes the playback position heard now (the core, every frame).
/// Small differences to the running clock are smoothed (the position moves
/// in steps of the audio device's buffers); jumps (seeks, other tracks)
/// are taken as they are.
pub(crate) fn update_song_clock(position: f64, playing: bool, now_us: i64) {
    let mut c = song_clock();
    let now = now_us as f64;
    if c.valid && c.playing && playing {
        let predicted = c.position + (now - c.at_us) / 1e6;
        if (position - predicted).abs() < 0.05 {
            c.position = predicted + (position - predicted) * 0.1;
            c.at_us = now;
            return;
        }
    }
    *c = SongClock {
        position,
        at_us: now,
        playing,
        valid: true,
    };
}

/// The song position (seconds) heard at `time_us` on [`clock_us`]; None
/// before playback was ever reported. While paused: the paused position.
pub fn song_time(time_us: f64) -> Option<f64> {
    let c = *song_clock();
    if !c.valid {
        return None;
    }
    Some(if c.playing {
        c.position + (time_us - c.at_us) / 1e6
    } else {
        c.position
    })
}

// ---- core side --------------------------------------------------------------------

/// An audio input the platform offers.
#[derive(Clone, Debug, Default, PartialEq)]
struct Device {
    id: String,
    name: String,
    channels: u32,
    rate: u32,
    default: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Open {
    stream: u32,
    device: String,
    channel: Option<u32>,
    /// The platform records it.
    running: bool,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct MicState {
    devices: Vec<Device>,
    /// The platform answered a list request.
    listed: bool,
    open: Vec<Open>,
}

impl Core {
    fn mic_offset(&self, device: &str) -> f32 {
        self.settings
            .mic_offsets
            .iter()
            .find(|(d, _)| d == device)
            .map(|(_, ms)| *ms)
            .unwrap_or(0.0)
    }

    fn mic_device_name(&self, device: &str) -> String {
        self.mic
            .devices
            .iter()
            .find(|d| d.id == device)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| device.to_string())
    }

    pub(crate) fn mic_open_count(&self) -> usize {
        self.mic.open.len()
    }

    /// Closes every microphone (the app goes away).
    pub(crate) fn close_mics(&mut self, host: &mut dyn Host) {
        for o in std::mem::take(&mut self.mic.open) {
            host.mic_request("close", &Value::object().with("stream", o.stream).to_json());
            close_stream(o.stream);
        }
    }

    fn close_mic(&mut self, host: &mut dyn Host, stream: u32) -> bool {
        let Some(i) = self.mic.open.iter().position(|o| o.stream == stream) else {
            return false;
        };
        self.mic.open.remove(i);
        host.mic_request("close", &Value::object().with("stream", stream).to_json());
        close_stream(stream);
        true
    }

    pub(crate) fn mic_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let ok = || Value::object().with("ok", true);
        let err = |e: &str| Value::object().with("error", e);
        match name {
            // The inputs (last known; asks the platform for the current
            // ones, which arrive as "mic.devices").
            "mic.list" => {
                if host.has_mic() {
                    host.mic_request("list", "{}");
                }
                self.mic_json(host)
            }
            // From the platform: its inputs.
            "mic.devices" => {
                let devices = c.get("devices").as_array().cloned().unwrap_or_default();
                self.mic.devices = devices
                    .iter()
                    .filter_map(|d| {
                        let id = d.str_or("id", "");
                        (!id.is_empty()).then(|| Device {
                            id: id.chars().take(512).collect(),
                            name: d.str_or("name", id).chars().take(200).collect(),
                            channels: d.get("channels").as_u64().unwrap_or(1).clamp(1, 64) as u32,
                            rate: d.get("rate").as_u64().unwrap_or(0).min(192_000) as u32,
                            default: d.bool_or("default", false),
                        })
                    })
                    .take(256)
                    .collect();
                self.mic.listed = true;
                ok()
            }
            // Opens an input: {device, channel (optional: one channel of
            // the device instead of all mixed)} -> {stream}.
            "mic.open" => {
                if !host.has_mic() {
                    return err("not available");
                }
                let device = c.str_or("device", "").to_string();
                if device.is_empty() {
                    return err("no device");
                }
                let channel = c
                    .get("channel")
                    .as_i64()
                    .filter(|&ch| ch >= 0)
                    .map(|ch| ch.min(63) as u32);
                if let Some(o) = self
                    .mic
                    .open
                    .iter()
                    .find(|o| o.device == device && o.channel == channel)
                {
                    if o.error.is_none() {
                        return Value::object().with("stream", o.stream);
                    }
                    // Failed before: tried again.
                    let stream = o.stream;
                    self.close_mic(host, stream);
                }
                if self.mic.open.len() >= 16 {
                    return err("too many microphones");
                }
                let stream = new_stream_id();
                open_stream(stream, channel, self.mic_offset(&device));
                let req = Value::object()
                    .with("stream", stream)
                    .with("device", device.as_str());
                if !host.mic_request("open", &req.to_json()) {
                    close_stream(stream);
                    return err("not available");
                }
                host.log(&format!(
                    "mic: opening {} as stream {stream}",
                    self.mic_device_name(&device)
                ));
                self.mic.open.push(Open {
                    stream,
                    device,
                    channel,
                    running: false,
                    error: None,
                });
                Value::object().with("stream", stream)
            }
            // {stream} or {device}: closes it (all of the device's).
            "mic.close" => {
                if let Some(stream) = c.get("stream").as_u64() {
                    return Value::object().with("ok", self.close_mic(host, stream as u32));
                }
                let device = c.str_or("device", "");
                let streams: Vec<u32> = self
                    .mic
                    .open
                    .iter()
                    .filter(|o| o.device == device)
                    .map(|o| o.stream)
                    .collect();
                for s in &streams {
                    self.close_mic(host, *s);
                }
                Value::object().with("ok", !streams.is_empty())
            }
            "mic.closeAll" => {
                self.close_mics(host);
                ok()
            }
            // From the platform: {stream, running, error}.
            "mic.state" => {
                let stream = c.get("stream").as_u64().unwrap_or(0) as u32;
                let running = c.bool_or("running", false);
                let error = c.str_or("error", "");
                let Some(o) = self.mic.open.iter_mut().find(|o| o.stream == stream) else {
                    return ok();
                };
                o.running = running;
                if !error.is_empty() {
                    host.log(&format!("mic: stream {stream}: {error}"));
                    o.error = Some(error.chars().take(300).collect());
                }
                // Ended on its own (unplugged, permission denied): the
                // audio stops, the entry stays with its error until closed.
                if !running {
                    close_stream(stream);
                }
                ok()
            }
            // The open streams with their meters.
            "mic.levels" => Value::object().with("streams", self.mic_streams_json()),
            // {device, ms}: the input's latency (positive: its audio
            // arrives that much later than the platform reports).
            "mic.setOffset" => {
                let device = c.str_or("device", "").to_string();
                if device.is_empty() {
                    return err("no device");
                }
                let ms = c
                    .get("ms")
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(-MAX_OFFSET_MS as f64, MAX_OFFSET_MS as f64)
                    as f32;
                self.settings.mic_offsets.retain(|(d, _)| *d != device);
                if ms != 0.0 {
                    self.settings.mic_offsets.push((device.clone(), ms));
                }
                for o in self.mic.open.iter().filter(|o| o.device == device) {
                    set_stream_offset(o.stream, ms);
                }
                self.settings_changed();
                ok()
            }
            _ => err(&format!("unknown command {name}")),
        }
    }

    fn mic_streams_json(&self) -> Value {
        Value::Array(
            self.mic
                .open
                .iter()
                .map(|o| {
                    let (level, peak, clipped, rate, last) =
                        meter(o.stream).unwrap_or((0.0, 0.0, 0, 0, 0));
                    let db = |v: f32| if v > 1e-5 { 20.0 * v.log10() } else { -100.0 };
                    Value::object()
                        .with("stream", o.stream)
                        .with("device", o.device.as_str())
                        .with("name", self.mic_device_name(&o.device))
                        .with("channel", o.channel.map(|c| c as i64).unwrap_or(-1))
                        .with("running", o.running)
                        .with("error", o.error.clone())
                        .with("rate", rate)
                        .with("level", level)
                        .with("peak", peak)
                        .with("levelDb", db(level))
                        .with("peakDb", db(peak))
                        .with("clipped", clipped)
                        .with(
                            "ageMs",
                            if last > 0 {
                                (clock_us() - last) / 1000
                            } else {
                                -1
                            },
                        )
                        .with("offsetMs", self.mic_offset(&o.device))
                })
                .collect(),
        )
    }

    pub(crate) fn mic_json(&self, host: &dyn Host) -> Value {
        let devices: Vec<Value> = self
            .mic
            .devices
            .iter()
            .map(|d| {
                Value::object()
                    .with("id", d.id.as_str())
                    .with("name", d.name.as_str())
                    .with("channels", d.channels)
                    .with("rate", d.rate)
                    .with("default", d.default)
                    .with("offsetMs", self.mic_offset(&d.id))
            })
            .collect();
        Value::object()
            .with("available", host.has_mic())
            .with("listed", self.mic.listed)
            .with("devices", Value::Array(devices))
            .with("streams", self.mic_streams_json())
    }

    /// Notes where the song is for [`song_time`] (every frame).
    pub(crate) fn mic_tick(&mut self, host: &dyn Host) {
        if self.mic.open.is_empty() {
            return;
        }
        let playing = self.is_playing();
        update_song_clock(self.position(host), playing, clock_us());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, channels: usize, amp: i16) -> Vec<i16> {
        (0..frames * channels)
            .map(|i| if (i / channels) % 2 == 0 { amp } else { -amp })
            .collect()
    }

    #[test]
    fn ring_keeps_the_newest_seconds_with_their_times() {
        let id = 9001;
        open_stream(id, None, 0.0);
        let rate = 8000;
        // 10 s of audio in 10 ms chunks, recorded 1 s ago onwards.
        let t0 = 1_000_000_000i64;
        for k in 0..1000i64 {
            push_pcm(id, &tone(80, 1, 1000), 1, rate, t0 + k * 10_000);
        }
        let b = read(id, 0, usize::MAX).unwrap();
        assert_eq!(b.samples.len(), (rate as f64 * RING_SECS) as usize);
        assert_eq!(b.index, 80_000 - b.samples.len() as u64);
        // Time of the oldest kept sample: 6 s after the first.
        assert!(
            (b.time_us - (t0 as f64 + 6e6)).abs() < 100.0,
            "{}",
            b.time_us
        );
        // Reading on from the end: nothing new yet.
        let next = read(id, b.index + b.samples.len() as u64, 100).unwrap();
        assert!(next.samples.is_empty());
        let l = latest(id, 80).unwrap();
        assert_eq!(l.samples.len(), 80);
        assert!((l.time_us - (t0 as f64 + 9.99e6)).abs() < 100.0);
        close_stream(id);
        assert!(read(id, 0, 10).is_none());
        // Closed: pushed audio is ignored.
        push_pcm(id, &tone(80, 1, 1000), 1, rate, t0);
        assert!(read(id, 0, 10).is_none());
    }

    #[test]
    fn timeline_smooths_jitter_and_resyncs_on_gaps() {
        let id = 9002;
        open_stream(id, None, 0.0);
        let rate = 48000;
        let t0 = 5_000_000i64;
        // 480-frame chunks (10 ms), reported with ±3 ms jitter.
        for k in 0..100i64 {
            let jitter = if k % 2 == 0 { 3000 } else { -3000 };
            push_pcm(id, &tone(480, 1, 100), 1, rate, t0 + k * 10_000 + jitter);
        }
        let b = read(id, 0, usize::MAX).unwrap();
        // The first chunk sets the timeline; the jitter moves it little.
        let last = b.index + b.samples.len() as u64 - 480;
        let st = streams();
        let t = st.map[&id].time_of(last).unwrap();
        drop(st);
        assert!((t - (t0 as f64 + 99.0 * 10_000.0)).abs() < 3500.0, "{t}");
        // A gap of 200 ms (dropped audio): a new segment.
        push_pcm(id, &tone(480, 1, 100), 1, rate, t0 + 1_200_000);
        let l = latest(id, 480).unwrap();
        assert!((l.time_us - (t0 as f64 + 1_200_000.0)).abs() < 1.0);
        // The earlier audio keeps its times.
        let st = streams();
        let t2 = st.map[&id].time_of(last).unwrap();
        assert_eq!(t, t2);
        drop(st);
        close_stream(id);
    }

    #[test]
    fn channels_meter_and_offset() {
        let id = 9003;
        // Channel 1 of a stereo device: left silent, right full scale.
        open_stream(id, Some(1), 20.0);
        let pcm: Vec<i16> = (0..960)
            .map(|i| if i % 2 == 0 { 0 } else { 32767 })
            .collect();
        push_pcm(id, &pcm, 2, 48000, 2_000_000);
        let b = read(id, 0, 1000).unwrap();
        assert_eq!(b.samples.len(), 480);
        assert!(b.samples.iter().all(|&s| s > 0.99));
        // The offset moves the audio earlier.
        assert!((b.time_us - 1_980_000.0).abs() < 1.0);
        let (level, peak, clipped, rate, _) = meter(id).unwrap();
        assert!(level > 0.99 && peak > 0.99);
        assert_eq!(clipped, 480);
        assert_eq!(rate, 48000);
        set_stream_offset(id, 0.0);
        assert!((read(id, 0, 1).unwrap().time_us - 2_000_000.0).abs() < 1.0);
        close_stream(id);

        // All channels mixed.
        open_stream(id, None, 0.0);
        push_pcm(id, &pcm, 2, 48000, 2_000_000);
        let b = read(id, 0, 1000).unwrap();
        assert!(b.samples.iter().all(|&s| (s - 0.5).abs() < 0.01));
        // Silence afterwards: the level falls, the peak is held.
        push_pcm(id, &vec![0i16; 960], 2, 48000, 2_010_000);
        let (level, peak, ..) = meter(id).unwrap();
        assert!(level > 0.4 && peak > 0.49);
        close_stream(id);
    }

    #[test]
    fn song_clock_follows_and_jumps() {
        update_song_clock(10.0, true, 1_000_000);
        assert!((song_time(1_500_000.0).unwrap() - 10.5).abs() < 1e-9);
        // The position moves in steps: smoothed.
        update_song_clock(11.01, true, 2_000_000);
        let t = song_time(2_000_000.0).unwrap();
        assert!(t > 11.0 && t < 11.002, "{t}");
        // A seek: taken as it is.
        update_song_clock(42.0, true, 3_000_000);
        assert_eq!(song_time(3_000_000.0), Some(42.0));
        // Paused: stays.
        update_song_clock(42.5, false, 4_000_000);
        assert_eq!(song_time(9_000_000.0), Some(42.5));
    }
}
