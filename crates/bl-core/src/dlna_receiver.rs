//! The app as a DLNA/UPnP speaker ("media renderer"): control points in the
//! network (Windows "Play to", BubbleUPnP, TVs, …) send it a stream URL and
//! control playback; it plays and visualises the stream like a track,
//! with the title, artist and cover the control point sends.

use crate::core::Core;
use crate::host::Host;
use crate::player::{PlayState, SourceOpener};
use bl_json::Value;
use bl_upnp::renderer::{Client, Command, Config, Meta, Renderer, State, Status};

/// Track id prefix of streams sent by control points.
pub(crate) const ID_PREFIX: &str = "dlna:";

/// What the decoders read (advertised to control points).
const MIMES: &[&str] = &[
    "audio/mpeg",
    "audio/mp3",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/vorbis",
    "audio/opus",
    "audio/wav",
    "audio/x-wav",
    "audio/wave",
    "audio/aiff",
    "audio/x-aiff",
    "audio/x-matroska",
    "audio/webm",
    "audio/alac",
];

#[derive(Default)]
pub(crate) struct DlnaState {
    renderer: Option<Renderer>,
    started_with: Option<(String, String)>,
    /// A control point's stream is the current track.
    pub(crate) active: bool,
    serial: u64,
    uri: String,
    meta: Meta,
    cover_url: String,
    pub(crate) error: Option<String>,
    /// A new control point waiting for the user's answer, with what it
    /// sent meanwhile.
    asking: Option<(Client, Vec<Command>)>,
    /// Control points allowed or refused until the app quits.
    allowed_now: Vec<std::net::IpAddr>,
    refused_now: Vec<std::net::IpAddr>,
}

impl DlnaState {
    pub(crate) fn cover_url(&self) -> &str {
        &self.cover_url
    }
}

/// The decoder's hint for a stream: from its MIME type, else the URL.
fn extension_for(mime: &str, uri: &str) -> Option<String> {
    let m = mime.to_ascii_lowercase();
    let ext = match () {
        _ if m.contains("mpeg") || m.contains("mp3") => "mp3",
        _ if m.contains("mp4") || m.contains("m4a") || m.contains("aac") || m.contains("alac") => {
            "m4a"
        }
        _ if m.contains("flac") => "flac",
        _ if m.contains("opus") => "opus",
        _ if m.contains("ogg") || m.contains("vorbis") => "ogg",
        _ if m.contains("wav") => "wav",
        _ if m.contains("aiff") => "aiff",
        _ if m.contains("matroska") => "mka",
        _ if m.contains("webm") => "webm",
        _ => return crate::jobs::extension(uri.split(['?', '#']).next().unwrap_or(uri)),
    };
    Some(ext.to_string())
}

impl Core {
    /// Name shown to control points (the same as for AirPlay).
    fn dlna_name(&self) -> String {
        let n = self.settings.airplay_name.trim();
        if n.is_empty() {
            "Beatlumen".to_string()
        } else {
            n.to_string()
        }
    }

    pub(crate) fn playing_dlna(&self) -> bool {
        self.dlna.active
            && self
                .current
                .as_ref()
                .is_some_and(|t| t.id.starts_with(ID_PREFIX))
    }

