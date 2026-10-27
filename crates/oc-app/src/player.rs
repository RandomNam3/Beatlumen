//! Audio playback: decoding, downmix, resampling and output.
//!
//! * Native (pull audio): a decoder thread fills a ring buffer that the
//!   platform's audio callback drains ([`Output`] implements
//!   [`AudioSource`]).
//! * Web (push audio): the same [`Pipeline`] is pumped every frame and the
//!   samples are pushed to the host (AudioWorklet).
//!
//! The playback position is derived from the frames actually consumed by
//! the device minus its reported latency, so visuals stay in sync.

use crate::host::{AudioSource, Host};
use oc_media::{Decoder, Resampler};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Opens the byte source of a track (called on the decoder thread).
pub type SourceOpener = Box<dyn FnOnce() -> Result<(Box<dyn oc_media::Source>, Option<String>), String> + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// Track opened: duration in seconds (if known), source format.
    Loaded { duration: Option<f64>, sample_rate: u32, channels: u16, codec: &'static str },
    /// Playback reached the end of the track.
    Ended,
    Error(String),
}

/// Decoded audio converted to interleaved stereo at the output rate.
pub struct Pipeline {
    decoder: Box<dyn Decoder>,
    in_rate: u32,
    in_channels: u16,
    out_rate: u32,
    resampler: Option<Resampler>,
    decoded: Vec<f32>,
    stereo: Vec<f32>,
    pub eof: bool,
}

impl Pipeline {
    pub fn new(decoder: Box<dyn Decoder>, out_rate: u32) -> Pipeline {
        let spec = decoder.spec();
        let resampler = (spec.sample_rate != out_rate && spec.sample_rate > 0).then(|| Resampler::new(spec.sample_rate, out_rate, 2));
        Pipeline {
            decoder,
            in_rate: spec.sample_rate.max(1),
            in_channels: spec.channels.max(1),
            out_rate,
            resampler,
            decoded: Vec::new(),
            stereo: Vec::new(),
            eof: false,
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.decoder.total_frames().map(|f| f as f64 / self.in_rate as f64)
    }

    pub fn codec(&self) -> &'static str {
        self.decoder.codec_name()
    }

    pub fn source_rate(&self) -> u32 {
        self.in_rate
    }

    pub fn source_channels(&self) -> u16 {
        self.in_channels
    }

    /// Decodes the next chunk, appending stereo samples at the output rate.
    /// Returns false at the end of the stream.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        if self.eof {
            return Ok(false);
        }
        self.decoded.clear();
        let more = self.decoder.next_chunk(&mut self.decoded).map_err(|e| e.to_string())?;
        if !more {
            self.eof = true;
        }
        self.stereo.clear();
        oc_media::downmix_to_stereo(&self.decoded, self.in_channels, &mut self.stereo);
        match &mut self.resampler {
            Some(r) => r.process(&self.stereo, out),
            None => out.extend_from_slice(&self.stereo),
        }
        Ok(more || !self.decoded.is_empty())
    }

    /// Seeks to `seconds`; returns the actual position in seconds.
    pub fn seek(&mut self, seconds: f64) -> Result<f64, String> {
        let frame = (seconds.max(0.0) * self.in_rate as f64) as u64;
        let actual = self.decoder.seek(frame).map_err(|e| e.to_string())?;
        if let Some(r) = &mut self.resampler {
            r.reset();
        }
        self.eof = false;
        Ok(actual as f64 / self.in_rate as f64)
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }
}

/// Recently played mono audio, for visualizers and live analysis.
pub struct Tap {
    /// Absolute output frame index of `buf[0]`.
    start: u64,
    buf: VecDeque<f32>,
    cap: usize,
}

impl Tap {
    fn new(cap: usize) -> Tap {
        Tap { start: 0, buf: VecDeque::with_capacity(cap), cap }
    }
    fn push_stereo(&mut self, stereo: &[f32]) {
        for f in stereo.chunks_exact(2) {
            if self.buf.len() == self.cap {
                self.buf.pop_front();
                self.start += 1;
            }
            self.buf.push_back((f[0] + f[1]) * 0.5);
        }
    }
    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }
}

