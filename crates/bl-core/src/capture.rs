//! Other apps' playback (Android): the platform records what they play
//! (playback capture) and reports their media session. The audio drives
//! the visuals without being played a second time; play, pause, next,
//! previous and seek go to the other app, and its title, artist and cover
//! are shown like a track's.

use crate::core::Core;
use crate::host::Host;
use crate::library::LOCAL_PREFIX;
use crate::player::PlayState;
use bl_json::Value;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Track id prefix of the captured playback.
pub const ID_PREFIX: &str = "capture:";

/// Captured audio waiting for the player; more is dropped (oldest first)
/// so the visuals never lag behind what is heard.
const MAX_BUFFERED_SECS: f64 = 0.25;
const KEEP_SECS: f64 = 0.05;
const WAIT: Duration = Duration::from_millis(100);

// ---- captured audio (from the platform's recording thread) ---------------

#[derive(Default)]
struct Buffer {
    /// Interleaved stereo.
    samples: VecDeque<f32>,
    rate: u32,
    open: bool,
    generation: u64,
}

#[derive(Default)]
struct Shared {
    buf: Mutex<Buffer>,
    cv: Condvar,
}

fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(Shared::default)
}

fn lock() -> MutexGuard<'static, Buffer> {
    shared().buf.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hands recorded audio (interleaved 16-bit PCM) to the player; ignored
/// unless capture playback is running. Called from any thread.
pub fn push_pcm(pcm: &[i16], channels: u32, rate: u32) {
    if channels == 0 || !(8000..=192_000).contains(&rate) {
        return;
    }
    let mut b = lock();
    if !b.open {
        return;
    }
    if b.rate != rate {
        b.samples.clear();
        b.rate = rate;
    }
    let ch = channels as usize;
    for f in pcm.chunks_exact(ch) {
        let l = f[0] as f32 / 32768.0;
        let r = if ch > 1 { f[1] as f32 / 32768.0 } else { l };
        b.samples.push_back(l);
        b.samples.push_back(r);
    }
    let max = (rate as f64 * MAX_BUFFERED_SECS) as usize * 2;
    if b.samples.len() > max {
        let keep = (rate as f64 * KEEP_SECS) as usize * 2;
        let drop = b.samples.len() - keep;
        b.samples.drain(..drop);
    }
    drop(b);
    shared().cv.notify_all();
}

/// Starts a new capture stream (earlier decoders end).
fn open_buffer() -> u64 {
    let mut b = lock();
    b.samples.clear();
    b.open = true;
    b.generation += 1;
    b.generation
}

fn close_buffer() {
    let mut b = lock();
    b.samples.clear();
    b.open = false;
    b.generation += 1;
    drop(b);
    shared().cv.notify_all();
}

/// Reads the captured audio for the player.
struct CaptureDecoder {
    generation: u64,
    rate: u32,
}

impl CaptureDecoder {
    /// Waits briefly for the first audio to learn its rate.
    fn open(generation: u64) -> CaptureDecoder {
        let t0 = Instant::now();
        let mut b = lock();
        while b.rate == 0 && b.open && t0.elapsed() < Duration::from_secs(2) {
            b = match shared().cv.wait_timeout(b, WAIT) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        let rate = if b.rate == 0 { 48000 } else { b.rate };
        CaptureDecoder { generation, rate }
    }
}

impl bl_media::Decoder for CaptureDecoder {
    fn spec(&self) -> bl_media::AudioSpec {
        bl_media::AudioSpec {
            sample_rate: self.rate,
            channels: 2,
        }
    }

    fn total_frames(&self) -> Option<u64> {
        None
    }

    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, bl_media::Error> {
        let mut b = lock();
        // Closed, restarted or another rate: this stream ends (the core
        // opens a new one while capture still runs).
        if !b.open || b.generation != self.generation || (b.rate != 0 && b.rate != self.rate) {
            return Ok(false);
        }
        if b.samples.is_empty() {
            drop(match shared().cv.wait_timeout(b, WAIT) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            });
            return Ok(true);
        }
        out.extend(b.samples.drain(..));
        Ok(true)
    }