    /// Starts or stops the renderer to match the settings, handles the
    /// control points' commands and reports the playback (every tick).
    pub(crate) fn update_dlna(&mut self, host: &mut dyn Host) {
        let wanted = self.settings.dlna_receiver && host.has_threads() && host.platform() != "web";
        if wanted && self.settings.dlna_uuid.is_empty() {
            let b: Vec<u8> = (0..16)
                .map(|_| (self.rng.next_u64() & 0xff) as u8)
                .collect();
            let x: String = b.iter().map(|v| format!("{v:02x}")).collect();
            self.settings.dlna_uuid = format!(
                "uuid:{}-{}-{}-{}-{}",
                &x[..8],
                &x[8..12],
                &x[12..16],
                &x[16..20],
                &x[20..]
            );
            self.settings_changed();
        }
        let config = (self.dlna_name(), self.settings.dlna_uuid.clone());
        if self.dlna.renderer.is_some()
            && (!wanted || self.dlna.started_with.as_ref() != Some(&config))
        {
            self.dlna.renderer = None;
            self.dlna.started_with = None;
            if self.playing_dlna() {
                self.end_dlna(host);
            }
        }
        if !wanted {
            self.dlna.error = None;
            return;
        }
        if self.dlna.renderer.is_none() && self.dlna.error.is_none() {
            let sinks = MIMES.iter().map(|m| format!("http-get:*:{m}:*")).collect();
            match Renderer::start(Config {
                name: config.0.clone(),
                udn: config.1.clone(),
                sink_protocols: sinks,
            }) {
                Ok(r) => {
                    host.log(&format!(
                        "dlna: renderer \"{}\" on port {}",
                        config.0,
                        r.port()
                    ));
                    self.dlna.renderer = Some(r);
                    self.dlna.started_with = Some(config);
                }
                Err(e) => {
                    host.log(&format!("dlna: {e}"));
                    self.dlna.error = Some(e.to_string());
                }
            }
        }
        // Another track was started here: the control point's stream ended.
        if self.dlna.active
            && !self
                .current
                .as_ref()
                .is_some_and(|t| t.id.starts_with(ID_PREFIX))
        {
            self.dlna.active = false;
        }
        let commands = match &self.dlna.renderer {
            Some(r) => r.poll_from(),
            None => return,
        };
        for (c, client) in commands {
            self.dlna_from(host, c, client);
        }
        let state = if self.playing_dlna() {
            match self.player.state() {
                PlayState::Playing => State::Playing,
                PlayState::Paused => State::Paused,
                PlayState::Loading => State::Transitioning,
                PlayState::Stopped => State::Stopped,
            }
        } else if self.dlna.uri.is_empty() {
            State::NoMedia
        } else {
            State::Stopped
        };
        let playing = self.playing_dlna();
        let status = Status {
            state,
            position: if playing {
                self.player.position(host)
            } else {
                0.0
            },
            duration: if playing {
                self.player.duration().unwrap_or(0.0)
            } else {
                self.dlna.meta.duration.unwrap_or(0.0)
            },
            volume: (self.settings.volume * 100.0).round() as u32,
            mute: self.settings.muted,
        };
        if let Some(r) = &self.dlna.renderer {
            r.set_status(status);
        }
    }

    /// A command from a control point: played when the control point is
    /// known, held while the user is asked about a new one.
    fn dlna_from(&mut self, host: &mut dyn Host, c: Command, client: Client) {
        let ip = client.ip;
        let known = ip.is_loopback()
            || self.dlna.allowed_now.contains(&ip)
            || self.settings.dlna_allowed.contains(&ip.to_string());
        if known {
            return self.on_dlna_command(host, c);
        }
        if self.dlna.refused_now.contains(&ip) {
            return;
        }
        match &mut self.dlna.asking {
            Some((asked, held)) if asked.ip == ip => {
                if held.len() < 32 {
                    held.push(c);
                }
            }
            // One question at a time; others wait for their next try.
            Some(_) => {}
            None => {
                host.log(&format!("dlna: new control point {ip} ({})", client.agent));
                self.dlna.asking = Some((client, vec![c]));
                self.revs.devices += 1;
            }
        }
    }

    /// The user's answer about a new control point.
    pub(crate) fn dlna_answer(&mut self, host: &mut dyn Host, allow: bool, always: bool) {
        let Some((client, held)) = self.dlna.asking.take() else {
            return;
        };
        self.revs.devices += 1;
        if !allow {
            self.dlna.refused_now.push(client.ip);
            return;
        }
        if always {
            let ip = client.ip.to_string();
            if !self.settings.dlna_allowed.contains(&ip) {
                self.settings.dlna_allowed.push(ip);
                self.settings_changed();
            }
        } else {
            self.dlna.allowed_now.push(client.ip);
        }
        for c in held {
            self.on_dlna_command(host, c);
        }
    }

    fn on_dlna_command(&mut self, host: &mut dyn Host, c: Command) {
        match c {
            Command::SetUri {
                uri,
                meta,
                next: false,
            } => {
                let was_playing = self.playing_dlna() && self.player.state() != PlayState::Paused;
                self.dlna.uri = uri;
                self.dlna.meta = meta;
                if was_playing {
                    self.start_dlna(host, true);
                }
            }
            Command::SetUri { next: true, .. } => {}
            Command::Play => {
                if self.playing_dlna() && self.player.state() == PlayState::Paused {
                    self.player.play(host);
                } else if self.playing_dlna() && self.player.state() != PlayState::Stopped {
                    // Already playing.
                } else if !self.dlna.uri.is_empty() {
                    self.start_dlna(host, true);
                }
            }
            Command::Pause => {
                if self.playing_dlna() {
                    self.player.pause(host);
                }
            }
            Command::Stop => {
                if self.playing_dlna() {
                    self.player.stop(host);
                }
            }
            Command::Seek(t) => {
                if self.playing_dlna() {
                    self.player.seek(host, t);
                }
            }
            Command::Volume(v) => self.set_volume(v as f32 / 100.0),
            Command::Mute(m) => self.set_muted(m),
        }
    }

