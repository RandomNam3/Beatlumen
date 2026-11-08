//! Karaoke data of a song (`bl_pitch::KaraokeTrack`): loaded from the
//! server, from the song's offline copy or from this device, computed on
//! the server (when it does that) or on the device (`bl-karaoke`: the
//! vocals separated, their melody as notes on the timed song text), and
//! imported or exported as UltraStar text files.
//!
//! Where it is kept:
//! - server songs: on the server (`/api/tracks/<id>/karaoke`), copied into
//!   the offline container when the song is kept offline; computed on the
//!   device when the server does not compute, then kept on this device;
//! - the device's own songs: in the app's storage (`karaoke/<hash>.json`).
//!
//! Computed data names the song text it was made from and is dropped when
//! the text changes; imported data stays.

use crate::core::{Core, Req};
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::library;
use crate::server::ServerState;
use bl_json::Value;
use bl_pitch::stored::{self, Source};
use bl_pitch::KaraokeTrack;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Seconds between status requests of a computation on the server.
const POLL_SECS: f64 = 2.0;
/// Largest karaoke file imported (bytes).
const MAX_IMPORT: usize = 1 << 20;

/// What a server request was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KaraokeReq {
    Get(String),
    Compute(String),
    Saved,
}

/// The karaoke data found for a song.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub track: KaraokeTrack,
    /// "computed" or "imported".
    pub source: String,
    /// Where it is kept: "server", "offline" or "device".
    pub place: &'static str,
}

/// A computation under way.
#[derive(Debug, Clone)]
pub(crate) struct Compute {
    pub id: String,
    /// "server" or "device".
    pub place: &'static str,
    /// "queued", "downloading" (the separation model), "running".
    pub state: String,
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
    pub next_poll: f64,
    pub asking: bool,
    /// The song text it starts from (device).
    pub lyrics: String,
}