    fn seek(&mut self, _frame: u64) -> Result<u64, bl_media::Error> {
        Ok(0)
    }

    fn codec_name(&self) -> &'static str {
        "pcm"
    }
}

// ---- core side ------------------------------------------------------------------

/// The other app's media session.
#[derive(Clone, Debug, Default, PartialEq)]
struct Session {
    app: String,
    title: String,
    artist: String,
    album: String,
    /// Cover picture file (written by the platform).
    cover: String,
    duration: f64,
    position: f64,
    playing: bool,
    can_seek: bool,
}

#[derive(Default)]
pub(crate) struct CaptureState {
    /// The platform is asked to record (or records).
    requested: bool,
    running: bool,
    /// The captured audio is the current track.
    pub(crate) active: bool,
    /// The platform may read other apps' media sessions.
    access: bool,
    session: Option<Session>,
    /// Core time the session's position was reported.
    session_at: f64,
    serial: u64,
    cover_url: String,
    error: Option<String>,
}

impl CaptureState {
    pub(crate) fn cover_url(&self) -> &str {
        &self.cover_url
    }
}

impl Core {
    pub(crate) fn playing_capture(&self) -> bool {
        self.capture.active
            && self
                .current
                .as_ref()
                .is_some_and(|t| t.id.starts_with(ID_PREFIX))
    }

    /// Ends capture (the user plays something else).
    pub(crate) fn release_capture(&mut self, host: &mut dyn Host) {
        if !self.capture.requested && !self.capture.active {
            return;
        }
        self.capture.requested = false;
        host.capture_request("stop", "{}");
        self.end_capture_playback(host);
    }