    /// Whether the current stream may be analysed ahead (the job checks
    /// that it is a file of moderate size).
    pub(crate) fn dlna_analysable(&self, host: &dyn Host) -> bool {
        host.has_threads() && !self.dlna.meta.duration.is_some_and(|d| d > 60.0 * 60.0)
    }

    pub(crate) fn dlna_opener(&self) -> Option<SourceOpener> {
        let uri = self.dlna.uri.clone();
        let ext = extension_for(&self.dlna.meta.mime, &uri);
        Some(Box::new(move || {
            Ok((crate::core::http_stream(&uri, &[])?, ext))
        }))
    }

    /// Plays the control point's stream as the current track.
    fn start_dlna(&mut self, host: &mut dyn Host, autoplay: bool) {
        self.release_airplay(host);
        self.release_capture(host);
        self.dlna.serial += 1;
        self.dlna.active = true;
        let meta = self.dlna.meta.clone();
        self.dlna.cover_url = meta.art_url.clone();
        let mut track = self.track_for("");
        track.id = format!("{ID_PREFIX}{}", self.dlna.serial);
        track.title = if meta.title.is_empty() {
            self.dlna
                .uri
                .rsplit('/')
                .next()
                .unwrap_or("DLNA")
                .to_string()
        } else {
            meta.title.clone()
        };
        track.artists = if meta.artist.is_empty() {
            Vec::new()
        } else {
            vec![meta.artist.clone()]
        };
        track.album = meta.album.clone();
        track.duration_ms = (meta.duration.unwrap_or(0.0) * 1000.0) as u64;
        track.has_cover = !meta.art_url.is_empty();
        self.current = Some(track);
        self.play_generation += 1;
        self.engine.reset_schedule();
        self.analysis = None;
        self.analysis_status = crate::core::AnalysisStatus::None;
        self.analysis_key.clear();
        self.revs.analysis += 1;
        self.live.reset();
        self.player.set_gain(Some(1.0), false);
        if let Some(opener) = self.dlna_opener() {
            self.player.load(host, opener, autoplay);
        }
        // Read ahead and analysed when it is a file (see the analysis).
        self.request_analysis_for_current();
        self.revs.queue += 1;
        if self.settings.cover_fallback && self.bg_queue.is_empty() {
            self.show_queue_background(host);
        }
    }

    /// The stream ended: the next one the control point set, if any.
    pub(crate) fn dlna_track_ended(&mut self, host: &mut dyn Host) {
        let next = self.dlna.renderer.as_ref().and_then(|r| r.advance());
        match next {
            Some((uri, meta)) => {
                self.dlna.uri = uri;
                self.dlna.meta = meta;
                self.start_dlna(host, true);
            }
            None => self.player.stop(host),
        }
    }

    /// Ends a control point's stream (the user plays something else).
    pub(crate) fn end_dlna_playback(&mut self, host: &mut dyn Host) {
        if self.playing_dlna() {
            self.end_dlna(host);
        }
    }

    fn end_dlna(&mut self, host: &mut dyn Host) {
        self.dlna.active = false;
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

    pub(crate) fn dlna_json(&self) -> Value {
        Value::object()
            .with("running", self.dlna.renderer.is_some())
            .with("active", self.playing_dlna())
            .with("port", self.dlna.renderer.as_ref().map(|r| r.port() as u64))
            .with("name", self.dlna_name())
            .with("error", self.dlna.error.clone())
            .with(
                "asking",
                self.dlna.asking.as_ref().map(|(c, _)| {
                    Value::object()
                        .with("ip", c.ip.to_string())
                        .with("agent", c.agent.as_str())
                }),
            )
            .with("allowed", self.settings.dlna_allowed.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(
            extension_for("audio/flac", "http://x/a").as_deref(),
            Some("flac")
        );
        assert_eq!(
            extension_for("audio/mpeg", "http://x/a").as_deref(),
            Some("mp3")
        );
        assert_eq!(
            extension_for("", "http://x/a.ogg?t=1").as_deref(),
            Some("ogg")
        );
    }
}
