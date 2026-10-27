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
//!
//! Gapless playback (native): the next track's opener is handed to the
//! decoder thread in advance ([`Player::preload`]); at the end of the
//! current track the thread continues decoding the next one into the same
//! ring, and [`PlayerEvent::Advanced`] is emitted once it becomes audible.

use crate::host::{AudioSource, Host};
use bl_media::{Decoder, Resampler};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Opens the byte source of a track (called on the decoder thread).
pub type SourceOpener =
    Box<dyn FnOnce() -> Result<(Box<dyn bl_media::Source>, Option<String>), String> + Send>;

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
    Loaded {
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
        codec: &'static str,
    },
    /// Playback reached the end of the track.
    Ended,
    /// The preloaded next track started playing seamlessly.
    Advanced {
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
        codec: &'static str,
    },
    /// A seek could not be applied because the decoder already moved on to
    /// the preloaded track: reload the current track and seek there.
    Reload(f64),
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
    /// Resampler output frames still to drop (its filter delay).
    skip: usize,
    /// Input frames decoded and output frames emitted since the start, to
    /// cut the flushed filter tail at the exact end of the track.
    in_frames: u64,
    out_frames: u64,
}

impl Pipeline {
    pub fn new(decoder: Box<dyn Decoder>, out_rate: u32) -> Pipeline {
        let spec = decoder.spec();
        let resampler = (spec.sample_rate != out_rate && spec.sample_rate > 0)
            .then(|| Resampler::new(spec.sample_rate, out_rate, 2));
        Pipeline {
            decoder,
            in_rate: spec.sample_rate.max(1),
            in_channels: spec.channels.max(1),
            out_rate,
            resampler,
            decoded: Vec::new(),
            stereo: Vec::new(),
            eof: false,
            skip: 0,
            in_frames: 0,
            out_frames: 0,
        }
        .primed()
    }

    fn primed(mut self) -> Pipeline {
        self.skip = self.resampler.as_ref().map_or(0, |r| r.latency());
        self
    }

    pub fn duration(&self) -> Option<f64> {
        self.decoder
            .total_frames()
            .map(|f| f as f64 / self.in_rate as f64)
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
        let more = self
            .decoder
            .next_chunk(&mut self.decoded)
            .map_err(|e| e.to_string())?;
        if !more {
            self.eof = true;
        }
        self.stereo.clear();
        bl_media::downmix_to_stereo(&self.decoded, self.in_channels, &mut self.stereo);
        match &mut self.resampler {
            Some(r) => {
                // Output aligned with the input: drop the filter delay at the
                // start and flush it at the end (gapless transitions).
                let before = out.len();
                r.process(&self.stereo, out);
                self.in_frames += (self.stereo.len() / 2) as u64;
                if !more {
                    let pad = (r.latency() as u64 * self.in_rate as u64
                        / self.out_rate.max(1) as u64) as usize
                        + 64;
                    self.stereo.clear();
                    self.stereo.resize(pad * 2, 0.0);
                    r.process(&self.stereo, out);
                }
                let drop = self.skip.min((out.len() - before) / 2);
                if drop > 0 {
                    out.drain(before..before + drop * 2);
                    self.skip -= drop;
                }
                if !more {
                    let expected = self.in_frames * self.out_rate as u64 / self.in_rate as u64;
                    let have = self.out_frames + ((out.len() - before) / 2) as u64;
                    let cut = have.saturating_sub(expected) as usize;
                    out.truncate(out.len() - cut.min((out.len() - before) / 2) * 2);
                }
                self.out_frames += ((out.len() - before) / 2) as u64;
            }
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
            self.skip = r.latency();
        }
        self.in_frames = actual;
        self.out_frames = actual * self.out_rate as u64 / self.in_rate as u64;
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
        Tap {
            start: 0,
            buf: VecDeque::with_capacity(cap),
            cap,
        }
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
    /// True while a track is streaming (an empty ring is an underrun, not
    /// the start or the end of playback).
    streaming: AtomicBool,
    pub sample_rate: AtomicU32,
}

impl Output {
    fn new() -> Output {
        Output {
            ring: Mutex::new(Ring {
                samples: VecDeque::new(),
                next_frame: 0,
            }),
            tap: Mutex::new(Tap::new(48000 * 4)),
            paused: AtomicBool::new(true),
            volume: AtomicU32::new(0.5f32.to_bits()),
            current_gain: Mutex::new(0.0),
            played: AtomicU64::new(0),
            latency: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            streaming: AtomicBool::new(false),
            sample_rate: AtomicU32::new(48000),
        }
    }

