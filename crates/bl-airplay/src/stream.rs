//! The received audio between the network threads and the player: packets
//! are kept by RTP timestamp and handed to the player shortly before the
//! sender wants them to be heard, so the output stays in sync with the
//! sender (video on the sending device, other speakers) and the player's
//! own buffer stays small.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Assumed delay until playback when the sender sends no sync packets.
const DEFAULT_LATENCY: Duration = Duration::from_secs(2);
/// Most audio kept ahead (guards memory against a misbehaving sender).
const MAX_BUFFER_SECS: f64 = 8.0;
/// Waiting slice of the player-side reader (keeps the player responsive).
const WAIT: Duration = Duration::from_millis(20);
/// Audio handed to the player ahead of time, so its buffer bridges
/// scheduling hiccups.
const CUSHION: Duration = Duration::from_millis(250);
/// How long a missing packet may be late (retransmission) before silence
/// takes its place.
const MISSING_GRACE: Duration = Duration::from_millis(100);
/// Time over which volume changes are smoothed.
const GAIN_RAMP_SECS: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Format {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames_per_packet: u32,
}

struct State {
    format: Format,
    /// Decoded packets by unwrapped RTP timestamp.
    packets: BTreeMap<i64, Vec<f32>>,
    /// Reference for unwrapping 32-bit timestamps.
    ref_ts: Option<(u32, i64)>,
    /// Next frame to hand to the player.
    next: Option<i64>,
    /// Local time at which a given frame is to be heard.
    anchor: Option<(Instant, i64)>,
    first_packet: Option<Instant>,
    last_packet: Option<Instant>,
    /// Delay between handing audio to the player and hearing it.
    out_latency: Duration,
    /// Whether output of the current run started (the first frame waits
    /// for its time; later ones are handed over `CUSHION` ahead).
    started: bool,
    /// Paused by the sender: nothing is played until the next sync.
    held: bool,
    gain: f32,
    current_gain: f32,
    ended: bool,
    /// Incremented for every new stream (stale readers stop).
    generation: u64,
}

impl State {
    fn unwrap(&mut self, ts: u32) -> i64 {
        match self.ref_ts {
            Some((r, e)) => {
                let ext = e + ts.wrapping_sub(r) as i32 as i64;
                self.ref_ts = Some((ts, ext));
                ext
            }
            None => {
                self.ref_ts = Some((ts, ts as i64));
                ts as i64
            }
        }
    }

    fn peek(&self, ts: u32) -> i64 {
        match self.ref_ts {
            Some((r, e)) => e + ts.wrapping_sub(r) as i32 as i64,
            None => ts as i64,
        }
    }

    fn play_time(&self, frame: i64) -> Option<Instant> {
        let (at, base) = self.anchor?;
        let secs = (frame - base) as f64 / self.format.sample_rate.max(1) as f64;
        if secs >= 0.0 {
            at.checked_add(Duration::from_secs_f64(secs))
        } else {
            at.checked_sub(Duration::from_secs_f64(-secs))
        }
    }
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

/// Handle shared by the network threads (writing) and the player (reading
/// through [`LiveStream::decoder`]).
#[derive(Clone)]
pub struct LiveStream {
    shared: Arc<Shared>,
}

impl Default for LiveStream {
    fn default() -> Self {
        LiveStream::new()
    }
}

impl LiveStream {
    pub fn new() -> LiveStream {
        LiveStream {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    format: Format {
                        sample_rate: 44100,
                        channels: 2,
                        frames_per_packet: 352,
                    },
                    packets: BTreeMap::new(),
                    ref_ts: None,
                    next: None,
                    anchor: None,
                    first_packet: None,
                    last_packet: None,
                    out_latency: Duration::ZERO,
                    started: false,
                    held: false,
                    gain: 1.0,
                    current_gain: 1.0,
                    ended: true,
                    generation: 0,
                }),
                cv: Condvar::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts a new stream (a reader of the previous one ends); returns
    /// its generation, which writers pass along.
    pub fn begin(&self, format: Format) -> u64 {
        let mut st = self.lock();
        st.format = format;
        st.packets.clear();
        st.ref_ts = None;
        st.next = None;
        st.anchor = None;
        st.first_packet = None;
        st.last_packet = None;
        st.started = false;
        st.held = false;
        st.ended = false;
        st.gain = 1.0;
        st.current_gain = 1.0;
        st.generation += 1;
        self.shared.cv.notify_all();
        st.generation
    }