#[derive(Default)]
pub(crate) struct KaraokeState {
    /// Track the data is for.
    pub id: String,
    pub loading: bool,
    pub found: Option<Found>,
    pub compute: Option<Compute>,
    pub error: Option<String>,
    /// The next picked file is karaoke data for the current track.
    pub expect_import: bool,
    /// Data read on a worker thread: (track id, stored JSON, place).
    loaded: Mailbox<(String, Option<(Value, &'static str)>)>,
    /// Results of computations on the device: (track id, stored JSON).
    computed: Mailbox<(String, Result<Value, String>)>,
    #[cfg(not(target_arch = "wasm32"))]
    models: Option<Arc<bl_asr::models::Models>>,
    /// The separation model's download finished.
    #[cfg(not(target_arch = "wasm32"))]
    model_ready: Mailbox<()>,
    pub rev: u64,
    /// How loud the singing plays (1: as recorded), the separation of the
    /// current track for that, and its result (track id, load, output rate,
    /// stereo samples).
    pub vocals_level: Option<f32>,
    vocals_job: Option<VocalsJob>,
    vocals_ready: Mailbox<(String, u64, Result<(u32, Vec<i16>), String>)>,
    /// The last track's separated singing (id, rate), kept for when it is
    /// loaded again (after the calibration's clicks, say).
    vocals_kept: Option<(String, u32, Arc<Vec<i16>>)>,
}

/// Separating the singing of the track that plays, to turn it down.
#[derive(Debug, Clone)]
struct VocalsJob {
    id: String,
    serial: u64,
    /// "downloading" (the model) or "running".
    state: String,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
}

/// Storage key of the karaoke data of a song on this device.
pub(crate) fn storage_key(id: &str) -> String {
    format!("karaoke/{:016x}.json", library::fnv(id))
}

/// A file name from a song's artist and title.
fn file_name(artist: &str, title: &str) -> String {
    let name = match (artist.trim(), title.trim()) {
        ("", "") => "Karaoke".to_string(),
        ("", t) => t.to_string(),
        (a, "") => a.to_string(),
        (a, t) => format!("{a} - {t}"),
    };
    name.chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

impl Core {
    fn karaoke_server_can(&self) -> bool {
        matches!(&self.server.state, ServerState::Connected(i) if i.karaoke)
    }

    /// The song text karaoke data of `id` is computed from: the loaded
    /// song text with its times, when it is this song's.
    fn karaoke_lyrics(&self, id: &str) -> Option<String> {
        let (l, _) = self
            .lyrics
            .found
            .as_ref()
            .filter(|_| self.lyrics.id == id)?;
        l.synced.then(|| l.to_enhanced_lrc())
    }

    /// Whether this device can compute karaoke data.
    fn karaoke_device_can(&self, host: &dyn Host) -> bool {
        cfg!(not(target_arch = "wasm32")) && host.has_threads() && host.platform() != "web"
    }

    /// `karaoke.*` commands.
    pub(crate) fn karaoke_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        match name {
            "karaoke.start" => return self.karaoke_start(host, c),
            "karaoke.live" => return self.karaoke_live(host, c),
            "karaoke.results" => return self.karaoke_results(),
            "karaoke.stop" => return self.karaoke_stop(),
            "karaoke.calibrate" => return self.karaoke_calibrate(host, c),
            "karaoke.calibration" => return self.karaoke_calibration_json(),
            "karaoke.send" | "karaoke.sendStop" | "karaoke.receive" | "karaoke.sending" => {
                return self.karaoke_phone_command(host, name, c)
            }
            _ => {}
        }
        let id = match c.get("id").as_str() {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => match self.queue.current() {
                Some(id) => id.clone(),
                None => return Value::object().with("status", "none"),
            },
        };
        match name {
            "karaoke.get" => self.karaoke_get(host, &id),
            "karaoke.compute" => self.karaoke_compute(host, &id),
            "karaoke.cancel" => {
                if let Some(j) = self.karaoke.compute.take() {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                self.karaoke.rev += 1;
                Value::object().with("ok", true)
            }
            "karaoke.import" => self.karaoke_import(host, &id, c.str_or("text", "").as_bytes()),
            "karaoke.importFile" => match host.fs_read(c.str_or("path", "")) {
                Ok(d) => {
                    let r = self.karaoke_import(host, &id, &d);
                    match r.get("error").as_str() {
                        Some(e) => self.toast(e.to_string(), true),
                        None => self.toast(self.tr("Karaoke data added"), false),
                    }
                    r
                }
                Err(e) => Value::object().with("error", e),
            },
            "karaoke.expectImport" => {
                self.karaoke.expect_import = true;
                Value::object().with("ok", true)
            }
            "karaoke.export" => self.karaoke_export(&id, c.str_or("folder", "")),
            "karaoke.ultrastar" => match &self.karaoke.found {
                Some(f) if self.karaoke.id == id => Value::object()
                    .with("text", bl_pitch::ultrastar::to_ultrastar(&f.track))
                    .with(
                        "name",
                        format!("{}.txt", file_name(&f.track.artist, &f.track.title)),
                    ),
                _ => Value::object().with("error", self.tr("This song has no karaoke data")),
            },
            "karaoke.remove" => self.karaoke_remove(host, &id),
            "karaoke.vocals" => self.karaoke_vocals(host, c.f64_or("level", 1.0) as f32),
            _ => Value::object().with("error", "unknown command"),
        }
    }

    /// `karaoke.get`: the song's karaoke data; starts loading it.
    fn karaoke_get(&mut self, host: &mut dyn Host, id: &str) -> Value {
        if self.karaoke.id != id {
            self.karaoke_load(host, id);
        }
        let k = &self.karaoke;
        let mut v = Value::object().with("id", id);
        v = match (&k.found, k.loading) {
            (_, true) => v.with("status", "loading"),
            (None, false) => v.with("status", "none"),
            (Some(f), false) => v
                .with("status", "ready")
                .with("source", f.source.as_str())
                .with("place", f.place)
                .with("track", f.track.to_json())
                .with("duet", f.track.is_duet())
                .with("key", f.track.key.map(|k| k.name()))
                .with(
                    "range",
                    f.track.range().map(|(lo, hi)| {
                        Value::from(vec![Value::from(lo as i64), Value::from(hi as i64)])
                    }),
                ),
        };
        let lyrics_ready = self.karaoke_lyrics(id).is_some();
        let can_compute = !k.loading
            && k.compute.is_none()
            && lyrics_ready
            && ((library::is_server_id(id) && self.karaoke_server_can())
                || self.karaoke_device_can(host));
        let computing = k.compute.as_ref().filter(|j| j.id == id).map(|j| {
            Value::object()
                .with("state", j.state.as_str())
                .with("place", j.place)
                .with(
                    "progress",
                    j.progress.load(Ordering::Relaxed) as f64 / 1000.0,
                )
        });
        let vocals = {
            let level = self.karaoke_vocals_level();
            let on = self
                .player
                .vocal_cut()
                .is_some_and(|c| c.serial == self.player.serial());
            let job = self.karaoke.vocals_job.as_ref();
            Value::object()
                .with("level", level as f64)
                .with(
                    "state",
                    match job {
                        Some(j) => j.state.as_str(),
                        None if on => "on",
                        None => "original",
                    },
                )
                .with(
                    "progress",
                    job.map_or(0.0, |j| j.progress.load(Ordering::Relaxed) as f64 / 1000.0),
                )
        };
        v.with("canCompute", can_compute)
            .with("vocals", vocals)
            .with("lyricsReady", lyrics_ready)
            .with("canRemove", k.found.is_some() && !k.loading)
            .with("computing", computing)
            .with("error", k.error.clone())
    }

    fn karaoke_load(&mut self, host: &mut dyn Host, id: &str) {
        self.karaoke.id = id.to_string();
        self.karaoke.loading = true;
        self.karaoke.found = None;
        self.karaoke.error = None;
        self.karaoke.rev += 1;
        if library::is_server_id(id) && self.server.is_connected() {
            let rid = self
                .server
                .get(host, &bl_net::api::paths::track_karaoke(id));
            self.requests
                .insert(rid, Req::Karaoke(KaraokeReq::Get(id.to_string())));
            return;
        }
        if let Some(k) = self.offline_karaoke(id) {
            self.karaoke
                .loaded
                .post((id.to_string(), Some((k, "offline"))));
            return;
        }
        self.karaoke_load_device(host, id);
    }

    /// Karaoke data kept on this device.
    fn karaoke_load_device(&mut self, host: &mut dyn Host, id: &str) {
        let found = host
            .storage_read(&storage_key(id))
            .and_then(|b| bl_json::parse_bytes(&b).ok())
            .map(|v| (v, "device"));
        self.karaoke.loaded.post((id.to_string(), found));
    }

    /// Takes data as found (from `tick`): checks it against the song text.
    fn karaoke_found(&mut self, id: &str, found: Option<(Value, &'static str)>) {
        if id != self.karaoke.id {
            return;
        }
        self.karaoke.loading = false;
        self.karaoke.rev += 1;
        let Some((v, place)) = found else {
            self.karaoke.found = None;
            return;
        };
        // Computed on this device from another song text than the current one.
        if place == "device" {
            if let Some(text) = self.karaoke_lyrics(id) {
                if !stored::is_current(&v, Some(&text)) {
                    self.karaoke.found = None;
                    return;
                }
            }
        }
        self.karaoke.found = match KaraokeTrack::from_json(&v) {
            Ok(track) => Some(Found {
                track,
                source: v.str_or("source", "imported").to_string(),
                place,
            }),
            Err(_) => None,
        };
    }

    /// A server answer about karaoke data.
    pub(crate) fn karaoke_response(
        &mut self,
        host: &mut dyn Host,
        req: KaraokeReq,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        match req {
            KaraokeReq::Get(id) => {
                if id != self.karaoke.id {
                    return;
                }
                match res {
                    Ok(r) if r.status == 200 => {
                        let v = bl_json::parse_bytes(&r.body).ok();
                        // The offline copy follows the server.
                        self.offline_set_karaoke(host, &id, v.clone());
                        self.karaoke_found(&id, v.map(|v| (v, "server")));
                    }
                    Ok(r) if r.status == 404 => {
                        if self.is_offline(&id) {
                            self.offline_set_karaoke(host, &id, None);
                        }
                        // Computed on this device instead.
                        self.karaoke_load_device(host, &id);
                    }
                    _ => match self.offline_karaoke(&id) {
                        Some(k) => self.karaoke_found(&id, Some((k, "offline"))),
                        None => self.karaoke_load_device(host, &id),
                    },
                }
            }
            KaraokeReq::Compute(id) => {
                let Some(job) = self
                    .karaoke
                    .compute
                    .as_mut()
                    .filter(|j| j.id == id && j.place == "server")
                else {
                    return;
                };
                let status = match res {
                    Ok(r) if (200..300).contains(&r.status) => bl_json::parse_bytes(&r.body).ok(),
                    _ => None,
                };
                let state = status
                    .as_ref()
                    .map(|v| v.str_or("state", "idle").to_string())
                    .unwrap_or_else(|| "failed".into());
                match state.as_str() {
                    "queued" | "running" => {
                        let downloading = status
                            .as_ref()
                            .is_some_and(|v| v.get("model").str_or("state", "") == "downloading");
                        job.state = if downloading {
                            "downloading".into()
                        } else {
                            state
                        };
                        let p = status.as_ref().map_or(0.0, |v| v.f64_or("progress", 0.0));
                        job.progress
                            .store((p * 1000.0).round() as u32, Ordering::Relaxed);
                        job.asking = false;
                        job.next_poll = host.now() + POLL_SECS;
                    }
                    _ => {
                        self.karaoke.compute = None;
                        if state == "done" {
                            self.toast(self.tr("Karaoke data computed"), false);
                        } else {
                            let why = status
                                .as_ref()
                                .and_then(|v| v.get("error").as_str().map(str::to_string))
                                .unwrap_or_default();
                            self.karaoke.error = Some(why);
                            self.toast(self.tr("Computing the karaoke data failed"), true);
                        }
                        if self.karaoke.id == id {
                            self.karaoke_load(host, &id);
                        }
                    }
                }
                self.karaoke.rev += 1;
            }
            KaraokeReq::Saved => {
                if !matches!(res, Ok(r) if (200..300).contains(&r.status)) {
                    self.toast(self.tr("Saving the karaoke data failed"), true);
                    let id = self.karaoke.id.clone();
                    if !id.is_empty() {
                        self.karaoke_load(host, &id);
                    }
                }
            }
        }
    }

    /// `karaoke.compute`: on the server when it computes, else here.
    fn karaoke_compute(&mut self, host: &mut dyn Host, id: &str) -> Value {
        if self.karaoke.compute.is_some() {
            return Value::object().with("error", self.tr("Already working on a song"));
        }
        let Some(lyrics) = self.karaoke_lyrics(id) else {
            return Value::object().with(
                "error",
                self.tr("Karaoke data needs a song text with times"),
            );
        };
        self.karaoke.error = None;
        let job = |place: &'static str, state: &str| Compute {
            id: id.to_string(),
            place,
            state: state.into(),
            progress: Arc::default(),
            cancel: Arc::default(),
            next_poll: 0.0,
            asking: place == "server",
            lyrics: lyrics.clone(),
        };
        if library::is_server_id(id) && self.karaoke_server_can() {
            let rid = self.server.send(
                host,
                "POST",
                &bl_net::api::paths::track_karaoke_compute(id),
                Vec::new(),
                "text/plain",
            );
            self.requests
                .insert(rid, Req::Karaoke(KaraokeReq::Compute(id.to_string())));
            self.karaoke.compute = Some(job("server", "queued"));
            self.karaoke.rev += 1;
            return Value::object().with("ok", true).with("place", "server");
        }
        if !self.karaoke_device_can(host) {
            return Value::object().with("error", self.tr("Karaoke data cannot be computed here"));
        }
        self.karaoke.compute = Some(job("device", "queued"));
        self.karaoke.rev += 1;
        if let Err(e) = self.karaoke_device_start(host) {
            self.karaoke.compute = None;
            return Value::object().with("error", e);
        }
        Value::object().with("ok", true).with("place", "device")
    }

    /// Stores data for a song where it belongs and shows it.
    fn karaoke_keep(&mut self, host: &mut dyn Host, id: &str, v: Value, upload: bool) {
        let place: &'static str = if library::is_server_id(id) && upload {
            let rid = self.server.send(
                host,
                "PUT",
                &bl_net::api::paths::track_karaoke(id),
                v.to_json().into_bytes(),
                "application/json",
            );
            self.requests.insert(rid, Req::Karaoke(KaraokeReq::Saved));
            "server"
        } else {
            host.storage_write(&storage_key(id), v.to_json().as_bytes());
            "device"
        };
        if self.is_offline(id) {
            self.offline_set_karaoke(host, id, Some(v.clone()));
        }
        if self.karaoke.id == id {
            if let Ok(track) = KaraokeTrack::from_json(&v) {
                self.karaoke.found = Some(Found {
                    track,
                    source: v.str_or("source", "imported").to_string(),
                    place,
                });
                self.karaoke.loading = false;
            }
            self.karaoke.rev += 1;
        }
    }

    /// `karaoke.import`: UltraStar text or JSON for the song.
    fn karaoke_import(&mut self, host: &mut dyn Host, id: &str, data: &[u8]) -> Value {
        if data.len() > MAX_IMPORT {
            return Value::object().with("error", self.tr("This file has no usable karaoke data"));
        }
        let mut track = match stored::import(data) {
            Ok(t) => t,
            Err(_) => {
                return Value::object()
                    .with("error", self.tr("This file has no usable karaoke data"))
            }
        };
        let t = self.track_for(id);
        if track.title.is_empty() {
            track.title = t.title.clone();
        }
        if track.artist.is_empty() {
            track.artist = t.artists.join(", ");
        }
        let v = stored::to_stored(&track, Source::Imported, None);
        let upload = library::is_server_id(id) && self.server.is_connected();
        if library::is_server_id(id) && !upload {
            return Value::object().with("error", self.tr("Not connected to the server"));
        }
        self.karaoke_keep(host, id, v, upload);
        Value::object()
            .with("ok", true)
            .with("notes", track.notes().count() as u64)
    }

    /// Imports a picked file as the current track's karaoke data.
    pub(crate) fn karaoke_import_bytes(&mut self, host: &mut dyn Host, data: &[u8]) {
        let Some(id) = self.queue.current().cloned() else {
            return;
        };
        let r = self.karaoke_import(host, &id, data);
        match r.get("error").as_str() {
            Some(e) => self.toast(e.to_string(), true),
            None => self.toast(self.tr("Karaoke data added"), false),
        }
    }

    /// `karaoke.export`: writes the song's data as UltraStar text into a
    /// folder.
    fn karaoke_export(&mut self, id: &str, folder: &str) -> Value {
        let Some(f) = self
            .karaoke
            .found
            .as_ref()
            .filter(|_| self.karaoke.id == id)
        else {
            return Value::object().with("error", self.tr("This song has no karaoke data"));
        };
        let text = bl_pitch::ultrastar::to_ultrastar(&f.track);
        let name = format!("{}.txt", file_name(&f.track.artist, &f.track.title));
        let file = std::path::Path::new(folder).join(name);
        match std::fs::write(&file, text) {
            Ok(()) => {
                let msg = format!("{}: {}", self.tr("Saved"), file.display());
                self.toast(msg, false);
                Value::object().with("path", file.to_string_lossy().to_string())
            }
            Err(e) => {
                let msg = format!("{}: {e}", self.tr("Cannot save the karaoke data"));
                self.toast(msg, true);
                Value::object().with("error", e.to_string())
            }
        }
    }

    /// `karaoke.remove`: drops the song's karaoke data.
    fn karaoke_remove(&mut self, host: &mut dyn Host, id: &str) -> Value {
        let on_server = self
            .karaoke
            .found
            .as_ref()
            .is_some_and(|f| f.place == "server" && self.karaoke.id == id);
        if on_server {
            if !self.server.is_connected() {
                return Value::object().with("error", self.tr("Not connected to the server"));
            }
            let rid = self.server.send(
                host,
                "DELETE",
                &bl_net::api::paths::track_karaoke(id),
                Vec::new(),
                "text/plain",
            );
            self.requests.insert(rid, Req::Karaoke(KaraokeReq::Saved));
        }
        host.storage_delete(&storage_key(id));
        if self.is_offline(id) {
            self.offline_set_karaoke(host, id, None);
        }
        if self.karaoke.id == id {
            self.karaoke.found = None;
            self.karaoke.loading = false;
            self.karaoke.rev += 1;
        }
        Value::object().with("ok", true)
    }

    /// Takes loaded data and finished computations, asks the server about
    /// its computation (from `tick`).
    pub(crate) fn poll_karaoke(&mut self, host: &mut dyn Host) {
        for (id, found) in self.karaoke.loaded.take() {
            self.karaoke_found(&id, found);
        }
        for (id, res) in self.karaoke.computed.take() {
            let mine = self
                .karaoke
                .compute
                .as_ref()
                .is_some_and(|j| j.id == id && j.place == "device");
            if !mine {
                continue;
            }
            self.karaoke.compute = None;
            self.karaoke.rev += 1;
            match res {
                Ok(v) => {
                    self.karaoke_keep(host, &id, v, false);
                    self.toast(self.tr("Karaoke data computed"), false);
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => {
                    self.karaoke.error = Some(e.clone());
                    self.toast(
                        format!("{}: {e}", self.tr("Computing the karaoke data failed")),
                        true,
                    );
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_karaoke_model(host);
        self.poll_karaoke_vocals();
        let Some(j) = &mut self.karaoke.compute else {
            return;
        };
        if j.place != "server" || j.asking || host.now() < j.next_poll {
            return;
        }
        if !self.server.is_connected() {
            self.karaoke.compute = None;
            self.karaoke.rev += 1;
            return;
        }
        j.asking = true;
        let id = j.id.clone();
        let rid = self
            .server
            .get(host, &bl_net::api::paths::track_karaoke_compute(&id));
        self.requests
            .insert(rid, Req::Karaoke(KaraokeReq::Compute(id)));
    }

    pub(crate) fn karaoke_rev(&self) -> u64 {
        self.karaoke.rev
    }

    fn karaoke_vocals_level(&self) -> f32 {
        self.karaoke.vocals_level.unwrap_or(1.0)
    }

    /// `karaoke.vocals {level}`: how loud the singing of the track that
    /// plays is (1: as recorded, 0: out). Below 1 its singing is separated
    /// first (once per track).
    fn karaoke_vocals(&mut self, host: &mut dyn Host, level: f32) -> Value {
        let level = level.clamp(0.0, 1.0);
        self.karaoke.vocals_level = Some(level);
        self.karaoke.rev += 1;
        let serial = self.player.serial();
        if let Some(c) = self.player.vocal_cut().filter(|c| c.serial == serial) {
            c.amount.store((1.0 - level).to_bits(), Ordering::Relaxed);
            return Value::object().with("ok", true).with("state", "on");
        }
        if level >= 0.999 {
            return Value::object().with("ok", true).with("state", "original");
        }
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        let rate = self.player.device_rate();
        if let Some((_, r, samples)) = self
            .karaoke
            .vocals_kept
            .as_ref()
            .filter(|(k, r, _)| *k == id && *r == rate)
        {
            self.player
                .set_vocal_cut(Some(Arc::new(crate::player::VocalCut {
                    serial,
                    rate: *r,
                    samples: samples.clone(),
                    amount: AtomicU32::new((1.0 - level).to_bits()),
                })));
            return Value::object().with("ok", true).with("state", "on");
        }
        if self
            .karaoke
            .vocals_job
            .as_ref()
            .is_some_and(|j| j.id == id && j.serial == serial)
        {
            return Value::object().with("ok", true).with("state", "preparing");
        }
        if let Some(j) = self.karaoke.vocals_job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
        if !self.karaoke_device_can(host) {
            return Value::object().with("error", self.tr("Karaoke data cannot be computed here"));
        }
        self.karaoke.vocals_job = Some(VocalsJob {
            id,
            serial,
            state: "running".into(),
            progress: Arc::default(),
            cancel: Arc::default(),
        });
        if let Err(e) = self.karaoke_vocals_start(host) {
            self.karaoke.vocals_job = None;
            return Value::object().with("error", e);
        }
        Value::object().with("ok", true).with("state", "preparing")
    }

    /// Puts separated singing in place when it is for what plays now, and
    /// drops that of a track no longer playing (from `tick`).
    fn poll_karaoke_vocals(&mut self) {
        let serial = self.player.serial();
        if self.player.vocal_cut().is_some_and(|c| c.serial != serial) {
            self.player.set_vocal_cut(None);
            self.karaoke.rev += 1;
        }
        for (id, s, res) in self.karaoke.vocals_ready.take() {
            if self
                .karaoke
                .vocals_job
                .as_ref()
                .is_some_and(|j| j.id == id && j.serial == s)
            {
                self.karaoke.vocals_job = None;
            }
            self.karaoke.rev += 1;
            match res {
                Ok((rate, samples)) => {
                    let samples = Arc::new(samples);
                    self.karaoke.vocals_kept = Some((id.clone(), rate, samples.clone()));
                    if s == serial {
                        let amount = 1.0 - self.karaoke_vocals_level();
                        self.player
                            .set_vocal_cut(Some(Arc::new(crate::player::VocalCut {
                                serial: s,
                                rate,
                                samples,
                                amount: AtomicU32::new(amount.to_bits()),
                            })));
                    }
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => self.toast(
                    format!("{}: {e}", self.tr("The singing could not be separated")),
                    true,
                ),
            }
        }
        // A job for a track no longer playing stops.
        if let Some(j) = &self.karaoke.vocals_job {
            if j.serial != serial {
                j.cancel.store(true, Ordering::Relaxed);
                self.karaoke.vocals_job = None;
                self.karaoke.rev += 1;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Core {
    fn karaoke_models(&mut self, host: &dyn Host) -> Option<Arc<bl_asr::models::Models>> {
        if self.karaoke.models.is_none() {
            let dir = host.storage_path("models/.keep").and_then(|p| {
                std::path::PathBuf::from(p)
                    .parent()
                    .map(std::path::PathBuf::from)
            })?;
            self.karaoke.models = Some(Arc::new(bl_asr::models::Models::new(dir)));
        }
        self.karaoke.models.clone()
    }

    /// The separation model's file, when it is there.
    fn karaoke_model_path(&mut self, host: &dyn Host) -> Option<std::path::PathBuf> {
        let m = self.karaoke_models(host)?;
        let p = m.dir.join(bl_sep::MODEL_ID).join(bl_sep::MODEL_FILE);
        p.is_file().then_some(p)
    }

    /// The song's audio, when the device can read it.
    fn karaoke_opener(&self, host: &dyn Host, id: &str) -> Option<crate::player::SourceOpener> {
        if let Some(path) = library::local_path(id).map(str::to_string) {
            let ext = crate::jobs::extension(&path);
            return Some(Box::new(move || {
                let f = std::fs::File::open(&path).map_err(|e| format!("{path}: {e}"))?;
                Ok((
                    Box::new(std::io::BufReader::new(f)) as Box<dyn bl_media::Source>,
                    ext,
                ))
            }));
        }
        if id.starts_with(crate::sources::ID_PREFIX) {
            return self.source_opener(id);
        }
        if id.starts_with(crate::vault::ID_PREFIX) {
            return self.vault_opener(host, id);
        }
        if let Some(o) = self.offline_opener(id) {
            return Some(o);
        }
        if library::is_server_id(id) && self.server.is_connected() {
            let url = self.server.track_stream_url(id);
            let headers = self.server.headers();
            let t = self.track_for(id);
            let ext = crate::jobs::extension(&t.path)
                .or_else(|| (!t.codec.is_empty()).then(|| t.codec.to_ascii_lowercase()));
            return Some(Box::new(move || {
                let s = bl_net::client::HttpStream::open(&url, &headers)?;
                Ok((Box::new(s) as Box<dyn bl_media::Source>, ext))
            }));
        }
        None
    }

    /// Starts the computation on the device (after the separation model's
    /// download, when it is not there yet).
    fn karaoke_device_start(&mut self, host: &mut dyn Host) -> Result<(), String> {
        let Some(job) = self.karaoke.compute.clone() else {
            return Ok(());
        };
        let Some(opener) = self.karaoke_opener(host, &job.id) else {
            return Err(self
                .tr("This song cannot be read on this device")
                .to_string());
        };
        let Some(model) = self.karaoke_model_path(host) else {
            let Some(models) = self.karaoke_models(host) else {
                return Err(self.tr("Karaoke data cannot be computed here").to_string());
            };
            let ready = self.karaoke.model_ready.clone();
            models.download_files(bl_sep::MODEL_ID, bl_sep::model_files(), move |_| {
                ready.post(())
            });
            if let Some(j) = &mut self.karaoke.compute {
                j.state = "downloading".into();
            }
            return Ok(());
        };
        if let Some(j) = &mut self.karaoke.compute {
            j.state = "running".into();
        }
        let out = self.karaoke.computed.clone();
        let t = self.track_for(&job.id);
        let (title, artist) = (t.title.clone(), t.artists.join(", "));
        host.spawn(Box::new(move || {
            let res = (|| {
                let (src, ext) = opener()?;
                let cancel = job.cancel.clone();
                let audio = bl_karaoke::decode(src, ext.as_deref(), &mut || {
                    !cancel.load(Ordering::Relaxed)
                })?;
                let sep = bl_sep::Separator::load(&model)?;
                let mut progress = |p: f32| {
                    job.progress
                        .store((p * 1000.0).round() as u32, Ordering::Relaxed);
                    !job.cancel.load(Ordering::Relaxed)
                };
                let (mut track, _) = bl_karaoke::compute(&sep, &audio, &job.lyrics, &mut progress)?;
                if track.title.is_empty() {
                    track.title = title;
                }
                if track.artist.is_empty() {
                    track.artist = artist;
                }
                Ok(stored::to_stored(
                    &track,
                    Source::Computed,
                    Some(&job.lyrics),
                ))
            })();
            out.post((job.id.clone(), res));
        }));
        Ok(())
    }

    /// Separates the singing of the track that plays, for turning it down
    /// (after the separation model's download, when it is not there yet).
    fn karaoke_vocals_start(&mut self, host: &mut dyn Host) -> Result<(), String> {
        let Some(job) = self.karaoke.vocals_job.clone() else {
            return Ok(());
        };
        let Some(opener) = self.karaoke_opener(host, &job.id) else {
            return Err(self
                .tr("This song cannot be read on this device")
                .to_string());
        };
        let Some(model) = self.karaoke_model_path(host) else {
            let Some(models) = self.karaoke_models(host) else {
                return Err(self.tr("Karaoke data cannot be computed here").to_string());
            };
            let ready = self.karaoke.model_ready.clone();
            models.download_files(bl_sep::MODEL_ID, bl_sep::model_files(), move |_| {
                ready.post(())
            });
            if let Some(j) = &mut self.karaoke.vocals_job {
                j.state = "downloading".into();
            }
            return Ok(());
        };
        if let Some(j) = &mut self.karaoke.vocals_job {
            j.state = "running".into();
        }
        let rate = self.player.device_rate();
        let out = self.karaoke.vocals_ready.clone();
        host.spawn(Box::new(move || {
            let res = (|| {
                let (src, ext) = opener()?;
                let cancel = job.cancel.clone();
                let audio = bl_karaoke::decode(src, ext.as_deref(), &mut || {
                    !cancel.load(Ordering::Relaxed)
                })?;
                let sep = bl_sep::Separator::load(&model)?;
                let mut progress = |p: f32| {
                    job.progress
                        .store((p * 1000.0).round() as u32, Ordering::Relaxed);
                    !job.cancel.load(Ordering::Relaxed)
                };
                let refs: Vec<&[f32]> = audio.channels.iter().map(Vec::as_slice).collect();
                let v = sep.separate(
                    &refs,
                    audio.rate,
                    bl_sep::Options {
                        wiener: false,
                        progress: &mut progress,
                    },
                )?;
                drop(audio);
                let l = bl_sep::resample(&v.left, bl_sep::SAMPLE_RATE, rate);
                let r = bl_sep::resample(&v.right, bl_sep::SAMPLE_RATE, rate);
                drop(v);
                let q = |x: f32| (x.clamp(-1.0, 1.0) * 32767.0) as i16;
                let samples: Vec<i16> =
                    l.iter().zip(&r).flat_map(|(a, b)| [q(*a), q(*b)]).collect();
                Ok((rate, samples))
            })();
            out.post((job.id.clone(), job.serial, res));
        }));
        Ok(())
    }

    /// Follows the separation model's download for a waiting computation.
    fn poll_karaoke_model(&mut self, host: &mut dyn Host) {
        let ready = !self.karaoke.model_ready.take().is_empty();
        if ready
            && self
                .karaoke
                .vocals_job
                .as_ref()
                .is_some_and(|j| j.state == "downloading")
        {
            if let Err(e) = self.karaoke_vocals_start(host) {
                self.karaoke.vocals_job = None;
                self.toast(e, true);
            }
        }
        let waiting = self
            .karaoke
            .compute
            .as_ref()
            .is_some_and(|j| j.place == "device" && j.state == "downloading");
        if !waiting {
            return;
        }
        if ready {
            if let Err(e) = self.karaoke_device_start(host) {
                self.karaoke.compute = None;
                self.karaoke.error = Some(e.clone());
                self.toast(e, true);
            }
            self.karaoke.rev += 1;
            return;
        }
        let Some(models) = self.karaoke.models.clone() else {
            return;
        };
        match models.state(bl_sep::MODEL_ID) {
            Some(bl_asr::models::Download::Running { done, total }) => {
                let p = (done as f64 / total.max(1) as f64 * 1000.0) as u32;
                if let Some(j) = &self.karaoke.compute {
                    if j.progress.swap(p, Ordering::Relaxed) / 10 != p / 10 {
                        self.karaoke.rev += 1;
                    }
                }
            }
            Some(bl_asr::models::Download::Failed(e)) => {
                self.karaoke.compute = None;
                self.karaoke.error = Some(e.clone());
                self.toast(
                    format!("{}: {e}", self.tr("Computing the karaoke data failed")),
                    true,
                );
                self.karaoke.rev += 1;
            }
            _ => {}
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Core {
    fn karaoke_device_start(&mut self, _host: &mut dyn Host) -> Result<(), String> {
        Err(self.tr("Karaoke data cannot be computed here").to_string())
    }

    fn karaoke_vocals_start(&mut self, _host: &mut dyn Host) -> Result<(), String> {
        Err(self.tr("Karaoke data cannot be computed here").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(file_name("A/B", "Song?"), "A_B - Song_");
        assert_eq!(file_name("", "Song"), "Song");
        assert_eq!(file_name(" ", " "), "Karaoke");
    }
}