    fn queued_frames(&self) -> usize {
        self.ring.lock().map(|r| r.samples.len() / 2).unwrap_or(0)
    }

    /// Frame index (in `played` units) just after the last queued frame.
    fn end_frame(&self) -> u64 {
        self.ring
            .lock()
            .map(|r| r.next_frame + (r.samples.len() / 2) as u64)
            .unwrap_or(0)
    }

    fn reset(&self, base_frame: u64) {
        self.streaming.store(false, Ordering::Relaxed);
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
        if !stereo.is_empty() {
            self.streaming.store(true, Ordering::Relaxed);
        }
    }
}

impl AudioSource for Output {
    fn render(&self, out: &mut [f32], latency_frames: u32) {
        self.latency.store(latency_frames, Ordering::Relaxed);
        let target = if self.paused.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.volume.load(Ordering::Relaxed))
        };
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
            if !self.paused.load(Ordering::Relaxed) && self.streaming.load(Ordering::Relaxed) {
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
    /// Seek within the track with the given index (number of advances).
    Seek(f64, u64),
    /// Track to continue with after the current one (None cancels).
    Next(Option<SourceOpener>),
    Stop,
}

struct NativeTrack {
    commands: std::sync::mpsc::Sender<Command>,
    events: std::sync::mpsc::Receiver<ThreadEvent>,
    /// Advances that became audible (the thread's index once it catches up).
    index: u64,
    /// Next track already decoding, starting at output frame `.0`.
    advance: Option<(u64, TrackInfo)>,
}

#[derive(Clone, Copy)]
struct TrackInfo {
    duration: Option<f64>,
    sample_rate: u32,
    channels: u16,
    codec: &'static str,
}

impl TrackInfo {
    fn of(p: &Pipeline) -> TrackInfo {
        TrackInfo {
            duration: p.duration(),
            sample_rate: p.source_rate(),
            channels: p.source_channels(),
            codec: p.codec(),
        }
    }
}

enum ThreadEvent {
    Loaded {
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
        codec: &'static str,
    },
    /// The next track was opened; its first frame is output frame `at`.
    Advanced {
        at: u64,
        info: TrackInfo,
    },
    Seeked(f64),
    /// A seek for an earlier track arrived after the thread advanced.
    SeekStale(f64),
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
    /// Output frame at which the current track started (gapless advance).
    track_offset: u64,
    /// Incremented on every load (a new decoder thread).
    serial: u64,
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
            track_offset: 0,
            serial: 0,
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
                self.output
                    .sample_rate
                    .store(info.sample_rate, Ordering::Relaxed);
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
        self.output
            .volume
            .store(self.volume.to_bits(), Ordering::Relaxed);
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
            self.base_secs
                + played.saturating_sub(lat).saturating_sub(self.track_offset) as f64 / rate
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
        self.track_offset = 0;
        self.eof = false;
        self.pending_seek = None;
    }

    /// Identifies the loaded track's decoder (changes on every load).
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Hands the track to continue with seamlessly after the current one to
    /// the decoder (None cancels). Returns false when gapless playback is
    /// not available (push audio, nothing loaded).
    pub fn preload(&mut self, opener: Option<SourceOpener>) -> bool {
        match &self.native {
            Some(t) => t.commands.send(Command::Next(opener)).is_ok(),
            None => false,
        }
    }

