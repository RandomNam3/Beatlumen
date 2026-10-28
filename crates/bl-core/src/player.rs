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

/// Opens a ready decoder (live streams such as AirPlay).
pub type DecoderOpener = Box<dyn FnOnce() -> Result<Box<dyn Decoder>, String> + Send>;

/// What the decoder thread opens.
enum Opener {
    Source(SourceOpener),
    Decoder(DecoderOpener),
}

impl Opener {
    fn open(self) -> Result<Box<dyn Decoder>, String> {
        match self {
            Opener::Source(o) => {
                let (src, ext) = o()?;
                bl_media::open(src, ext.as_deref()).map_err(|e| e.to_string())
            }
            Opener::Decoder(o) => o(),
        }
    }
}

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
            None => {
                out.extend_from_slice(&self.stereo);
                self.in_frames += (self.stereo.len() / 2) as u64;
                self.out_frames += (self.stereo.len() / 2) as u64;
            }
        }
        Ok(more || !self.decoded.is_empty())
    }

    /// Output frames left until the end of the track (None if the length
    /// is unknown).
    pub fn remaining_frames(&self) -> Option<u64> {
        let total =
            self.decoder.total_frames()? * self.out_rate as u64 / self.in_rate.max(1) as u64;
        Some(total.saturating_sub(self.out_frames))
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

/// Loudness normalization gain of the output.
struct Norm {
    /// Applied gain (follows `target` smoothly).
    gain: f32,
    target: f32,
    /// False while the current track's gain is being measured.
    known: bool,
    /// Until then, playback of a new track waits for its gain.
    hold_until: Option<std::time::Instant>,
    /// First output frame of the next (gapless) track and its gain.
    next_at: Option<u64>,
    next: Option<f32>,
}

impl Default for Norm {
    fn default() -> Norm {
        Norm {
            gain: 1.0,
            target: 1.0,
            known: true,
            hold_until: None,
            next_at: None,
            next: None,
        }
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
    norm: Mutex<Norm>,
    /// Output frame index (since track start, including the seek base) of
    /// the last frame handed to the device.
    played: AtomicU64,
    latency: AtomicU32,
    underruns: AtomicU64,
    /// True while a track is streaming (an empty ring is an underrun, not
    /// the start or the end of playback).
    streaming: AtomicBool,
    /// Crossfade length in milliseconds (0 = gapless) and whether silence
    /// at the end and start of tracks is skipped (read by the decoder).
    crossfade_ms: AtomicU32,
    skip_silence: AtomicBool,
    pub sample_rate: AtomicU32,
    /// A speaker in the network plays instead of the device (the device
    /// gets silence; the network output renders at full volume and sets the
    /// speaker's own volume).
    external: AtomicBool,
    /// Counts resets (seeks, new tracks): a network output drops what the
    /// speaker has buffered.
    resets: AtomicU64,
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
            norm: Mutex::new(Norm::default()),
            played: AtomicU64::new(0),
            latency: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            streaming: AtomicBool::new(false),
            crossfade_ms: AtomicU32::new(0),
            skip_silence: AtomicBool::new(false),
            sample_rate: AtomicU32::new(48000),
            external: AtomicBool::new(false),
            resets: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_external(&self, on: bool) {
        self.external.store(on, Ordering::Relaxed);
    }

    pub(crate) fn resets(&self) -> u64 {
        self.resets.load(Ordering::Relaxed)
    }

    /// Paused and faded out.
    pub(crate) fn is_silent(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
            && self.current_gain.lock().map(|g| *g <= 1e-4).unwrap_or(true)
    }

    /// Renders for a network output (see [`Output::set_external`]).
    pub(crate) fn render_external(&self, out: &mut [f32], latency_frames: u32) {
        self.render_frames(out, latency_frames);
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

    /// Removes up to `frames` frames from the end of the queue (those not
    /// played yet).
    fn trim_tail(&self, frames: usize) {
        if let Ok(mut r) = self.ring.lock() {
            let n = (frames * 2).min(r.samples.len());
            let keep = r.samples.len() - n;
            r.samples.truncate(keep);
        }
    }

    fn reset(&self, base_frame: u64) {
        self.resets.fetch_add(1, Ordering::Relaxed);
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
        if self.external.load(Ordering::Relaxed) {
            out.fill(0.0);
            return;
        }
        self.render_frames(out, latency_frames);
    }
}

impl Output {
    fn render_frames(&self, out: &mut [f32], latency_frames: u32) {
        self.latency.store(latency_frames, Ordering::Relaxed);
        let target = if self.paused.load(Ordering::Relaxed) {
            0.0
        } else if self.external.load(Ordering::Relaxed) {
            1.0
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
        let mut norm = self.norm.lock().unwrap_or_else(|e| e.into_inner());
        if !norm.known {
            if norm
                .hold_until
                .is_some_and(|t| std::time::Instant::now() < t)
            {
                // Wait (without consuming) until the new track's gain is known.
                out.fill(0.0);
                return;
            }
            // Measuring takes longer: keep the previous gain for now.
            norm.known = true;
            norm.hold_until = None;
        }
        let frames = out.len() / 2;
        let mut taken = 0usize;
        let mut first_frame = 0;
        if let Ok(mut r) = self.ring.lock() {
            first_frame = r.next_frame;
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
        // Smooth gain changes (pause/resume fades, volume changes); the
        // normalization gain follows within ~0.25 s and switches exactly at
        // the first frame of a gapless next track.
        let rate = self.sample_rate.load(Ordering::Relaxed).max(1) as f32;
        let step = 1.0 / (0.02 * rate);
        let follow = 1.0 / (0.25 * rate);
        for (i, f) in out[..taken * 2].chunks_exact_mut(2).enumerate() {
            if norm.next_at.is_some_and(|at| first_frame + i as u64 >= at) {
                norm.next_at = None;
                if let Some(g) = norm.next.take() {
                    norm.target = g;
                    norm.gain = g;
                }
            }
            norm.gain += (norm.target - norm.gain) * follow;
            if gain < target {
                gain = (gain + step).min(target);
            } else if gain > target {
                gain = (gain - step).max(target);
            }
            // Perceptual volume curve.
            let g = gain * gain * norm.gain;
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
    /// Next track already decoding: first output frame, info, start time.
    advance: Option<(u64, TrackInfo, f64)>,
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
    /// The next track was opened; its first frame is output frame `at` and
    /// it starts `start` seconds into the track (skipped silence).
    Advanced {
        at: u64,
        info: TrackInfo,
        start: f64,
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
    /// Normalization gain (push audio) and the gain last sent to the host.
    norm_gain: f32,
    sent_gain: f32,
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
            norm_gain: 1.0,
            sent_gain: -1.0,
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

    /// Sets the loudness normalization gain of the current track; None
    /// while it is being measured (with `hold`, a newly loaded track waits
    /// up to a second for it instead of starting at the previous gain).
    pub fn set_gain(&mut self, gain: Option<f32>, hold: bool) {
        if let Some(g) = gain {
            self.norm_gain = g;
        }
        if let Ok(mut n) = self.output.norm.lock() {
            match gain {
                Some(g) => {
                    n.target = g;
                    n.known = true;
                    n.hold_until = None;
                }
                None => {
                    n.known = false;
                    n.hold_until =
                        hold.then(|| std::time::Instant::now() + std::time::Duration::from_secs(1));
                }
            }
        }
    }

    /// Crossfade length between tracks (0 = gapless) and whether silence at
    /// the end and start of tracks is skipped when moving on.
    pub fn set_transition(&mut self, crossfade_secs: f32, skip_silence: bool) {
        let ms = (crossfade_secs.clamp(0.0, 30.0) * 1000.0) as u32;
        self.output.crossfade_ms.store(ms, Ordering::Relaxed);
        self.output
            .skip_silence
            .store(skip_silence, Ordering::Relaxed);
    }

    /// Normalization gain of the preloaded next track (None = unknown, the
    /// current gain is kept).
    pub fn set_next_gain(&mut self, gain: Option<f32>) {
        if let Ok(mut n) = self.output.norm.lock() {
            n.next = gain;
        }
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

    pub(crate) fn output(&self) -> Arc<Output> {
        self.output.clone()
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
        self.load_with(host, Opener::Source(opener), autoplay);
    }

    /// Plays a ready decoder (a live stream: no preloading of a next track).
    pub fn load_decoder(&mut self, host: &mut dyn Host, opener: DecoderOpener, autoplay: bool) {
        self.load_with(host, Opener::Decoder(opener), autoplay);
    }

    /// Delay between audio leaving the decoder and being heard (output
    /// latency reported by the device).
    pub fn output_latency(&self) -> f64 {
        self.output.latency.load(Ordering::Relaxed) as f64 / self.device_rate.max(1) as f64
    }

    fn load_with(&mut self, host: &mut dyn Host, opener: Opener, autoplay: bool) {
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
            if let Ok(mut n) = output.norm.lock() {
                n.next_at = None;
                n.next = None;
            }
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
            match opener.open() {
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
                    ThreadEvent::Advanced { at, info, start } => {
                        t.advance = Some((at, info, start));
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
            if let Some((at, info, start)) = t.advance {
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
                    self.base_secs = start;
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
        if self.push.is_some() {
            // Push audio: the host applies volume and normalization.
            let g = self.volume * self.volume * self.norm_gain;
            if g != self.sent_gain {
                self.sent_gain = g;
                host.audio_set_gain(g);
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

fn open_pipeline(opener: Opener, out_rate: u32) -> Result<Pipeline, String> {
    Ok(Pipeline::new(opener.open()?, out_rate))
}

/// Level below which audio counts as silence (-50 dBFS).
const SILENCE: f32 = 0.0032;
/// Silence of this length within the last `END_REGION` seconds ends a
/// track early (when skipping silence).
const SILENT_END: f64 = 0.75;
const END_REGION: f64 = 15.0;
/// Most silence skipped at the start of the next track.
const MAX_LEAD_SKIP: f64 = 10.0;

/// The previous track while it fades out under the next one.
struct FadeOut {
    a: Pipeline,
    buf: Vec<f32>,
    done: usize,
    len: usize,
    /// Level correction for the outgoing track (the output already applies
    /// the incoming track's normalization gain).
    ratio: f32,
}

impl FadeOut {
    /// Mixes the outgoing track into `b` (incoming, interleaved stereo)
    /// with an equal-power fade.
    fn mix(&mut self, b: &mut [f32]) {
        let n = b.len() / 2;
        let mut tmp = Vec::new();
        while self.buf.len() < n * 2 && !self.a.eof {
            tmp.clear();
            if self.a.next(&mut tmp).is_err() {
                self.a.eof = true;
            }
            self.buf.extend_from_slice(&tmp);
        }
        for i in 0..n {
            let k = self.done + i;
            if k >= self.len {
                break;
            }
            let x = (k as f32 + 0.5) / self.len as f32 * std::f32::consts::FRAC_PI_2;
            let (ga, gb) = (x.cos() * self.ratio, x.sin());
            for c in 0..2 {
                let a = self.buf.get(i * 2 + c).copied().unwrap_or(0.0);
                b[i * 2 + c] = b[i * 2 + c] * gb + a * ga;
            }
        }
        self.done += n;
        let used = (n * 2).min(self.buf.len());
        self.buf.drain(..used);
    }
}

/// Number of silent frames at the end of `stereo`.
fn trailing_silence(stereo: &[f32]) -> usize {
    stereo
        .chunks_exact(2)
        .rev()
        .take_while(|f| f[0].abs() < SILENCE && f[1].abs() < SILENCE)
        .count()
}

/// Decodes past leading silence (up to `max` frames); the first audible
/// samples are left in `out`. Returns the number of frames skipped.
fn skip_leading_silence(p: &mut Pipeline, out: &mut Vec<f32>, max: usize) -> usize {
    let mut skipped = 0;
    while !p.eof && skipped < max {
        out.clear();
        if p.next(out).is_err() {
            p.eof = true;
            break;
        }
        let first = out
            .chunks_exact(2)
            .position(|f| f[0].abs() >= SILENCE || f[1].abs() >= SILENCE);
        match first {
            Some(i) => {
                out.drain(..i * 2);
                return skipped + i;
            }
            None => skipped += out.len() / 2,
        }
    }
    out.clear();
    skipped
}

fn decoder_thread(
    opener: Opener,
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
    let rate = out_rate.max(1) as f64;
    let max_queue = out_rate as usize; // one second
    let mut buf = Vec::with_capacity(8192);
    let mut eof_sent = false;
    let mut next: Option<SourceOpener> = None;
    // Number of tracks advanced to (seeks name the track they are meant for).
    let mut index = 0u64;
    let mut wait = false;
    // Silent frames at the end of what was decoded so far.
    let mut silent_run = 0usize;
    let mut fade: Option<FadeOut> = None;
    // Samples of the current track decoded ahead (after skipped silence).
    let mut pending: Vec<f32> = Vec::new();
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
                    fade = None;
                    pending.clear();
                    silent_run = 0;
                    output.reset(0);
                    eof_sent = false;
                    let _ = events.send(ThreadEvent::Seeked(actual));
                }
                Command::Seek(s, _) => {
                    let _ = events.send(ThreadEvent::SeekStale(s));
                }
            }
        }
        // Move on to the next track: at the end, when the crossfade starts,
        // or when the track has gone silent near its end.
        if fade.is_none() && next.is_some() && pending.is_empty() {
            let crossfade =
                output.crossfade_ms.load(Ordering::Relaxed) as u64 * out_rate as u64 / 1000;
            let skip = output.skip_silence.load(Ordering::Relaxed);
            let remaining = pipeline.remaining_frames();
            let silent_end = skip
                && !pipeline.eof
                && silent_run as f64 >= SILENT_END * rate
                && remaining.is_some_and(|r| (r as f64) <= END_REGION * rate);
            let fade_now =
                crossfade > 0 && !pipeline.eof && remaining.is_some_and(|r| r <= crossfade);
            if pipeline.eof || silent_end || fade_now {
                let opener = next.take().expect("checked");
                // On failure the track ends normally; loading the next one
                // again reports the error.
                if let Ok(mut b) = open_pipeline(Opener::Source(opener), out_rate) {
                    if silent_end {
                        output.trim_tail(silent_run);
                    }
                    let skipped = if skip {
                        skip_leading_silence(&mut b, &mut pending, (MAX_LEAD_SKIP * rate) as usize)
                    } else {
                        0
                    };
                    let at = output.end_frame();
                    let ratio = output
                        .norm
                        .lock()
                        .ok()
                        .and_then(|n| n.next.filter(|g| *g > 0.0).map(|g| n.target / g))
                        .unwrap_or(1.0);
                    if let Ok(mut n) = output.norm.lock() {
                        n.next_at = Some(at);
                    }
                    let a = std::mem::replace(&mut pipeline, b);
                    if fade_now && !silent_end {
                        let len = remaining.unwrap_or(crossfade).min(crossfade) as usize;
                        fade = Some(FadeOut {
                            a,
                            buf: Vec::new(),
                            done: 0,
                            len,
                            ratio,
                        });
                    }
                    index += 1;
                    eof_sent = false;
                    silent_run = 0;
                    let _ = events.send(ThreadEvent::Advanced {
                        at,
                        info: TrackInfo::of(&pipeline),
                        start: skipped as f64 / rate,
                    });
                    continue;
                }
            }
        }
        if pipeline.eof && pending.is_empty() {
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
        if !pending.is_empty() {
            buf.append(&mut pending);
        } else if let Err(e) = pipeline.next(&mut buf) {
            let _ = events.send(ThreadEvent::Error(e));
            pipeline.eof = true;
        }
        if let Some(f) = &mut fade {
            f.mix(&mut buf);
            if f.done >= f.len {
                fade = None;
            }
        } else {
            let t = trailing_silence(&buf);
            silent_run = if t == buf.len() / 2 {
                silent_run + t
            } else {
                t
            };
        }
        output.push(&buf);
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
