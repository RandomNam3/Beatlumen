//! AirPlay receiver in the app: started and stopped with the setting,
//! announced in the local network, and while a sender streams it replaces
//! the current playback (title, artist, cover and progress come from the
//! sender; the visuals follow the live audio).

use crate::core::Core;
use crate::host::Host;
use crate::library::LOCAL_PREFIX;
use bl_airplay::{Event, Receiver};
use bl_json::Value;

/// Track id prefix of AirPlay streams.
pub(crate) const ID_PREFIX: &str = "airplay:";

#[derive(Default)]
pub(crate) struct AirplayState {
    receiver: Option<Receiver>,
    mdns: Option<bl_airplay::mdns::Responder>,
    registered: bool,
    /// Settings the running receiver was started with.
    started_with: Option<(String, String, String)>,
    /// A sender is streaming to us.
    pub(crate) active: bool,
    client: String,
    serial: u64,
    cover_url: String,
    /// Elapsed seconds reported by the sender and when (core time).
    progress: Option<(f64, f64)>,
    pub(crate) error: Option<String>,
}

impl AirplayState {
    pub(crate) fn cover_url(&self) -> &str {
        &self.cover_url
    }

    pub(crate) fn running(&self) -> bool {
        self.receiver.is_some()
    }

    pub(crate) fn client(&self) -> &str {
        &self.client
    }
}

impl Core {
    /// Name shown on AirPlay senders.
    fn airplay_name(&self) -> String {
        let n = self.settings.airplay_name.trim();
        if n.is_empty() {
            "Beatlumen".to_string()
        } else {
            n.to_string()
        }
    }

    /// Starts or stops the receiver to match the settings; called every
    /// tick, also delivers the receiver's events.
    pub(crate) fn update_airplay(&mut self, host: &mut dyn Host) {
        let wanted = self.settings.airplay && host.has_threads() && host.platform() != "web";
        let config = (
            self.airplay_name(),
            self.settings.airplay_password.clone(),
            self.settings.airplay_id.clone(),
        );
        if self.airplay.receiver.is_some()
            && (!wanted || self.airplay.started_with.as_ref() != Some(&config))
        {
            self.stop_airplay(host);
        }
        if wanted && self.airplay.receiver.is_none() && self.airplay.error.is_none() {
            self.start_airplay(host);
        }
        if !wanted {
            self.airplay.error = None;
        }
        let events = match &self.airplay.receiver {
            Some(r) => {
                r.stream().set_output_latency(self.player.output_latency());
                r.poll()
            }
            None => return,
        };
        for e in events {
            self.on_airplay_event(host, e);
        }
    }

    fn start_airplay(&mut self, host: &mut dyn Host) {
        if self.settings.airplay_id.len() != 12 {
            let id: String = (0..6)
                .map(|_| format!("{:02X}", (self.rng.next_u64() & 0xff) as u8))
                .collect();
            self.settings.airplay_id = id;
            self.settings_changed();
        }
        let id_bytes = &self.settings.airplay_id;
        let mut id = [0u8; 6];
        for (i, b) in id.iter_mut().enumerate() {
            *b = u8::from_str_radix(&id_bytes[i * 2..i * 2 + 2], 16).unwrap_or(0);
        }
        // Locally administered unicast address, as random MACs are.
        id[0] = (id[0] & 0xfc) | 0x02;
        let name = self.airplay_name();
        let password = Some(self.settings.airplay_password.clone()).filter(|p| !p.is_empty());
        let receiver = match Receiver::start(bl_airplay::Config {
            name: name.clone(),
            id,
            password,
        }) {
            Ok(r) => r,
            Err(e) => {
                host.log(&format!("airplay: {e}"));
                self.airplay.error = Some(e.to_string());
                return;
            }
        };
        let instance = receiver.instance_name();
        let txt = receiver.txt();
        self.airplay.registered =
            host.service_register(&instance, "_raop._tcp", receiver.port(), &txt);
        if !self.airplay.registered {
            let host_label = format!("beatlumen-{}", self.settings.airplay_id.to_lowercase());
            match bl_airplay::mdns::Responder::start(
                bl_airplay::mdns::Service {
                    instance,
                    service_type: "_raop._tcp".into(),
                    port: receiver.port(),
                    txt,
                },
                host_label,
            ) {
                Ok(m) => self.airplay.mdns = Some(m),
                Err(e) => {
                    host.log(&format!("airplay: mDNS: {e}"));
                    self.airplay.error = Some(format!("mDNS: {e}"));
                }
            }
        }
        host.log(&format!(
            "airplay: receiver \"{}\" on port {}",
            receiver.instance_name(),
            receiver.port()
        ));
        self.airplay.started_with = Some((
            name,
            self.settings.airplay_password.clone(),
            self.settings.airplay_id.clone(),
        ));
        self.airplay.receiver = Some(receiver);
    }

    fn stop_airplay(&mut self, host: &mut dyn Host) {
        if self.airplay.active {
            self.end_airplay_playback(host);
        }
        if self.airplay.registered {
            host.service_unregister();
            self.airplay.registered = false;
        }
        self.airplay.mdns = None;
        self.airplay.receiver = None;
        self.airplay.started_with = None;
    }