    /// Ends the stream: the reader reports the end once.
    pub fn end(&self) {
        let mut st = self.lock();
        st.ended = true;
        st.packets.clear();
        self.shared.cv.notify_all();
    }

    pub fn is_active(&self) -> bool {
        !self.lock().ended
    }

    /// True while audio arrives (false while the sender is paused).
    pub fn receiving(&self) -> bool {
        let st = self.lock();
        !st.ended
            && st
                .last_packet
                .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
    }

    pub fn format(&self) -> Format {
        self.lock().format
    }

    /// Adds a decoded packet (interleaved samples) of stream `generation`.
    pub fn push(&self, generation: u64, ts: u32, pcm: Vec<f32>) {
        let mut st = self.lock();
        if st.ended || st.generation != generation || pcm.is_empty() {
            return;
        }
        let ext = st.unwrap(ts);
        if st.next.is_some_and(|n| ext < n) {
            return; // too late
        }
        let rate = st.format.sample_rate.max(1) as f64;
        if let Some((&first, _)) = st.packets.iter().next() {
            if (ext - first) as f64 / rate > MAX_BUFFER_SECS {
                return;
            }
        }
        let now = Instant::now();
        st.first_packet.get_or_insert(now);
        st.last_packet = Some(now);
        st.packets.insert(ext, pcm);
        self.shared.cv.notify_all();
    }

    /// Sync packet: `frame` is to be heard at `at`.
    pub fn sync(&self, generation: u64, frame: u32, at: Instant) {
        let mut st = self.lock();
        if st.generation != generation {
            return;
        }
        let ext = st.peek(frame);
        st.anchor = Some((at, ext));
        st.held = false;
        self.shared.cv.notify_all();
    }

    /// Pauses output until the next [`sync`](Self::sync) (buffered audio
    /// stays; AirPlay 2 senders set the rate to 0).
    pub fn hold(&self) {
        let mut st = self.lock();
        st.held = true;
        st.anchor = None;
        st.next = None;
        st.started = false;
        self.shared.cv.notify_all();
    }

    /// Seconds of audio buffered ahead of what was handed to the player;
    /// `None` once `generation` is no longer the current stream.
    pub fn buffered_secs(&self, generation: u64) -> Option<f64> {
        let st = self.lock();
        if st.ended || st.generation != generation {
            return None;
        }
        let ch = st.format.channels.max(1) as i64;
        let (Some((&first, _)), Some((&last, pcm))) =
            (st.packets.iter().next(), st.packets.iter().next_back())
        else {
            return Some(0.0);
        };
        let start = st.next.map_or(first, |n| n.min(first));
        let end = last + pcm.len() as i64 / ch;
        Some((end - start) as f64 / st.format.sample_rate.max(1) as f64)
    }

    /// Drops buffered packets with timestamps in `[from, until)` (a skip
    /// the sender announced ahead of time); playback continues.
    pub fn remove_range(&self, from: u32, until: u32) {
        let mut st = self.lock();
        let (a, b) = (st.peek(from), st.peek(until));
        if b > a {
            let mut tail = st.packets.split_off(&a);
            let mut keep = tail.split_off(&b);
            st.packets.append(&mut keep);
            drop(tail);
        }
    }

    /// Drops buffered audio before `until` (all of it when `None`), e.g.
    /// when the sender pauses or skips.
    pub fn flush(&self, until: Option<u32>) {
        let mut st = self.lock();
        match until {
            Some(ts) => {
                let ext = st.peek(ts);
                st.packets = st.packets.split_off(&ext);
            }
            None => st.packets.clear(),
        }
        st.next = None;
        st.anchor = None;
        st.first_packet = None;
        st.started = false;
        self.shared.cv.notify_all();
    }

    /// Volume set by the sender (linear gain).
    pub fn set_volume(&self, gain: f32) {
        self.lock().gain = gain.clamp(0.0, 1.0);
    }

    /// Delay from handing audio to the player until it is heard (player
    /// buffer target plus the device's output latency).
    pub fn set_output_latency(&self, secs: f64) {
        let secs = secs.clamp(0.0, 1.5);
        self.lock().out_latency = Duration::from_secs_f64(secs);
    }