    /// Loads a track; `opener` opens the byte source (on a background
    /// thread on native platforms).
    pub fn load(&mut self, host: &mut dyn Host, opener: SourceOpener, autoplay: bool) {
        self.stop(host);
        self.ensure_device(host);
        self.error = None;
        self.state = PlayState::Loading;
        self.serial += 1;
        let out_rate = self.device_rate;
        if host.has_threads() && !host.audio_is_push() {
            let (ctx, crx) = std::sync::mpsc::channel();
            let (etx, erx) = std::sync::mpsc::channel();
            let output = self.output.clone();
            output.reset(0);
            output.paused.store(!autoplay, Ordering::Relaxed);
            std::thread::Builder::new()
                .name("bl-decoder".into())
                .spawn(move || decoder_thread(opener, out_rate, output, crx, etx))
                .ok();
            self.native = Some(NativeTrack {
                commands: ctx,
                events: erx,
                index: 0,
                advance: None,
            });
        } else {
            // Push mode: open synchronously.
            match opener().and_then(|(src, ext)| {
                bl_media::open(src, ext.as_deref()).map_err(|e| e.to_string())
            }) {
                Ok(dec) => {
                    let pipeline = Pipeline::new(dec, out_rate);
                    self.duration = pipeline.duration();
                    self.events.push(PlayerEvent::Loaded {
                        duration: self.duration,
                        sample_rate: pipeline.source_rate(),
                        channels: pipeline.source_channels(),
                        codec: pipeline.codec(),
                    });
                    self.push = Some(PushTrack {
                        pipeline,
                        pushed: 0,
                        base_secs: 0.0,
                        eof_pushed: false,
                    });
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
        self.state = if autoplay {
            PlayState::Playing
        } else {
            PlayState::Paused
        };
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
            let _ = t.commands.send(Command::Seek(seconds, t.index));
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
        if let Some(t) = &mut self.native {
            let mut ended = false;
            for ev in t.events.try_iter() {
                match ev {
                    ThreadEvent::Loaded {
                        duration,
                        sample_rate,
                        channels,
                        codec,
                    } => {
                        self.duration = duration;
                        self.events.push(PlayerEvent::Loaded {
                            duration,
                            sample_rate,
                            channels,
                            codec,
                        });
                    }
                    ThreadEvent::Advanced { at, info } => {
                        t.advance = Some((at, info));
                        self.eof = false;
                    }
                    ThreadEvent::Seeked(actual) => {
                        self.base_secs = actual;
                        self.track_offset = 0;
                        self.pending_seek = None;
                    }
                    ThreadEvent::SeekStale(s) => {
                        self.pending_seek = None;
                        self.events.push(PlayerEvent::Reload(s));
                    }
                    ThreadEvent::Eof => self.eof = true,
                    ThreadEvent::Error(e) => {
                        self.error = Some(e.clone());
                        self.events.push(PlayerEvent::Error(e));
                        ended = true;
                    }
                }
            }
            if let Some((at, info)) = t.advance {
                let lat = self.output.latency.load(Ordering::Relaxed) as u64;
                if self
                    .output
                    .played
                    .load(Ordering::SeqCst)
                    .saturating_sub(lat)
                    >= at
                {
                    t.advance = None;
                    t.index += 1;
                    self.track_offset = at;
                    self.base_secs = 0.0;
                    self.duration = info.duration;
                    self.events.push(PlayerEvent::Advanced {
                        duration: info.duration,
                        sample_rate: info.sample_rate,
                        channels: info.channels,
                        codec: info.codec,
                    });
                }
            }
            if self.eof
                && self.state == PlayState::Playing
                && self.output.queued_frames() == 0
                && self.pending_seek.is_none()
            {
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

fn open_pipeline(opener: SourceOpener, out_rate: u32) -> Result<Pipeline, String> {
    let (src, ext) = opener()?;
    let dec = bl_media::open(src, ext.as_deref()).map_err(|e| e.to_string())?;
    Ok(Pipeline::new(dec, out_rate))
}

fn decoder_thread(
    opener: SourceOpener,
    out_rate: u32,
    output: Arc<Output>,
    commands: std::sync::mpsc::Receiver<Command>,
    events: std::sync::mpsc::Sender<ThreadEvent>,
) {
    let mut pipeline = match open_pipeline(opener, out_rate) {
        Ok(p) => p,
        Err(e) => {
            let _ = events.send(ThreadEvent::Error(e));
            return;
        }
    };
    let info = TrackInfo::of(&pipeline);
    let _ = events.send(ThreadEvent::Loaded {
        duration: info.duration,
        sample_rate: info.sample_rate,
        channels: info.channels,
        codec: info.codec,
    });
    let max_queue = out_rate as usize; // one second
    let mut buf = Vec::with_capacity(8192);
    let mut eof_sent = false;
    let mut next: Option<SourceOpener> = None;
    // Number of tracks advanced to (seeks name the track they are meant for).
    let mut index = 0u64;
    let mut wait = false;
    loop {
        // Commands (blocking briefly when there is nothing to decode).
        loop {
            let cmd = if wait {
                wait = false;
                match commands.recv_timeout(std::time::Duration::from_millis(15)) {
                    Ok(c) => c,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match commands.try_recv() {
                    Ok(c) => c,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            };
            match cmd {
                Command::Stop => return,
                Command::Next(o) => next = o,
                Command::Seek(s, i) if i == index => {
                    let actual = pipeline.seek(s).unwrap_or(s);
                    output.reset(0);
                    eof_sent = false;
                    let _ = events.send(ThreadEvent::Seeked(actual));
                }
                Command::Seek(s, _) => {
                    let _ = events.send(ThreadEvent::SeekStale(s));
                }
            }
        }
        if pipeline.eof {
            if let Some(o) = next.take() {
                // Continue with the next track right behind this one.
                match open_pipeline(o, out_rate) {
                    Ok(p) => {
                        pipeline = p;
                        index += 1;
                        eof_sent = false;
                        let _ = events.send(ThreadEvent::Advanced {
                            at: output.end_frame(),
                            info: TrackInfo::of(&pipeline),
                        });
                        continue;
                    }
                    // Ends normally; loading it again reports the error.
                    Err(_) => {}
                }
            }
            if !eof_sent {
                eof_sent = true;
                output.streaming.store(false, Ordering::Relaxed);
                let _ = events.send(ThreadEvent::Eof);
            }
            wait = true;
            continue;
        }
        if output.queued_frames() >= max_queue {
            wait = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-bit stereo WAV with a sine of `freq` Hz.
    fn wav(rate: u32, frames: usize, freq: f64) -> Vec<u8> {
        let mut d = Vec::new();
        let data_len = (frames * 4) as u32;
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&(36 + data_len).to_le_bytes());
        d.extend_from_slice(b"WAVEfmt ");
        d.extend_from_slice(&16u32.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&rate.to_le_bytes());
        d.extend_from_slice(&(rate * 4).to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&16u16.to_le_bytes());
        d.extend_from_slice(b"data");
        d.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let v =
                ((i as f64 * freq * std::f64::consts::TAU / rate as f64).sin() * 16000.0) as i16;
            d.extend_from_slice(&v.to_le_bytes());
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    #[test]
    fn resampled_output_is_aligned_and_complete() {
        let frames = 44100 * 2 + 123;
        let dec = bl_media::open(
            Box::new(std::io::Cursor::new(wav(44100, frames, 440.0))),
            Some("wav"),
        )
        .unwrap();
        let mut p = Pipeline::new(dec, 48000);
        let mut out = Vec::new();
        while p.next(&mut out).unwrap() {}
        // Exactly as long as the source (no filter delay, no lost tail).
        assert_eq!(out.len() / 2, frames * 48000 / 44100);
        // No delay: the output follows the source sine at 48 kHz.
        let max_err = (1000..out.len() / 2 - 1000)
            .map(|i| {
                (out[i * 2] as f64
                    - (i as f64 * 440.0 * std::f64::consts::TAU / 48000.0).sin() * 16000.0
                        / 32768.0)
                    .abs()
            })
            .fold(0.0, f64::max);
        assert!(max_err < 2e-3, "max error {max_err}");
    }
}