    /// Ends the AirPlay stream (the user plays something else or stops):
    /// the sender sees the speaker disconnect.
    pub(crate) fn release_airplay(&mut self, host: &mut dyn Host) {
        if !self.airplay.active {
            return;
        }
        if let Some(r) = &self.airplay.receiver {
            r.disconnect();
        }
        self.end_airplay_playback(host);
    }

    fn end_airplay_playback(&mut self, host: &mut dyn Host) {
        self.airplay.active = false;
        self.airplay.progress = None;
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

    fn on_airplay_event(&mut self, host: &mut dyn Host, e: Event) {
        match e {
            Event::Started { client } => {
                let Some(stream) = self.airplay.receiver.as_ref().map(|r| r.stream().clone())
                else {
                    return;
                };
                self.airplay.active = true;
                self.airplay.client = client.clone();
                self.airplay.serial += 1;
                self.airplay.cover_url.clear();
                self.airplay.progress = None;
                let mut track = self.track_for("");
                track.id = format!("{ID_PREFIX}{}", self.airplay.serial);
                track.title = self.tr("AirPlay").to_string();
                track.artists = vec![client.clone()];
                track.album.clear();
                track.has_cover = false;
                self.current = Some(track);
                self.play_generation += 1;
                self.engine.reset_schedule();
                self.analysis = None;
                self.analysis_status = crate::core::AnalysisStatus::None;
                self.analysis_key.clear();
                self.revs.analysis += 1;
                self.live.reset();
                self.player.set_gain(None, true);
                self.player.load_decoder(
                    host,
                    Box::new(move || Ok(Box::new(stream.decoder()) as Box<dyn bl_media::Decoder>)),
                    true,
                );
                self.revs.queue += 1;
                let msg = format!("{} {client}", self.tr("AirPlay from"));
                self.toast(msg, false);
                if self.settings.cover_fallback && self.bg_queue.is_empty() {
                    self.load_background(host, crate::core::DEFAULT_BG);
                }
            }
            Event::Ended => {
                if self.airplay.active {
                    self.end_airplay_playback(host);
                }
            }
            Event::Track(info) => {
                if let Some(t) = self
                    .current
                    .as_mut()
                    .filter(|t| t.id.starts_with(ID_PREFIX))
                {
                    if !info.title.is_empty() {
                        t.title = info.title;
                    }
                    t.artists = if info.artist.is_empty() {
                        vec![self.airplay.client.clone()]
                    } else {
                        vec![info.artist]
                    };
                    t.album = info.album;
                }
            }
            Event::Artwork { mime, data } => self.airplay_artwork(host, &mime, data),
            Event::Progress { elapsed, duration } => {
                self.airplay.progress = Some((elapsed, self.time));
                if let Some(t) = self
                    .current
                    .as_mut()
                    .filter(|t| t.id.starts_with(ID_PREFIX))
                {
                    t.duration_ms = (duration.max(0.0) * 1000.0) as u64;
                }
            }
            Event::Volume(_) => {}
        }
    }

    fn airplay_artwork(&mut self, host: &mut dyn Host, mime: &str, data: Vec<u8>) {
        let ext = match mime {
            m if m.contains("png") => "png",
            m if m.contains("jpeg") || m.contains("jpg") => "jpg",
            _ => {
                // "image/none": the track has no cover.
                self.airplay.cover_url.clear();
                return;
            }
        };
        if data.is_empty() || data.len() > bl_airplay::rtsp::MAX_BODY {
            return;
        }
        self.airplay.serial += 1;
        let key = format!("airplay/cover.{ext}");
        if !host.storage_write(&key, &data) {
            return;
        }
        let Some(path) = host.storage_path(&key) else {
            return;
        };
        // A new URL per picture, so image caches show the new one.
        self.airplay.cover_url =
            format!("{}#{}", crate::core::file_url(&path), self.airplay.serial);
        if let Some(t) = self
            .current
            .as_mut()
            .filter(|t| t.id.starts_with(ID_PREFIX))
        {
            t.has_cover = true;
        }
        if self.settings.cover_fallback && self.bg_queue.is_empty() {
            self.load_background(host, &format!("{LOCAL_PREFIX}{path}"));
        }
    }

    /// Playback position of the AirPlay stream (as the sender reports it).
    pub(crate) fn airplay_position(&self) -> Option<f64> {
        if !self.airplay.active {
            return None;
        }
        let (elapsed, at) = self.airplay.progress?;
        let receiving = self
            .airplay
            .receiver
            .as_ref()
            .is_some_and(|r| r.stream().receiving());
        Some(if receiving {
            elapsed + (self.time - at).max(0.0)
        } else {
            elapsed
        })
    }

    /// The announced name of our own receiver (not offered as a speaker).
    pub(crate) fn airplay_instance(&self) -> Option<String> {
        self.airplay.receiver.as_ref().map(|r| r.instance_name())
    }

    pub(crate) fn airplay_json(&self) -> Value {
        Value::object()
            .with("running", self.airplay.running())
            .with("active", self.airplay.active)
            .with("client", self.airplay.client())
            .with("name", self.airplay_name())
            .with("error", self.airplay.error.clone())
    }
}