    /// A decoder that plays this stream until it ends.
    pub fn decoder(&self) -> LiveDecoder {
        let st = self.lock();
        LiveDecoder {
            stream: self.clone(),
            generation: st.generation,
            format: st.format,
        }
    }
}

/// Reads a [`LiveStream`] for the player.
pub struct LiveDecoder {
    stream: LiveStream,
    generation: u64,
    format: Format,
}

impl LiveDecoder {
    fn wait<'a>(&self, st: MutexGuard<'a, State>, d: Duration) -> MutexGuard<'a, State> {
        match self.stream.shared.cv.wait_timeout(st, d.min(WAIT)) {
            Ok((g, _)) => g,
            Err(e) => e.into_inner().0,
        }
    }

    /// Appends the packet at `frame` (or silence) with the volume ramp.
    fn emit(st: &mut State, frame: i64, out: &mut Vec<f32>) -> i64 {
        let ch = st.format.channels.max(1) as usize;
        let pcm = st.packets.remove(&frame);
        let frames = pcm
            .as_ref()
            .map_or(st.format.frames_per_packet as usize, |p| p.len() / ch);
        let step = 1.0 / (GAIN_RAMP_SECS * st.format.sample_rate.max(1) as f32);
        for i in 0..frames {
            let g = st.current_gain;
            for c in 0..ch {
                let v = pcm.as_ref().map_or(0.0, |p| p[i * ch + c]);
                out.push(v * g);
            }
            if st.current_gain < st.gain {
                st.current_gain = (st.current_gain + step).min(st.gain);
            } else if st.current_gain > st.gain {
                st.current_gain = (st.current_gain - step).max(st.gain);
            }
        }
        frame + frames as i64
    }
}

impl bl_media::Decoder for LiveDecoder {
    fn spec(&self) -> bl_media::AudioSpec {
        bl_media::AudioSpec {
            sample_rate: self.format.sample_rate,
            channels: self.format.channels,
        }
    }

    fn total_frames(&self) -> Option<u64> {
        None
    }

    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, bl_media::Error> {
        let mut st = self.stream.lock();
        if st.ended || st.generation != self.generation {
            return Ok(false);
        }
        let now = Instant::now();
        let rate = st.format.sample_rate.max(1) as f64;
        if st.held {
            drop(self.wait(st, WAIT));
            return Ok(true);
        }
        if st.anchor.is_none() {
            // No sync packet yet: play with the usual delay after the first
            // packet.
            match (st.first_packet, st.packets.keys().next().copied()) {
                (Some(t), Some(first)) if t.elapsed() > Duration::from_millis(300) => {
                    st.anchor = Some((t + DEFAULT_LATENCY, first));
                }
                _ => {
                    drop(self.wait(st, WAIT));
                    return Ok(true);
                }
            }
        }
        if st.next.is_none() {
            match st.packets.keys().next().copied() {
                Some(first) => st.next = Some(first),
                None => {
                    drop(self.wait(st, WAIT));
                    return Ok(true);
                }
            }
        }
        let next = st.next.expect("set above");
        let Some(play_at) = st.play_time(next) else {
            drop(self.wait(st, WAIT));
            return Ok(true);
        };
        // The first frame of a run is handed over when it is due (the
        // player starts playing at once); after that, audio goes ahead by
        // the cushion, which then stays in the player's buffer.
        let lead = if st.started {
            st.out_latency + CUSHION
        } else {
            st.out_latency
        };
        let release = play_at.checked_sub(lead).unwrap_or(play_at);
        if now < release {
            drop(self.wait(st, release - now));
            return Ok(true);
        }
        st.started = true;
        // Far behind (e.g. after a stall): continue with what is due now.
        if now > release + Duration::from_millis(500) {
            let behind = (now - release).as_secs_f64();
            let target = next + (behind * rate) as i64;
            st.packets = st.packets.split_off(&target);
            st.next = Some(st.packets.keys().next().copied().unwrap_or(target));
            return Ok(true);
        }
        if !st.packets.contains_key(&next) {
            // Give a missing packet time to arrive (retransmission).
            let deadline = release + MISSING_GRACE;
            if now < deadline {
                drop(self.wait(st, deadline - now));
                return Ok(true);
            }
            // Nothing buffered and no audio coming: the sender paused
            // without a flush; wait instead of filling the player with
            // silence, and start over with the next packets.
            let later = st.packets.range(next + 1..).next().is_some();
            let recent = st
                .last_packet
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
            if !later && !recent {
                st.next = None;
                st.anchor = None;
                st.first_packet = None;
                st.started = false;
                drop(self.wait(st, WAIT));
                return Ok(true);
            }
        }
        let after = Self::emit(&mut st, next, out);
        st.next = Some(after);
        Ok(true)
    }