    pub(crate) fn capture_command(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Option<Value> {
        let ok = || Value::object().with("ok", true);
        match name {
            "capture.start" => {
                if !host.has_capture() {
                    return Some(Value::object().with("error", "not available"));
                }
                self.release_airplay(host);
                self.end_dlna_playback(host);
                self.player.stop(host);
                self.capture.error = None;
                self.capture.requested = host.capture_request("start", "{}");
                Some(ok())
            }
            "capture.stop" => {
                self.release_capture(host);
                Some(ok())
            }
            "capture.sessionAccess" => {
                host.capture_request("access", "{}");
                Some(ok())
            }
            // From the platform: whether media sessions can be read.
            "capture.access" => {
                self.capture.access = c.get("granted").as_bool().unwrap_or(false);
                Some(ok())
            }
            // From the platform: recording started or stopped.
            "capture.state" => {
                let running = c.get("running").as_bool().unwrap_or(false);
                let error = c.str_or("error", "");
                if !error.is_empty() {
                    host.log(&format!("capture: {error}"));
                    self.capture.error = Some(error.to_string());
                    let msg = format!("{}: {error}", self.tr("Other apps"));
                    self.toast(msg, true);
                }
                self.capture.running = running;
                if running && !self.capture.active && self.capture.requested {
                    self.begin_capture_playback(host);
                } else if !running {
                    self.capture.requested = false;
                    if self.capture.active {
                        self.end_capture_playback(host);
                    }
                }
                Some(ok())
            }
            // From the platform: the other app's media session (null: none).
            "capture.session" => {
                self.on_capture_session(host, c.get("session"));
                Some(ok())
            }
            _ => None,
        }
    }

    /// Transport commands while other apps' playback is shown go to the
    /// other app; None for the rest.
    pub(crate) fn capture_transport(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Option<Value> {
        if !self.playing_capture() {
            return None;
        }
        let playing = self.capture.session.as_ref().is_none_or(|s| s.playing);
        let control = |host: &mut dyn Host, action: &str, position: f64| {
            let j = Value::object()
                .with("action", action)
                .with("position", position.max(0.0));
            host.capture_request("control", &j.to_json());
        };
        match name {
            "play" if c.get("ids").as_array().is_some_and(|a| !a.is_empty()) => return None,
            "play" => control(host, "play", 0.0),
            "pause" => control(host, "pause", 0.0),
            "toggle" => control(host, if playing { "pause" } else { "play" }, 0.0),
            "next" => control(host, "next", 0.0),
            "prev" => control(host, "prev", 0.0),
            "seek" => control(host, "seek", c.get("position").as_f64().unwrap_or(0.0)),
            "seekBy" => {
                let p = self.capture_position().unwrap_or(0.0);
                control(host, "seek", p + c.get("delta").as_f64().unwrap_or(0.0));
            }
            "stop" => self.release_capture(host),
            _ => return None,
        }
        Some(Value::object().with("ok", true))
    }

    fn begin_capture_playback(&mut self, host: &mut dyn Host) {
        let generation = open_buffer();
        self.capture.active = true;
        self.capture.serial += 1;
        let mut track = self.track_for("");
        track.id = format!("{ID_PREFIX}{}", self.capture.serial);
        self.current = Some(track);
        self.apply_capture_session();
        self.play_generation += 1;
        self.engine.reset_schedule();
        self.analysis = None;
        self.analysis_status = crate::core::AnalysisStatus::None;
        self.analysis_key.clear();
        self.revs.analysis += 1;
        self.live.reset();
        self.player.set_gain(Some(1.0), false);
        self.player.set_monitor(true);
        self.player.load_decoder(
            host,
            Box::new(move || {
                Ok(Box::new(CaptureDecoder::open(generation)) as Box<dyn bl_media::Decoder>)
            }),
            true,
        );
        self.revs.queue += 1;
        if self.settings.cover_fallback && self.bg_queue.is_empty() {
            let cover = self.capture.session.as_ref().map(|s| s.cover.clone());
            match cover.filter(|c| !c.is_empty()) {
                Some(path) => self.load_background(host, &format!("{LOCAL_PREFIX}{path}")),
                None => self.load_background(host, crate::core::DEFAULT_BG),
            }
        }
    }

    fn end_capture_playback(&mut self, host: &mut dyn Host) {
        close_buffer();
        self.capture.active = false;
        self.capture.running = false;
        self.player.set_monitor(false);
        if self
            .current
            .as_ref()
            .is_some_and(|t| t.id.starts_with(ID_PREFIX))
        {
            self.player.stop(host);
            self.current = None;
            self.revs.queue += 1;
            self.show_queue_background(host);
        }
    }

    /// The stream ended while still capturing (the rate changed): a new one.
    pub(crate) fn capture_stream_ended(&mut self, host: &mut dyn Host) {
        if self.capture.running {
            let generation = open_buffer();
            self.player.load_decoder(
                host,
                Box::new(move || {
                    Ok(Box::new(CaptureDecoder::open(generation)) as Box<dyn bl_media::Decoder>)
                }),
                true,
            );
        } else {
            self.end_capture_playback(host);
        }
    }

    fn on_capture_session(&mut self, host: &mut dyn Host, v: &Value) {
        let session = if v.is_null() {
            None
        } else {
            Some(Session {
                app: v.str_or("app", "").to_string(),
                title: v.str_or("title", "").to_string(),
                artist: v.str_or("artist", "").to_string(),
                album: v.str_or("album", "").to_string(),
                cover: v.str_or("cover", "").to_string(),
                duration: v.get("duration").as_f64().unwrap_or(0.0).max(0.0),
                position: v.get("position").as_f64().unwrap_or(0.0).max(0.0),
                playing: v.get("playing").as_bool().unwrap_or(false),
                can_seek: v.get("canSeek").as_bool().unwrap_or(false),
            })
        };
        let old_cover = self.capture.session.as_ref().map(|s| s.cover.clone());
        let new_cover = session.as_ref().map(|s| s.cover.clone());
        self.capture.session = session;
        self.capture.session_at = self.time;
        if old_cover != new_cover {
            self.capture.serial += 1;
            self.capture.cover_url = match new_cover.as_deref() {
                Some(p) if !p.is_empty() => {
                    // A new URL per picture, so image caches show the new one.
                    format!("{}#{}", crate::core::file_url(p), self.capture.serial)
                }
                _ => String::new(),
            };
            if self.playing_capture() && self.settings.cover_fallback && self.bg_queue.is_empty() {
                match new_cover.filter(|c| !c.is_empty()) {
                    Some(path) => self.load_background(host, &format!("{LOCAL_PREFIX}{path}")),
                    None => self.load_background(host, crate::core::DEFAULT_BG),
                }
            }
        }
        if self.playing_capture() {
            self.apply_capture_session();
            self.revs.queue += 1;
        }
    }

    /// Shows the session's title, artist and cover on the current track.
    fn apply_capture_session(&mut self) {
        let fallback = self.tr("Other apps").to_string();
        let cover = !self.capture.cover_url.is_empty();
        let s = self.capture.session.clone().unwrap_or_default();
        if let Some(t) = self
            .current
            .as_mut()
            .filter(|t| t.id.starts_with(ID_PREFIX))
        {
            t.title = if s.title.is_empty() {
                fallback
            } else {
                s.title.clone()
            };
            t.artists = match (s.artist.is_empty(), s.app.is_empty()) {
                (false, _) => vec![s.artist.clone()],
                (true, false) => vec![s.app.clone()],
                (true, true) => Vec::new(),
            };
            t.album = s.album.clone();
            t.duration_ms = (s.duration * 1000.0) as u64;
            t.has_cover = cover;
        }
    }

    /// The other app's position (as its session reports it).
    pub(crate) fn capture_position(&self) -> Option<f64> {
        if !self.playing_capture() {
            return None;
        }
        let s = self.capture.session.as_ref()?;
        let p = if s.playing {
            s.position + (self.time - self.capture.session_at).max(0.0)
        } else {
            s.position
        };
        Some(if s.duration > 0.0 {
            p.min(s.duration)
        } else {
            p
        })
    }

    /// The play state shown while capturing: the other app's.
    pub(crate) fn capture_play_state(&self) -> Option<PlayState> {
        if !self.playing_capture() {
            return None;
        }
        Some(match &self.capture.session {
            Some(s) if !s.playing => PlayState::Paused,
            _ => PlayState::Playing,
        })
    }

    pub(crate) fn capture_duration(&self) -> Option<f64> {
        self.capture
            .session
            .as_ref()
            .filter(|_| self.playing_capture())
            .map(|s| s.duration)
    }

    pub(crate) fn capture_json(&self, host: &dyn Host) -> Value {
        let s = self.capture.session.as_ref();
        Value::object()
            .with("available", host.has_capture())
            .with("requested", self.capture.requested)
            .with("running", self.capture.running)
            .with("active", self.playing_capture())
            .with("access", self.capture.access)
            .with("app", s.map(|s| s.app.clone()).unwrap_or_default())
            .with("canSeek", s.is_some_and(|s| s.can_seek))
            .with("error", self.capture.error.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_media::Decoder;

    #[test]
    fn buffer_drops_old_audio_and_ends_on_close() {
        let generation = open_buffer();
        push_pcm(&[1000, -1000, 2000, -2000], 2, 48000);
        let mut d = CaptureDecoder::open(generation);
        assert_eq!(d.spec().sample_rate, 48000);
        let mut out = Vec::new();
        assert!(d.next_chunk(&mut out).unwrap());
        assert_eq!(out.len(), 4);
        // A second of audio arrives at once: only the newest is kept.
        push_pcm(&vec![0i16; 48000 * 2], 2, 48000);
        out.clear();
        d.next_chunk(&mut out).unwrap();
        assert_eq!(out.len(), (48000.0 * KEEP_SECS) as usize * 2);
        // Mono becomes stereo.
        push_pcm(&[100, 200], 1, 48000);
        out.clear();
        d.next_chunk(&mut out).unwrap();
        assert_eq!(out.len(), 4);
        close_buffer();
        assert!(!d.next_chunk(&mut out).unwrap());
        // Closed: nothing is kept.
        push_pcm(&[1, 2], 2, 48000);
        assert!(lock().samples.is_empty());
    }
}