struct Ring {
    samples: VecDeque<f32>,
    /// Output frame index (since track start) of the first queued frame.
    next_frame: u64,
}

/// State shared with the audio callback.
pub struct Output {
    ring: Mutex<Ring>,
    tap: Mutex<Tap>,
    paused: AtomicBool,
    volume: AtomicU32,
    current_gain: Mutex<f32>,
    /// Output frame index (since track start, including the seek base) of
    /// the last frame handed to the device.
    played: AtomicU64,
    latency: AtomicU32,
    underruns: AtomicU64,
    pub sample_rate: AtomicU32,
}

impl Output {
    fn new() -> Output {
        Output {
            ring: Mutex::new(Ring { samples: VecDeque::new(), next_frame: 0 }),
            tap: Mutex::new(Tap::new(48000 * 4)),
            paused: AtomicBool::new(true),
            volume: AtomicU32::new(0.5f32.to_bits()),
            current_gain: Mutex::new(0.0),
            played: AtomicU64::new(0),
            latency: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            sample_rate: AtomicU32::new(48000),
        }
    }

    fn queued_frames(&self) -> usize {
        self.ring.lock().map(|r| r.samples.len() / 2).unwrap_or(0)
    }

    fn reset(&self, base_frame: u64) {
        if let Ok(mut r) = self.ring.lock() {
            r.samples.clear();
            r.next_frame = base_frame;
        }
        self.played.store(base_frame, Ordering::SeqCst);
        if let Ok(mut t) = self.tap.lock() {
            t.buf.clear();
            t.start = base_frame;
        }
    }

    fn push(&self, stereo: &[f32]) {
        if let Ok(mut r) = self.ring.lock() {
            r.samples.extend(stereo.iter().copied());
        }
    }
}