    fn seek(&mut self, _frame: u64) -> Result<u64, bl_media::Error> {
        Err(bl_media::Error::Unsupported("live stream".into()))
    }

    fn codec_name(&self) -> &'static str {
        "airplay"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_media::Decoder;

    fn fmt() -> Format {
        Format {
            sample_rate: 1000,
            channels: 2,
            frames_per_packet: 10,
        }
    }

    fn drain(d: &mut LiveDecoder, until: Duration) -> Vec<f32> {
        let mut out = Vec::new();
        let end = Instant::now() + until;
        while Instant::now() < end {
            d.next_chunk(&mut out).unwrap();
        }
        out
    }

    #[test]
    fn plays_in_timestamp_order_at_the_synced_time() {
        let s = LiveStream::new();
        let g = s.begin(fmt());
        s.set_output_latency(0.0);
        // Frame 100 is to be heard 300 ms from now: it is handed over then,
        // the next one right after (cushion).
        let start = Instant::now();
        s.sync(g, 100, start + Duration::from_millis(300));
        s.push(g, 110, vec![0.5; 20]);
        s.push(g, 100, vec![0.25; 20]);
        let mut d = s.decoder();
        let mut out = Vec::new();
        while out.is_empty() {
            d.next_chunk(&mut out).unwrap();
        }
        assert!(start.elapsed() >= Duration::from_millis(280));
        while out.len() < 40 {
            d.next_chunk(&mut out).unwrap();
        }
        assert_eq!(out.len(), 40);
        assert!((out[0] - 0.25).abs() < 1e-6 && (out[39] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn missing_packets_become_silence_and_late_ones_are_dropped() {
        let s = LiveStream::new();
        let g = s.begin(fmt());
        s.set_output_latency(0.0);
        s.sync(g, 0, Instant::now());
        s.push(g, 0, vec![1.0; 20]);
        s.push(g, 20, vec![1.0; 20]); // 10..20 missing
        let mut d = s.decoder();
        let out = drain(&mut d, Duration::from_millis(60));
        assert!(out.len() >= 60);
        assert!(out[..20].iter().all(|&v| v == 1.0));
        assert!(out[20..40].iter().all(|&v| v == 0.0));
        assert!(out[40..60].iter().all(|&v| v == 1.0));
        s.push(g, 10, vec![1.0; 20]); // arrives too late
        assert!(!drain(&mut d, Duration::from_millis(30)).contains(&1.0));
    }

    #[test]
    fn timestamps_wrap_around() {
        let s = LiveStream::new();
        let g = s.begin(fmt());
        s.set_output_latency(0.0);
        let base = u32::MAX - 9;
        s.sync(g, base, Instant::now());
        s.push(g, base, vec![0.1; 20]);
        s.push(g, base.wrapping_add(10), vec![0.2; 20]);
        let mut d = s.decoder();
        let out = drain(&mut d, Duration::from_millis(50));
        assert!(out.len() >= 40);
        assert!((out[0] - 0.1).abs() < 1e-6 && (out[39] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn ending_or_restarting_ends_the_reader() {
        let s = LiveStream::new();
        let _g = s.begin(fmt());
        let mut d = s.decoder();
        s.begin(fmt());
        assert!(!d.next_chunk(&mut Vec::new()).unwrap());
        let mut d = s.decoder();
        s.end();
        assert!(!d.next_chunk(&mut Vec::new()).unwrap());
    }

    #[test]
    fn volume_ramps() {
        let s = LiveStream::new();
        let g = s.begin(fmt());
        s.set_output_latency(0.0);
        s.set_volume(0.0);
        s.sync(g, 0, Instant::now());
        for i in 0..10 {
            s.push(g, i * 10, vec![1.0; 20]);
        }
        let mut d = s.decoder();
        let out = drain(&mut d, Duration::from_millis(120));
        assert!(out[0] > 0.9 && out[out.len() - 1] == 0.0);
    }
}