impl AudioSource for Output {
    fn render(&self, out: &mut [f32], latency_frames: u32) {
        self.latency.store(latency_frames, Ordering::Relaxed);
        let target = if self.paused.load(Ordering::Relaxed) { 0.0 } else { f32::from_bits(self.volume.load(Ordering::Relaxed)) };
        let mut gain = self.current_gain.lock().map(|g| *g).unwrap_or(target);
        if self.paused.load(Ordering::Relaxed) && gain <= 1e-4 {
            out.fill(0.0);
            if let Ok(mut g) = self.current_gain.lock() {
                *g = 0.0;
            }
            return;
        }
        let frames = out.len() / 2;
        let mut taken = 0usize;
        if let Ok(mut r) = self.ring.lock() {
            let avail = r.samples.len() / 2;
            taken = avail.min(frames);
            for i in 0..taken * 2 {
                out[i] = r.samples.pop_front().unwrap_or(0.0);
            }
            r.next_frame += taken as u64;
        }
        if taken < frames {
            out[taken * 2..].fill(0.0);
            if !self.paused.load(Ordering::Relaxed) {
                self.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Ok(mut t) = self.tap.lock() {
            t.push_stereo(&out[..taken * 2]);
        }
        // Smooth gain changes (pause/resume fades, volume changes).
        let step = 1.0 / (0.02 * self.sample_rate.load(Ordering::Relaxed).max(1) as f32);
        for f in out[..taken * 2].chunks_exact_mut(2) {
            if gain < target {
                gain = (gain + step).min(target);
            } else if gain > target {
                gain = (gain - step).max(target);
            }
            // Perceptual volume curve.
            let g = gain * gain;
            f[0] *= g;
            f[1] *= g;
        }
        if let Ok(mut g) = self.current_gain.lock() {
            *g = gain;
        }
        self.played.fetch_add(taken as u64, Ordering::SeqCst);
    }
}

enum Command {
    Seek(f64),
    Stop,
}

struct NativeTrack {
    commands: std::sync::mpsc::Sender<Command>,
    events: std::sync::mpsc::Receiver<ThreadEvent>,
}

enum ThreadEvent {
    Loaded { duration: Option<f64>, sample_rate: u32, channels: u16, codec: &'static str },
    Seeked(f64),
    Eof,
    Error(String),
}

/// Web (push) state.
struct PushTrack {
    pipeline: Pipeline,
    /// Frames pushed to the host since `base`.
    pushed: u64,
    base_secs: f64,
    eof_pushed: bool,
}

pub struct Player {
    output: Arc<Output>,
    device_rate: u32,
    device_started: bool,
    state: PlayState,
    duration: Option<f64>,
    /// Seek base in seconds (position = base + played frames).
    base_secs: f64,
    native: Option<NativeTrack>,
    push: Option<PushTrack>,
    eof: bool,
    events: Vec<PlayerEvent>,
    volume: f32,
    error: Option<String>,
    scratch: Vec<f32>,
    pending_seek: Option<f64>,
}

impl Default for Player {
    fn default() -> Self {
        Player::new()
    }
}

impl Player {
    pub fn new() -> Player {
        Player {
            output: Arc::new(Output::new()),
            device_rate: 48000,
            device_started: false,
            state: PlayState::Stopped,
            duration: None,
            base_secs: 0.0,
            native: None,
            push: None,
            eof: false,
            events: Vec::new(),
            volume: 0.5,
            error: None,
            scratch: Vec::new(),
            pending_seek: None,
        }
    }

    fn ensure_device(&mut self, host: &mut dyn Host) {
        if self.device_started || host.audio_is_push() {
            if host.audio_is_push() {
                self.device_rate = 48000;
            }
            return;
        }
        match host.audio_start(self.output.clone()) {
            Ok(info) => {
                self.device_rate = info.sample_rate;
                self.output.sample_rate.store(info.sample_rate, Ordering::Relaxed);
                self.device_started = true;
            }
            Err(e) => {
                host.log(&format!("audio device unavailable: {e}"));
                self.device_started = true;
            }
        }
    }

    pub fn state(&self) -> PlayState {
        self.state
    }

    pub fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.output.volume.store(self.volume.to_bits(), Ordering::Relaxed);
    }

    pub fn last_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Current playback position in seconds.
    pub fn position(&self, host: &dyn Host) -> f64 {
        if let Some(s) = self.pending_seek {
            return s;
        }
        let rate = self.device_rate.max(1) as f64;
        let pos = if let Some(p) = &self.push {
            let queued = host.audio_queued() as u64;
            p.base_secs + p.pushed.saturating_sub(queued) as f64 / rate
        } else {
            let played = self.output.played.load(Ordering::SeqCst);
            let lat = self.output.latency.load(Ordering::Relaxed) as u64;
            self.base_secs + played.saturating_sub(lat) as f64 / rate
        };
        match self.duration {
            Some(d) => pos.min(d),
            None => pos,
        }
    }

    /// Mono samples recently played (ending at the current position);
    /// fills `out` completely (zeros where unavailable).
    pub fn recent_samples(&self, out: &mut [f32]) {
        out.fill(0.0);
        if let Ok(t) = self.output.tap.lock() {
            let lat = self.output.latency.load(Ordering::Relaxed) as u64;
            let end = t.end().saturating_sub(lat);
            let n = out.len() as u64;
            let start = end.saturating_sub(n);
            for (i, o) in out.iter_mut().enumerate() {
                let f = start + i as u64;
                if f >= t.start && f < t.end() {
                    *o = t.buf[(f - t.start) as usize];
                }
            }
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// Stops playback and releases the track.
    pub fn stop(&mut self, host: &mut dyn Host) {
        if let Some(t) = self.native.take() {
            let _ = t.commands.send(Command::Stop);
        }
        if self.push.take().is_some() {
            host.audio_clear();
        }
        self.output.paused.store(true, Ordering::Relaxed);
        self.output.reset(0);
        self.state = PlayState::Stopped;
        self.duration = None;
        self.base_secs = 0.0;
        self.eof = false;
        self.pending_seek = None;
    }

    /// Loads a track; `opener` opens the byte source (on a background
    /// thread on native platforms).
    pub fn load(&mut self, host: &mut dyn Host, opener: SourceOpener, autoplay: bool) {
        self.stop(host);
        self.ensure_device(host);
        self.error = None;
        self.state = PlayState::Loading;
        let out_rate = self.device_rate;
        if host.has_threads() && !host.audio_is_push() {
            let (ctx, crx) = std::sync::mpsc::channel();
            let (etx, erx) = std::sync::mpsc::channel();
            let output = self.output.clone();
            output.reset(0);
            output.paused.store(!autoplay, Ordering::Relaxed);
            std::thread::Builder::new()
                .name("oc-decoder".into())
                .spawn(move || decoder_thread(opener, out_rate, output, crx, etx))
                .ok();
            self.native = Some(NativeTrack { commands: ctx, events: erx });
        } else {
            // Push mode: open synchronously.
            match opener().and_then(|(src, ext)| oc_media::open(src, ext.as_deref()).map_err(|e| e.to_string())) {
                Ok(dec) => {
                    let pipeline = Pipeline::new(dec, out_rate);
                    self.duration = pipeline.duration();
                    self.events.push(PlayerEvent::Loaded {
                        duration: self.duration,
                        sample_rate: pipeline.source_rate(),
                        channels: pipeline.source_channels(),
                        codec: pipeline.codec(),
                    });
                    self.push = Some(PushTrack { pipeline, pushed: 0, base_secs: 0.0, eof_pushed: false });
                    host.audio_clear();
                }
                Err(e) => {
                    self.error = Some(e.clone());
                    self.events.push(PlayerEvent::Error(e));
                    self.state = PlayState::Stopped;
                    return;
                }
            }
        }
        self.state = if autoplay { PlayState::Playing } else { PlayState::Paused };
        if self.push.is_some() {
            host.audio_set_paused(!autoplay);
        }
    }

    pub fn play(&mut self, host: &mut dyn Host) {
        if self.native.is_none() && self.push.is_none() {
            return;
        }
        if self.eof && self.state != PlayState::Playing {
            // Restart a finished track.
            self.seek(host, 0.0);
        }
        self.state = PlayState::Playing;
        self.output.paused.store(false, Ordering::Relaxed);
        host.audio_set_paused(false);
    }

    pub fn pause(&mut self, host: &mut dyn Host) {
        if self.state == PlayState::Playing || self.state == PlayState::Loading {
            self.state = PlayState::Paused;
        }
        self.output.paused.store(true, Ordering::Relaxed);
        host.audio_set_paused(true);
    }

    pub fn toggle(&mut self, host: &mut dyn Host) {
        if self.state == PlayState::Playing {
            self.pause(host);
        } else {
            self.play(host);
        }
    }

    pub fn seek(&mut self, host: &mut dyn Host, seconds: f64) {
        let seconds = match self.duration {
            Some(d) => seconds.clamp(0.0, (d - 0.05).max(0.0)),
            None => seconds.max(0.0),
        };
        self.eof = false;
        if let Some(t) = &self.native {
            let _ = t.commands.send(Command::Seek(seconds));
            self.pending_seek = Some(seconds);
        } else if let Some(p) = &mut self.push {
            host.audio_clear();
            match p.pipeline.seek(seconds) {
                Ok(actual) => {
                    p.base_secs = actual;
                    p.pushed = 0;
                    p.eof_pushed = false;
                }
                Err(e) => self.events.push(PlayerEvent::Error(e)),
            }
        }
    }

    /// Per-frame housekeeping: collects decoder events and pumps audio in
    /// push mode.
    pub fn update(&mut self, host: &mut dyn Host) {
        if let Some(t) = &self.native {
            let mut ended = false;
            for ev in t.events.try_iter() {
                match ev {
                    ThreadEvent::Loaded { duration, sample_rate, channels, codec } => {
                        self.duration = duration;
                        self.events.push(PlayerEvent::Loaded { duration, sample_rate, channels, codec });
                    }
                    ThreadEvent::Seeked(actual) => {
                        self.base_secs = actual;
                        self.pending_seek = None;
                    }
                    ThreadEvent::Eof => self.eof = true,
                    ThreadEvent::Error(e) => {
                        self.error = Some(e.clone());
                        self.events.push(PlayerEvent::Error(e));
                        ended = true;
                    }
                }
            }
            if self.eof && self.state == PlayState::Playing && self.output.queued_frames() == 0 && self.pending_seek.is_none() {
                ended = true;
            }
            if ended {
                self.eof = true;
                self.state = PlayState::Paused;
                self.output.paused.store(true, Ordering::Relaxed);
                self.events.push(PlayerEvent::Ended);
            }
        }
        if let Some(p) = &mut self.push {
            if self.state == PlayState::Playing {
                let target = (self.device_rate as f64 * 0.6) as u32;
                let mut guard = 0;
                while host.audio_queued() < target && !p.eof_pushed && guard < 64 {
                    guard += 1;
                    self.scratch.clear();
                    match p.pipeline.next(&mut self.scratch) {
                        Ok(more) => {
                            if !self.scratch.is_empty() {
                                host.audio_push(&self.scratch);
                                p.pushed += (self.scratch.len() / 2) as u64;
                                if let Ok(mut t) = self.output.tap.lock() {
                                    t.push_stereo(&self.scratch);
                                }
                            }
                            if !more {
                                p.eof_pushed = true;
                            }
                        }
                        Err(e) => {
                            self.events.push(PlayerEvent::Error(e));
                            p.eof_pushed = true;
                        }
                    }
                }
                if p.eof_pushed && host.audio_queued() == 0 {
                    self.eof = true;
                    self.state = PlayState::Paused;
                    host.audio_set_paused(true);
                    self.events.push(PlayerEvent::Ended);
                }
            }
        }
    }

    pub fn underruns(&self) -> u64 {
        self.output.underruns.load(Ordering::Relaxed)
    }
}

fn decoder_thread(
    opener: SourceOpener,
    out_rate: u32,
    output: Arc<Output>,
    commands: std::sync::mpsc::Receiver<Command>,
    events: std::sync::mpsc::Sender<ThreadEvent>,
) {
    let mut pipeline = match opener().and_then(|(src, ext)| oc_media::open(src, ext.as_deref()).map_err(|e| e.to_string())) {
        Ok(d) => Pipeline::new(d, out_rate),
        Err(e) => {
            let _ = events.send(ThreadEvent::Error(e));
            return;
        }
    };
    let _ = events.send(ThreadEvent::Loaded {
        duration: pipeline.duration(),
        sample_rate: pipeline.source_rate(),
        channels: pipeline.source_channels(),
        codec: pipeline.codec(),
    });
    let max_queue = out_rate as usize; // one second
    let mut buf = Vec::with_capacity(8192);
    let mut eof_sent = false;
    loop {
        // Commands.
        loop {
            match commands.try_recv() {
                Ok(Command::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Ok(Command::Seek(s)) => {
                    let actual = pipeline.seek(s).unwrap_or(s);
                    output.reset(0);
                    eof_sent = false;
                    let _ = events.send(ThreadEvent::Seeked(actual));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        if pipeline.eof || output.queued_frames() >= max_queue {
            if pipeline.eof && !eof_sent {
                eof_sent = true;
                let _ = events.send(ThreadEvent::Eof);
            }
            match commands.recv_timeout(std::time::Duration::from_millis(15)) {
                Ok(Command::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(Command::Seek(s)) => {
                    let actual = pipeline.seek(s).unwrap_or(s);
                    output.reset(0);
                    eof_sent = false;
                    let _ = events.send(ThreadEvent::Seeked(actual));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            continue;
        }
        buf.clear();
        match pipeline.next(&mut buf) {
            Ok(_) => output.push(&buf),
            Err(e) => {
                let _ = events.send(ThreadEvent::Error(e));
                pipeline.eof = true;
            }
        }
    }
}
