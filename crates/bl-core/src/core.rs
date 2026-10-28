//! The core: playback, library, queues, presets, backgrounds, server
//! connection and the visualizer. The user interface drives it through
//! [`Core::command`] (see `commands.rs`), calls [`Core::tick`] regularly and
//! [`Core::render`] once per displayed frame.

use crate::host::{Host, HttpResponse};
use crate::jobs::{decode_mono, extension, Mailbox};
use crate::library::{self, LibraryState, LOCAL_PREFIX};
use crate::player::{PlayState, Player, PlayerEvent, SourceOpener};
use crate::queue::Queue;
use crate::server::{ServerClient, ServerState};
use crate::settings::{BackgroundMode, Repeat, Settings, VisualizerKind};
use bl_analysis::loudness::{Loudness, LoudnessMeter};
use bl_analysis::realtime::RealtimeAnalyzer;
use bl_analysis::TrackAnalysis;
use bl_fx::{Engine, FrameInput, ImageRef, Preset, Rng};
use bl_gl::Gl;
use bl_json::Value;
use bl_net::api::{ImageEntry, Library, Playlist, ServerInfo, Track};
use bl_render::{Batch2d, Rect, Target, Visualizer};
use std::collections::HashMap;
use std::sync::Arc;

/// Command line / launch options.
#[derive(Debug, Clone, Default)]
pub struct LaunchArgs {
    pub files: Vec<String>,
    pub server: Option<String>,
    pub fullscreen: bool,
    pub preset: Option<String>,
    pub backgrounds: Vec<String>,
}

impl LaunchArgs {
    pub fn parse(args: impl Iterator<Item = String>) -> Result<LaunchArgs, String> {
        let mut a = LaunchArgs::default();
        let mut it = args.peekable();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--server" => a.server = it.next(),
                "--background" | "--bg" => a.backgrounds.extend(it.next()),
                "--preset" => a.preset = it.next(),
                "--fullscreen" => a.fullscreen = true,
                s if s.starts_with("--") => return Err(format!("unknown option {s}")),
                _ => a.files.push(arg),
            }
        }
        Ok(a)
    }

    pub const USAGE: &'static str = "usage: beatlumen [options] [audio files...]\n\
        \n  --server URL        connect to a Beatlumen server\
        \n  --background PATH   add a background image (repeatable)\
        \n  --preset NAME       activate a preset\
        \n  --data DIR          settings/data directory\
        \n  --fullscreen        start in fullscreen";
}

/// HTTP request purposes, for dispatching responses.
#[derive(Debug, Clone)]
pub(crate) enum Req {
    Info,
    Login,
    Library,
    Playlists,
    PlaylistSaved,
    Images,
    Presets,
    PresetGet(String),
    PresetSaved(String),
    Analysis(String),
    Loudness(String),
    TrackDownload {
        track_id: String,
        autoplay: bool,
        generation: u64,
    },
    BgImage(String),
    Upload(String),
    Rescan,
    /// A vault request and the session it was sent with.
    Vault(crate::vault::VaultReq, String),
    WebCheck(crate::webcheck::WebReq),
}

pub(crate) struct Gfx {
    pub vis: Visualizer,
    pub batch: Batch2d,
}

/// A decoded background image delivered by a worker.
pub(crate) struct Decoded {
    pub key: String,
    pub result: Result<bl_image::Image, String>,
    pub animate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalysisStatus {
    None,
    Working,
    Ready,
    Failed,
}

impl AnalysisStatus {
    pub fn name(self) -> &'static str {
        match self {
            AnalysisStatus::None => "none",
            AnalysisStatus::Working => "working",
            AnalysisStatus::Ready => "ready",
            AnalysisStatus::Failed => "failed",
        }
    }
}

/// Where the active preset came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetOrigin {
    Builtin,
    User,
    Server,
}

impl PresetOrigin {
    pub fn name(self) -> &'static str {
        match self {
            PresetOrigin::Builtin => "builtin",
            PresetOrigin::User => "user",
            PresetOrigin::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetEntry {
    pub name: String,
    pub origin: PresetOrigin,
}

/// Where the live spectrum/waveform overlay is drawn (logical pixels of
/// the window, set by the UI).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Overlay {
    pub rect: [f32; 4],
    pub alpha: f32,
}

/// Change counters of the lists the UI shows; the UI refetches a list when
/// its counter changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Revisions {
    pub queue: u64,
    pub playlists: u64,
    pub presets: u64,
    pub preset: u64,
    pub backgrounds: u64,
    pub images: u64,
    pub analysis: u64,
    pub settings: u64,
}

pub const DEFAULT_BG: &str = "builtin:default";

/// Loudness all tracks are normalized to (LUFS).
const NORMALIZE_TARGET_LUFS: f64 = -16.0;

/// Parses a cached loudness ("<LUFS or -> <peak>").
fn parse_loudness(s: &str) -> Option<Loudness> {
    let mut it = s.split_whitespace();
    let integrated = match it.next()? {
        "-" => None,
        v => Some(v.parse().ok()?),
    };
    Some(Loudness {
        integrated,
        peak: it.next()?.parse().ok()?,
    })
}

pub struct Core {
    pub settings: Settings,
    pub(crate) settings_dirty_at: Option<f64>,
    /// Position in the restored current track, used by the first play.
    resume_at: Option<f64>,
    position_saved_at: f64,
    pub(crate) rng: Rng,
    pub(crate) gfx: Option<Gfx>,
    pub(crate) engine: Engine,
    pub preset: Preset,
    pub(crate) preset_origin: PresetOrigin,
    pub(crate) preset_dirty: bool,
    pub(crate) preset_list: Vec<PresetEntry>,
    pub(crate) undo: Vec<Preset>,
    pub(crate) redo: Vec<Preset>,
    pub(crate) last_edit: (String, f64),
    pub(crate) player: Player,
    pub(crate) current: Option<Track>,
    pub(crate) play_generation: u64,
    /// Track handed to the player for gapless playback: (player serial, id).
    preloaded: (u64, Option<String>),
    /// Measured loudness per track id (None while measuring).
    loudness: HashMap<String, Option<Loudness>>,
    loudness_box: Mailbox<(String, Result<Loudness, String>)>,
    pub(crate) queue: Queue,
    pub(crate) bg_queue: Queue,
    pub(crate) library: LibraryState,
    pub(crate) analysis: Option<Arc<TrackAnalysis>>,
    pub(crate) analysis_key: String,
    pub(crate) analysis_status: AnalysisStatus,
    pub(crate) analysis_box: Mailbox<(String, Result<Vec<u8>, String>)>,
    pub(crate) analysis_requested: bool,
    /// Whether the app is on screen. In the background (phone locked,
    /// another app in front) no visuals are drawn, so the track analysis
    /// they need is put off until the app is visible again.
    pub(crate) visible: bool,
    pub(crate) live: RealtimeAnalyzer,
    live_buf: Vec<f32>,
    last_live_pos: f64,
    pub(crate) spectrum: Vec<f32>,
    pub(crate) waveform: Vec<f32>,
    next_image_id: u64,
    pub(crate) bg_source: Option<String>,
    pub(crate) bg_pending: Option<String>,
    pub(crate) image_box: Mailbox<Decoded>,
    /// Extracted local cover files by track id (None = no cover).
    pub(crate) covers: HashMap<String, Option<String>>,
    pub(crate) cover_box: Mailbox<(String, Option<String>)>,
    pub(crate) server: ServerClient,
    pub(crate) requests: HashMap<u64, Req>,
    pub(crate) server_playlists: Vec<Playlist>,
    pub(crate) local_playlists: Vec<Playlist>,
    pub(crate) server_images: Vec<ImageEntry>,
    pub(crate) server_presets: Vec<String>,
    /// Files handed over as bytes (browser), by name.
    pub(crate) memory_files: HashMap<String, Vec<u8>>,
    pub(crate) pending_playlist_items: Option<(String, Vec<String>)>,
    /// Metadata of files played outside the library.
    pub(crate) extra_tracks: HashMap<String, Track>,
    pub(crate) events: Vec<Value>,
    pub(crate) revs: Revisions,
    last_state: String,
    pub(crate) blur_target: f32,
    menu_blur: f32,
    pub(crate) overlay: Overlay,
    launch: LaunchArgs,
    pub(crate) last_dt: f64,
    pub(crate) time: f64,
    pub(crate) download_progress: Option<String>,
    pub(crate) airplay: crate::airplay::AirplayState,
    pub(crate) outputs: crate::outputs::Outputs,
    pub(crate) vault: crate::vault::Vault,
    pub(crate) webcheck: crate::webcheck::WebCheckState,
    pub(crate) push: crate::push::PushState,
    started: bool,
}

/// Default background: soft coloured light blobs on a dark gradient.
fn default_background(w: u32, h: u32, seed: u64) -> bl_image::Image {
    let mut rng = Rng::new(seed);
    let blobs: Vec<(f32, f32, f32, (f32, f32, f32))> = (0..6)
        .map(|_| {
            (
                rng.f32(),
                rng.f32(),
                0.25 + rng.f32() * 0.45,
                hue(250.0 + rng.f32() * 140.0),
            )
        })
        .collect();
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32;
            let v = y as f32 / h as f32;
            let mut col = (0.03 + 0.04 * v, 0.025 + 0.02 * v, 0.06 + 0.05 * (1.0 - v));
            for (bx, by, br, c) in &blobs {
                let dx = (u - bx) * (w as f32 / h as f32);
                let dy = v - by;
                let k = (-(dx * dx + dy * dy) / (br * br) * 2.5).exp() * 0.32;
                col.0 += c.0 * k;
                col.1 += c.1 * k;
                col.2 += c.2 * k;
            }
            // Subtle dithering noise.
            let n = (rng.f32() - 0.5) * 0.012;
            let i = ((y * w + x) * 4) as usize;
            px[i] = ((col.0 + n).clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 1] = ((col.1 + n).clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 2] = ((col.2 + n).clamp(0.0, 1.0) * 255.0) as u8;
            px[i + 3] = 255;
        }
    }
    bl_image::Image {
        width: w,
        height: h,
        pixels: px,
    }
}

fn hue(h: f32) -> (f32, f32, f32) {
    let h = (h % 360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as i32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    (0.35 + 0.65 * r, 0.25 + 0.65 * g, 0.4 + 0.6 * b)
}

impl Core {
    /// Creates the core. `language` is the UI language ("de", "en", …)
    /// used for messages until the user picks one.
    pub fn new(host: &mut dyn Host, launch: LaunchArgs, language: &str) -> Core {
        let mut settings = host
            .storage_read(Settings::STORAGE_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .map(|v| Settings::from_json(&v))
            .unwrap_or_default();
        if settings.language.is_empty() {
            settings.language = language.to_string();
        }
        let mut rng = Rng::new(host.random_seed());
        let mut player = Player::new();
        player.set_volume(if settings.muted { 0.0 } else { settings.volume });
        player.set_transition(settings.crossfade, settings.skip_silence);
        let queue = Queue::new(
            settings.audio_queue.clone(),
            settings.audio_index,
            settings.shuffle,
            &mut rng,
        );
        let bg_queue = Queue::new(
            settings.backgrounds.clone(),
            settings.background_index,
            settings.background_shuffle,
            &mut rng,
        );
        let mut library = LibraryState::default();
        if let Some(d) = host.storage_read("library-local.json") {
            library.load_local_cache(&d);
        }
        let local_playlists = host
            .storage_read("playlists.json")
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .and_then(|v| {
                v.get("playlists")
                    .as_array()
                    .map(|a| a.iter().map(Playlist::from_json).collect())
            })
            .unwrap_or_default();
        let preset = bl_fx::preset::builtin("Balanced", &mut rng).unwrap_or_default();
        let analysis_cfg = preset.analysis_config();
        let server_url = launch
            .server
            .clone()
            .unwrap_or_else(|| settings.server_url.clone());
        let server = ServerClient::new(&server_url, &settings.server_token);
        let resume_at =
            Some(settings.audio_position).filter(|p| *p > 1.0 && settings.audio_index.is_some());
        let mut core = Core {
            settings,
            settings_dirty_at: None,
            resume_at,
            position_saved_at: 0.0,
            engine: Engine::new(rng.next_u64()),
            rng,
            gfx: None,
            preset,
            preset_origin: PresetOrigin::Builtin,
            preset_dirty: false,
            preset_list: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: (String::new(), 0.0),
            player,
            current: None,
            play_generation: 0,
            preloaded: (0, None),
            loudness: HashMap::new(),
            loudness_box: Mailbox::default(),
            queue,
            bg_queue,
            library,
            analysis: None,
            analysis_key: String::new(),
            analysis_status: AnalysisStatus::None,
            analysis_box: Mailbox::default(),
            analysis_requested: false,
            visible: true,
            live: RealtimeAnalyzer::new(48000, &analysis_cfg),
            live_buf: vec![0.0; 2048],
            last_live_pos: 0.0,
            spectrum: Vec::new(),
            waveform: Vec::new(),
            next_image_id: 10,
            bg_source: None,
            bg_pending: None,
            image_box: Mailbox::default(),
            covers: HashMap::new(),
            cover_box: Mailbox::default(),
            server,
            requests: HashMap::new(),
            server_playlists: Vec::new(),
            local_playlists,
            server_images: Vec::new(),
            server_presets: Vec::new(),
            memory_files: HashMap::new(),
            pending_playlist_items: None,
            extra_tracks: HashMap::new(),
            events: Vec::new(),
            revs: Revisions::default(),
            last_state: String::new(),
            blur_target: 0.0,
            menu_blur: 0.0,
            overlay: Overlay::default(),
            launch,
            last_dt: 0.0,
            time: 0.0,
            download_progress: None,
            airplay: Default::default(),
            outputs: Default::default(),
            vault: Default::default(),
            webcheck: Default::default(),
            push: Default::default(),
            started: false,
        };
        core.engine.set_intensity(core.settings.intensity);
        core.engine.set_adaptive(core.settings.adaptive);
        // Native apps load server images through the core's HTTP client.
        core.server.image_provider = host.platform() != "web";
        core.refresh_preset_list(host);
        let wanted = core
            .launch
            .preset
            .clone()
            .unwrap_or_else(|| core.settings.preset.clone());
        core.load_preset_by_name(host, &wanted);
        core.engine.set_preset(Some(core.preset.clone()));
        if host.platform() == "web" || core.settings.use_server || core.launch.server.is_some() {
            core.connect_server(host);
        }
        if !core.settings.library_folders.is_empty() && host.has_threads() {
            core.rescan_local();
        }
        core
    }

    // ---- events and state ----------------------------------------------------

    pub(crate) fn emit(&mut self, ev: Value) {
        self.events.push(ev);
    }

    /// Events since the last call (JSON array).
    pub fn take_events(&mut self) -> Value {
        Value::Array(std::mem::take(&mut self.events))
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>, error: bool) {
        let text: String = text.into();
        self.emit(
            Value::object()
                .with("type", "toast")
                .with("text", text)
                .with("error", error),
        );
    }

    pub(crate) fn tr<'a>(&self, s: &'a str) -> &'a str {
        crate::strings::tr(&self.settings.language, s)
    }

    pub(crate) fn settings_changed(&mut self) {
        self.settings_dirty_at = Some(self.time);
        self.revs.settings += 1;
    }

    fn sync_settings_state(&mut self) {
        self.settings.audio_queue = self.queue.items.clone();
        self.settings.audio_index = self.queue.current_index();
        self.settings.shuffle = self.queue.shuffle;
        self.settings.backgrounds = self.bg_queue.items.clone();
        self.settings.background_index = self.bg_queue.current_index();
        self.settings.background_shuffle = self.bg_queue.shuffle;
    }

    pub fn save_settings(&mut self, host: &mut dyn Host) {
        self.sync_settings_state();
        if self.resume_at.is_none() {
            self.settings.audio_position = if self.current.is_some() {
                self.player.position(host)
            } else {
                0.0
            };
        }
        host.storage_write(
            Settings::STORAGE_KEY,
            self.settings.to_json().to_json_pretty().as_bytes(),
        );
        self.settings_dirty_at = None;
    }

    /// Settings as shown to the UI (without queues and secrets).
    pub(crate) fn settings_json(&self) -> Value {
        let mut v = self.settings.to_json();
        if let Some(m) = v.as_object_mut() {
            for k in [
                "audioQueue",
                "audioIndex",
                "audioPosition",
                "backgrounds",
                "backgroundIndex",
                "serverToken",
                "version",
                "airplayPassword",
                "airplayId",
            ] {
                m.remove(k);
            }
        }
        v.set(
            "airplayPasswordSet",
            !self.settings.airplay_password.is_empty(),
        );
        v
    }

    /// Compact state snapshot (everything except the playback position,
    /// which the UI polls separately).
    pub fn state(&self, host: &dyn Host) -> Value {
        let (server_state, server_name, server_error) = match &self.server.state {
            ServerState::Disabled => ("disabled", String::new(), String::new()),
            ServerState::Connecting => ("connecting", String::new(), String::new()),
            ServerState::Connected(i) => ("connected", i.name.clone(), String::new()),
            ServerState::NeedsLogin(i) => ("login", i.name.clone(), String::new()),
            ServerState::Error(e) => ("error", String::new(), e.clone()),
        };
        // A certificate the system does not trust: its fingerprint, so the
        // user can compare it and choose to trust it.
        let (server_error, untrusted_cert) =
            match server_error.split_once(bl_net::UNTRUSTED_CERT_MARKER) {
                Some((msg, rest)) => (
                    msg.trim_end().to_string(),
                    rest.split(']').next().unwrap_or("").to_string(),
                ),
                None => (server_error, String::new()),
            };
        // Source code of the connected server (AGPL section 13), else ours.
        let source_url = match &self.server.state {
            ServerState::Connected(i) | ServerState::NeedsLogin(i) if !i.source_url.is_empty() => {
                i.source_url.clone()
            }
            _ => bl_net::api::SOURCE_URL.to_string(),
        };
        let play = match self.player.state() {
            PlayState::Stopped => "stopped",
            PlayState::Loading => "loading",
            PlayState::Playing => "playing",
            PlayState::Paused => "paused",
        };
        let scan = self
            .library
            .scan
            .lock()
            .map(|p| (p.running, p.found, p.processed))
            .unwrap_or((false, 0, 0));
        let current = self
            .current
            .as_ref()
            .map(|t| self.track_json(t))
            .unwrap_or(Value::Null);
        Value::object()
            .with("platform", host.platform())
            .with("play", play)
            .with(
                "duration",
                self.player
                    .duration()
                    .or_else(|| self.current.as_ref().map(|t| t.duration_ms as f64 / 1000.0))
                    .unwrap_or(0.0),
            )
            .with("current", current)
            .with("loading", self.download_progress.clone())
            .with("queueIndex", self.queue.position())
            .with("queueLength", self.queue.len())
            .with("shuffle", self.queue.shuffle)
            .with("repeat", self.settings.repeat.name())
            .with("volume", self.settings.volume)
            .with("muted", self.settings.muted)
            .with("server", server_state)
            .with("serverName", server_name)
            .with("serverError", server_error)
            .with("serverUrl", self.server.base.as_str())
            .with("sourceUrl", source_url)
            .with("serverUntrustedCertificate", untrusted_cert)
            .with("preset", self.preset.name.as_str())
            .with("presetOrigin", self.preset_origin.name())
            .with("presetDirty", self.preset_dirty)
            .with("canUndo", !self.undo.is_empty())
            .with("canRedo", !self.redo.is_empty())
            .with("analysis", self.analysis_status.name())
            .with("background", self.bg_source.clone())
            .with("backgroundIndex", self.bg_queue.position())
            .with("backgroundCount", self.bg_queue.len())
            .with("backgroundShuffle", self.bg_queue.shuffle)
            .with("underruns", self.player.underruns())
            .with("scanning", scan.0)
            .with("scanFound", scan.1)
            .with("scanProcessed", scan.2)
            .with("libraryRev", self.library.revision)
            .with("libraryTracks", self.library.tracks.len())
            .with("queueRev", self.revs.queue)
            .with("playlistsRev", self.revs.playlists)
            .with("presetsRev", self.revs.presets)
            .with("presetRev", self.revs.preset)
            .with("backgroundsRev", self.revs.backgrounds)
            .with("imagesRev", self.revs.images)
            .with("analysisRev", self.revs.analysis)
            .with("settingsRev", self.revs.settings)
            .with("airplay", self.airplay_json())
            .with("outputs", self.outputs_json())
            .with("vault", self.vault_json(host))
            .with("webCheck", self.webcheck_json())
            .with(
                "serverWebIntegrity",
                match &self.server.state {
                    ServerState::Connected(i) | ServerState::NeedsLogin(i) => {
                        i.web_integrity.clone()
                    }
                    _ => String::new(),
                },
            )
            .with("settings", self.settings_json())
    }

    /// Track as shown in lists.
    pub(crate) fn track_json(&self, t: &Track) -> Value {
        Value::object()
            .with("id", t.id.as_str())
            .with("title", t.title.as_str())
            .with("artist", t.artists.join(", "))
            .with("album", t.album.as_str())
            .with("albumId", t.album_id.as_str())
            .with("duration", t.duration_ms as f64 / 1000.0)
            .with("track", t.track)
            .with("year", t.year)
            .with("local", library::is_local(&t.id))
            .with("cover", self.cover_url_cached(t))
    }

    // ---- server ----------------------------------------------------------------

    pub(crate) fn connect_server(&mut self, host: &mut dyn Host) {
        if host.platform() != "web" && self.server.base.is_empty() {
            self.server.state = ServerState::Disabled;
            return;
        }
        // The browser handles TLS itself on the web.
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(u) = bl_net::client::Url::parse(&self.server.base) {
            let pin = &self.settings.server_cert_pin;
            bl_net::client::set_certificate_pin(
                &format!("{}:{}", u.host, u.port),
                Some(pin.as_str()).filter(|p| !p.is_empty()),
            );
        }
        self.server.state = ServerState::Connecting;
        let id = self.server.request_info(host);
        self.requests.insert(id, Req::Info);
    }

    pub(crate) fn server_login(&mut self, host: &mut dyn Host, password: &str) {
        let id = self.server.request_login(host, password);
        self.requests.insert(id, Req::Login);
    }

    pub(crate) fn refresh_server_data(&mut self, host: &mut dyn Host) {
        let reqs = [
            (bl_net::api::paths::LIBRARY, Req::Library),
            (bl_net::api::paths::PLAYLISTS, Req::Playlists),
            (bl_net::api::paths::IMAGES, Req::Images),
            (bl_net::api::paths::PRESETS, Req::Presets),
        ];
        for (p, r) in reqs {
            let id = self.server.get(host, p);
            self.requests.insert(id, r);
        }
    }

    pub(crate) fn server_rescan(&mut self, host: &mut dyn Host) {
        let id = self.server.send(
            host,
            "POST",
            bl_net::api::paths::RESCAN,
            Vec::new(),
            "application/json",
        );
        self.requests.insert(id, Req::Rescan);
    }

    fn handle_http(&mut self, host: &mut dyn Host, id: u64, res: Result<HttpResponse, String>) {
        let Some(req) = self.requests.remove(&id) else {
            return;
        };
        let ok_json = |r: &Result<HttpResponse, String>| -> Option<Value> {
            match r {
                Ok(resp) if (200..300).contains(&resp.status) => {
                    bl_json::parse_bytes(&resp.body).ok()
                }
                _ => None,
            }
        };
        if let Req::Vault(v, token) = req {
            return self.vault_http(host, v, &token, res);
        }
        if let Req::WebCheck(w) = req {
            return self.webcheck_http(host, w, res);
        }
        let status = res.as_ref().map(|r| r.status).unwrap_or(0);
        if status == 401 && !matches!(req, Req::Login | Req::Info) {
            if let ServerState::Connected(info) = &self.server.state {
                self.server.state = ServerState::NeedsLogin(info.clone());
            }
            return;
        }
        match req {
            Req::Info => match ok_json(&res) {
                Some(v) => {
                    let info = ServerInfo::from_json(&v);
                    if info.auth_required && self.server.token.is_empty() {
                        self.server.state = ServerState::NeedsLogin(info);
                    } else {
                        self.server.state = ServerState::Connected(info);
                        self.refresh_server_data(host);
                    }
                }
                None => {
                    let msg = match &res {
                        Err(e) => e.clone(),
                        Ok(r) => format!("HTTP {}", r.status),
                    };
                    self.server.state = ServerState::Error(msg);
                }
            },
            Req::Login => {
                match ok_json(&res).and_then(|v| v.get("token").as_str().map(str::to_string)) {
                    Some(token) => {
                        self.server.token = token.clone();
                        self.settings.server_token = token;
                        self.settings_changed();
                        self.connect_server(host);
                    }
                    None => self.toast(self.tr("Login failed"), true),
                }
            }
            Req::Library => {
                if let Some(v) = ok_json(&res) {
                    self.library.set_remote(Library::from_json(&v));
                    self.restore_current_track();
                }
            }
            Req::Playlists => {
                if let Some(v) = ok_json(&res) {
                    self.server_playlists = v
                        .as_array()
                        .map(|a| a.iter().map(Playlist::from_json).collect())
                        .unwrap_or_default();
                    self.revs.playlists += 1;
                }
            }
            Req::PlaylistSaved => {
                let id = self.server.get(host, bl_net::api::paths::PLAYLISTS);
                self.requests.insert(id, Req::Playlists);
            }
            Req::Images => {
                if let Some(v) = ok_json(&res) {
                    self.server_images = v
                        .as_array()
                        .map(|a| a.iter().map(ImageEntry::from_json).collect())
                        .unwrap_or_default();
                    self.revs.images += 1;
                }
            }
            Req::Presets => {
                if let Some(v) = ok_json(&res) {
                    self.server_presets = v
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| p.get("name").as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.refresh_preset_list(host);
                    if self.preset_origin == PresetOrigin::Builtin
                        && self.settings.preset != self.preset.name
                    {
                        let wanted = self.settings.preset.clone();
                        if self.server_presets.contains(&wanted) {
                            self.load_preset_by_name(host, &wanted);
                        }
                    }
                }
            }
            Req::PresetGet(name) => match &res {
                Ok(r) if r.status == 200 => match Preset::parse(&r.body, &name, &mut self.rng) {
                    Ok(p) => {
                        self.preset_origin = PresetOrigin::Server;
                        self.apply_new_preset(p);
                    }
                    Err(e) => self.toast(format!("{}: {e}", self.tr("Invalid preset")), true),
                },
                _ => self.toast(self.tr("Could not load preset"), true),
            },
            Req::PresetSaved(name) => {
                if matches!(&res, Ok(r) if (200..300).contains(&r.status)) {
                    self.toast(format!("{} \"{name}\"", self.tr("Saved preset")), false);
                    let id = self.server.get(host, bl_net::api::paths::PRESETS);
                    self.requests.insert(id, Req::Presets);
                } else {
                    self.toast(self.tr("Saving the preset failed"), true);
                }
            }
            Req::Loudness(id) => {
                let l = match res {
                    Ok(r) if r.status == 200 => bl_json::parse(&String::from_utf8_lossy(&r.body))
                        .map(|v| Loudness {
                            integrated: v.get("integrated").as_f64(),
                            peak: v.f32_or("peak", 1.0),
                        })
                        .map_err(|e| e.to_string()),
                    Ok(r) => Err(format!("HTTP {}", r.status)),
                    Err(e) => Err(e),
                };
                self.loudness_box.post((id, l));
            }
            Req::Analysis(key) => {
                if key != self.analysis_key {
                    return;
                }
                match res {
                    Ok(r) if r.status == 200 => self.analysis_box.post((key, Ok(r.body))),
                    Ok(r) => self
                        .analysis_box
                        .post((key, Err(format!("HTTP {}", r.status)))),
                    Err(e) => self.analysis_box.post((key, Err(e))),
                }
            }
            Req::TrackDownload {
                track_id,
                autoplay,
                generation,
            } => {
                self.download_progress = None;
                if generation != self.play_generation {
                    return;
                }
                match res {
                    Ok(r) if r.status == 200 => {
                        let ext = self.library.track(&track_id).map(|t| t.codec.clone());
                        let data = r.body;
                        let opener: SourceOpener = Box::new(move || {
                            Ok((
                                Box::new(std::io::Cursor::new(data)) as Box<dyn bl_media::Source>,
                                ext,
                            ))
                        });
                        self.player.load(host, opener, autoplay);
                    }
                    Ok(r) => self.toast(
                        format!("{}: HTTP {}", self.tr("Could not load track"), r.status),
                        true,
                    ),
                    Err(e) => self.toast(format!("{}: {e}", self.tr("Could not load track")), true),
                }
            }
            Req::BgImage(source) => match res {
                Ok(r) if r.status == 200 => {
                    let animate = self.bg_source.is_some();
                    self.decode_background_async(host, source, r.body, animate);
                }
                _ => {
                    self.toast(self.tr("Could not load background image"), true);
                    self.bg_pending = None;
                }
            },
            Req::Upload(name) => {
                if matches!(&res, Ok(r) if (200..300).contains(&r.status)) {
                    self.toast(format!("{} {name}", self.tr("Uploaded")), false);
                    let id = self.server.get(host, bl_net::api::paths::IMAGES);
                    self.requests.insert(id, Req::Images);
                } else {
                    self.toast(format!("{} {name}", self.tr("Upload failed:")), true);
                }
            }
            Req::Rescan => self.toast(self.tr("Server library rescan started"), false),
            Req::Vault(..) | Req::WebCheck(_) => {}
        }
    }

    // ---- presets -------------------------------------------------------------

    pub(crate) fn refresh_preset_list(&mut self, host: &mut dyn Host) {
        let mut list: Vec<PresetEntry> = bl_fx::preset::BUILTIN
            .iter()
            .map(|(n, _)| PresetEntry {
                name: n.to_string(),
                origin: PresetOrigin::Builtin,
            })
            .collect();
        for key in host.storage_list("presets/") {
            if let Some(name) = key
                .strip_prefix("presets/")
                .and_then(|n| n.strip_suffix(".hfx"))
            {
                list.push(PresetEntry {
                    name: name.to_string(),
                    origin: PresetOrigin::User,
                });
            }
        }
        for name in &self.server_presets {
            list.push(PresetEntry {
                name: name.clone(),
                origin: PresetOrigin::Server,
            });
        }
        self.preset_list = list;
        self.revs.presets += 1;
    }

    pub(crate) fn load_preset_by_name(&mut self, host: &mut dyn Host, name: &str) {
        if let Some(data) = host.storage_read(&format!("presets/{name}.hfx")) {
            match Preset::parse(&data, name, &mut self.rng) {
                Ok(p) => {
                    self.preset_origin = PresetOrigin::User;
                    self.apply_new_preset(p);
                    return;
                }
                Err(e) => host.log(&format!("preset {name}: {e}")),
            }
        }
        if self.server_presets.iter().any(|n| n == name) && self.server.is_connected() {
            let id = self.server.get(host, &bl_net::api::paths::preset(name));
            self.requests.insert(id, Req::PresetGet(name.to_string()));
            return;
        }
        if let Some(p) = bl_fx::preset::builtin(name, &mut self.rng) {
            self.preset_origin = PresetOrigin::Builtin;
            self.apply_new_preset(p);
        }
    }

    pub(crate) fn apply_new_preset(&mut self, p: Preset) {
        let analysis_changed = p.analysis_config() != self.preset.analysis_config();
        self.settings.preset = p.name.clone();
        self.settings_changed();
        self.preset = p;
        self.preset_dirty = false;
        self.undo.clear();
        self.redo.clear();
        self.engine.set_preset(Some(self.preset.clone()));
        self.revs.preset += 1;
        if analysis_changed {
            self.live = RealtimeAnalyzer::new(
                self.player.device_rate().max(8000),
                &self.preset.analysis_config(),
            );
            self.request_analysis_for_current();
        }
    }

    fn preset_applied(&mut self, analysis_changed: bool) {
        self.preset_dirty = true;
        self.engine.set_preset(Some(self.preset.clone()));
        self.revs.preset += 1;
        if analysis_changed {
            self.live = RealtimeAnalyzer::new(
                self.player.device_rate().max(8000),
                &self.preset.analysis_config(),
            );
            self.request_analysis_for_current();
        }
    }

    /// Records an edit of the preset (undo history) and applies it.
    pub(crate) fn preset_edited(&mut self, before: Preset) {
        let analysis_changed = before.analysis_config() != self.preset.analysis_config();
        self.undo.push(before);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.preset_applied(analysis_changed);
    }

    /// Like [`Core::preset_edited`] but merges rapid consecutive edits of
    /// the same field (slider drags) into one undo step.
    pub(crate) fn preset_edited_coalesced(&mut self, before: Preset, key: &str) {
        let merge =
            self.last_edit.0 == key && self.time - self.last_edit.1 < 1.0 && !self.undo.is_empty();
        self.last_edit = (key.to_string(), self.time);
        if merge {
            let analysis_changed = before.analysis_config() != self.preset.analysis_config();
            self.redo.clear();
            self.preset_applied(analysis_changed);
        } else {
            self.preset_edited(before);
        }
    }

    pub(crate) fn undo_preset(&mut self) {
        if let Some(p) = self.undo.pop() {
            let cur = std::mem::replace(&mut self.preset, p);
            let changed = cur.analysis_config() != self.preset.analysis_config();
            self.redo.push(cur);
            self.preset_applied(changed);
        }
    }

    pub(crate) fn redo_preset(&mut self) {
        if let Some(p) = self.redo.pop() {
            let cur = std::mem::replace(&mut self.preset, p);
            let changed = cur.analysis_config() != self.preset.analysis_config();
            self.undo.push(cur);
            self.preset_applied(changed);
        }
    }

    /// Saves the preset under `name` (user storage, or the server when
    /// `to_server`).
    pub(crate) fn save_preset_as(&mut self, host: &mut dyn Host, name: &str, to_server: bool) {
        let name: String = name
            .chars()
            .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
            .collect::<String>()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        self.preset.name = name.clone();
        let json = self.preset.to_string_pretty();
        if to_server && self.server.is_connected() {
            let id = self.server.send(
                host,
                "PUT",
                &bl_net::api::paths::preset(&name),
                json.into_bytes(),
                "application/json",
            );
            self.requests.insert(id, Req::PresetSaved(name.clone()));
            self.preset_origin = PresetOrigin::Server;
        } else if host.storage_write(&format!("presets/{name}.hfx"), json.as_bytes()) {
            self.preset_origin = PresetOrigin::User;
            self.toast(format!("{} \"{name}\"", self.tr("Saved preset")), false);
        } else {
            self.toast(self.tr("Saving the preset failed"), true);
        }
        self.preset_dirty = false;
        self.settings.preset = name;
        self.settings_changed();
        self.refresh_preset_list(host);
        self.revs.preset += 1;
    }

    pub(crate) fn delete_preset(&mut self, host: &mut dyn Host, name: &str) {
        let Some(entry) = self
            .preset_list
            .iter()
            .find(|e| e.name == name && e.origin != PresetOrigin::Builtin)
            .cloned()
        else {
            return;
        };
        match entry.origin {
            PresetOrigin::User => host.storage_delete(&format!("presets/{}.hfx", entry.name)),
            PresetOrigin::Server => {
                let id = self.server.send(
                    host,
                    "DELETE",
                    &bl_net::api::paths::preset(&entry.name),
                    Vec::new(),
                    "application/json",
                );
                self.requests
                    .insert(id, Req::PresetSaved(entry.name.clone()));
                self.server_presets.retain(|n| n != &entry.name);
            }
            PresetOrigin::Builtin => return,
        }
        self.refresh_preset_list(host);
        if self.preset.name == name {
            self.load_preset_by_name(host, "Balanced");
        }
    }

    pub(crate) fn import_preset_bytes(&mut self, name: &str, data: &[u8]) {
        match Preset::parse(data, name, &mut self.rng) {
            Ok(p) => {
                let before = self.preset.clone();
                self.preset = p;
                self.preset.name = name.to_string();
                self.preset_edited(before);
                self.toast(format!("{} {name}", self.tr("Imported preset")), false);
            }
            Err(e) => self.toast(format!("{}: {e}", self.tr("Invalid preset")), true),
        }
    }

    // ---- playback --------------------------------------------------------------

    /// Track metadata for a queue entry (library or a bare local path).
    pub(crate) fn track_for(&self, id: &str) -> Track {
        if let Some(t) = self.library.track(id) {
            return t.clone();
        }
        if let Some(t) = self.extra_tracks.get(id) {
            return t.clone();
        }
        if let Some(t) = self.vault_track(id) {
            return t;
        }
        if let Some(p) = library::local_path(id) {
            return library::local_track(p, 0, 0, None);
        }
        if let Some(name) = id.strip_prefix("mem:") {
            return library::local_track(name, 0, 0, None);
        }
        Track {
            id: id.to_string(),
            title: id.to_string(),
            ..Default::default()
        }
    }

    fn restore_current_track(&mut self) {
        if self.current.is_none() {
            if let Some(id) = self.queue.current().cloned() {
                self.current = Some(self.track_for(&id));
            }
        } else if let Some(cur) = &self.current {
            if let Some(t) = self.library.track(&cur.id) {
                self.current = Some(t.clone());
            }
        }
        self.revs.queue += 1;
    }

    /// Starts playing the queue's current item.
    /// True while an AirPlay sender's stream is the current track.
    pub(crate) fn playing_airplay(&self) -> bool {
        self.airplay.active
            && self
                .current
                .as_ref()
                .is_some_and(|t| t.id.starts_with(crate::airplay::ID_PREFIX))
    }

    pub(crate) fn play_current(&mut self, host: &mut dyn Host, autoplay: bool) {
        self.start_current(host, autoplay, true);
    }

    /// Makes the queue's current item the current track; `load` starts its
    /// playback (false when the player already advanced to it gaplessly).
    fn start_current(&mut self, host: &mut dyn Host, autoplay: bool, load: bool) {
        self.resume_at = None;
        self.release_airplay(host);
        self.revs.queue += 1;
        let Some(id) = self.queue.current().cloned() else {
            self.player.stop(host);
            self.current = None;
            return;
        };
        if self.library.track(&id).is_none() && !self.extra_tracks.contains_key(&id) {
            if let Some(p) = library::local_path(&id) {
                let ext = extension(p);
                let meta = host.fs_metadata(p);
                let info = host
                    .fs_open(p)
                    .ok()
                    .and_then(|mut src| bl_media::read_info(&mut *src, ext.as_deref(), false).ok());
                let t = library::local_track(
                    p,
                    meta.as_ref().map(|m| m.size).unwrap_or(0),
                    meta.as_ref().map(|m| m.modified).unwrap_or(0),
                    info.as_ref(),
                );
                self.extra_tracks.insert(id.clone(), t);
            }
        }
        let track = self.track_for(&id);
        self.play_generation += 1;
        self.current = Some(track.clone());
        self.engine.reset_schedule();
        self.analysis = None;
        self.analysis_status = AnalysisStatus::None;
        self.revs.analysis += 1;
        self.live.reset();
        let gain = self.track_gain(host, &id);
        self.player.set_gain(gain, load);
        if !load {
            // Already playing (gapless advance).
        } else if let Some(opener) = self.opener_for(host, &id) {
            self.player.load(host, opener, autoplay);
        } else if let Some(path) = library::local_path(&id) {
            match host.fs_read(path) {
                Ok(data) => {
                    let ext = extension(path);
                    let opener: SourceOpener = Box::new(move || {
                        Ok((
                            Box::new(std::io::Cursor::new(data)) as Box<dyn bl_media::Source>,
                            ext,
                        ))
                    });
                    self.player.load(host, opener, autoplay);
                }
                Err(e) => self.toast(e, true),
            }
        } else if id.starts_with(crate::vault::ID_PREFIX) {
            // Encrypted file without streaming: downloaded first.
            self.player.stop(host);
            self.vault_download(host, &id, autoplay);
        } else if !id.starts_with("mem:") {
            // Server track without threads: downloaded first.
            self.player.stop(host);
            let url = self.server.track_stream_url(&id);
            let req = crate::host::HttpRequest {
                method: "GET".into(),
                url,
                headers: self.server.headers(),
                body: Vec::new(),
            };
            let rid = host.http_request(req);
            self.requests.insert(
                rid,
                Req::TrackDownload {
                    track_id: id.clone(),
                    autoplay,
                    generation: self.play_generation,
                },
            );
            self.download_progress = Some(track.title.clone());
        }
        self.request_analysis_for_current_with(host);
        if self.settings.cover_fallback && self.bg_queue.is_empty() {
            self.show_cover_background(host);
        }
        self.settings_changed();
    }

    /// Opener streaming a track (None when it must be read or downloaded
    /// up front because the platform has no threads).
    fn opener_for(&self, host: &dyn Host, id: &str) -> Option<SourceOpener> {
        if let Some(path) = library::local_path(id).map(str::to_string) {
            if !host.has_threads() {
                return None;
            }
            let ext = extension(&path);
            return Some(Box::new(move || {
                let f = std::fs::File::open(&path).map_err(|e| format!("{path}: {e}"))?;
                Ok((
                    Box::new(std::io::BufReader::with_capacity(64 * 1024, f))
                        as Box<dyn bl_media::Source>,
                    ext,
                ))
            }));
        }
        if let Some(name) = id.strip_prefix("mem:") {
            let data = self.memory_files.get(name).cloned()?;
            let ext = extension(name);
            return Some(Box::new(move || {
                Ok((
                    Box::new(std::io::Cursor::new(data)) as Box<dyn bl_media::Source>,
                    ext,
                ))
            }));
        }
        if id.starts_with(crate::vault::ID_PREFIX) {
            return self.vault_opener(host, id);
        }
        if !host.has_threads() || host.audio_is_push() {
            return None;
        }
        let url = self.server.track_stream_url(id);
        let codec = self.track_for(id).codec;
        let ext = if codec.is_empty() { None } else { Some(codec) };
        let headers = self.server.headers();
        Some(Box::new(move || Ok((http_stream(&url, &headers)?, ext))))
    }

    /// The track that plays after the current one ends.
    fn upcoming(&self) -> Option<String> {
        if self.settings.repeat == Repeat::Item {
            return self.queue.current().cloned();
        }
        self.queue.peek_next(self.settings.repeat).cloned()
    }

    /// Keeps the player's preloaded next track in sync with the queue.
    fn sync_preload(&mut self, host: &mut dyn Host) {
        if !matches!(self.player.state(), PlayState::Playing | PlayState::Paused) {
            return;
        }
        let serial = self.player.serial();
        let want = self.upcoming();
        if self.preloaded.0 == serial && self.preloaded.1 == want {
            return;
        }
        let opener = want.as_deref().and_then(|id| self.opener_for(host, id));
        let want = if opener.is_some() { want } else { None };
        if self.player.preload(opener) {
            let gain = match &want {
                Some(id) => self.track_gain(host, id),
                None => None,
            };
            self.player.set_next_gain(gain);
            self.preloaded = (serial, want);
        } else {
            self.preloaded = (serial, None);
        }
    }

    // ---- loudness normalization ------------------------------------------------

    /// Storage key of a track's cached loudness (changes with the file).
    fn loudness_storage_key(&self, id: &str) -> String {
        let t = self.track_for(id);
        format!(
            "loudness/{:016x}.txt",
            library::fnv(&format!("{id}|{}|{}", t.size, t.modified))
        )
    }

    /// Normalization gain of a track: 1 when normalization is off, None
    /// while its loudness is being measured (the measurement is started).
    fn track_gain(&mut self, host: &mut dyn Host, id: &str) -> Option<f32> {
        if !self.settings.normalize {
            return Some(1.0);
        }
        match self.loudness.get(id) {
            Some(Some(l)) => return Some(l.gain_for(NORMALIZE_TARGET_LUFS, 1.0)),
            Some(None) => return None,
            None => {}
        }
        let key = self.loudness_storage_key(id);
        if let Some(l) = host
            .storage_read(&key)
            .and_then(|b| parse_loudness(&String::from_utf8_lossy(&b)))
        {
            self.loudness.insert(id.to_string(), Some(l));
            return Some(l.gain_for(NORMALIZE_TARGET_LUFS, 1.0));
        }
        self.measure_loudness(host, id);
        None
    }

    /// Measures a track's loudness in the background (server tracks are
    /// measured by the server).
    fn measure_loudness(&mut self, host: &mut dyn Host, id: &str) {
        let measure =
            |src: Box<dyn bl_media::Source>, ext: Option<String>| -> Result<Loudness, String> {
                let mut dec = bl_media::open(src, ext.as_deref()).map_err(|e| e.to_string())?;
                let spec = dec.spec();
                let mut meter = LoudnessMeter::new(spec.sample_rate, spec.channels);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    let more = dec.next_chunk(&mut buf).map_err(|e| e.to_string())?;
                    meter.push(&buf);
                    if !more {
                        return Ok(meter.finish());
                    }
                }
            };
        let mailbox = self.loudness_box.clone();
        let key = id.to_string();
        if let Some(path) = library::local_path(id).map(str::to_string) {
            let ext = extension(&path);
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    let res = std::fs::File::open(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|f| {
                            measure(
                                Box::new(std::io::BufReader::with_capacity(64 * 1024, f)),
                                ext,
                            )
                        });
                    mailbox.post((key, res));
                }));
            } else {
                let res = host
                    .fs_read(&path)
                    .and_then(|d| measure(Box::new(std::io::Cursor::new(d)), ext));
                mailbox.post((key, res));
            }
        } else if let Some(name) = id.strip_prefix("mem:") {
            let Some(data) = self.memory_files.get(name).cloned() else {
                return;
            };
            let ext = extension(name);
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    mailbox.post((key, measure(Box::new(std::io::Cursor::new(data)), ext)))
                }));
            } else {
                mailbox.post((key, measure(Box::new(std::io::Cursor::new(data)), ext)));
            }
        } else if id.starts_with(crate::vault::ID_PREFIX) {
            let Some(opener) = self.vault_opener(host, id) else {
                return;
            };
            let job =
                move || mailbox.post((key, opener().and_then(|(src, ext)| measure(src, ext))));
            if host.has_threads() {
                host.spawn(Box::new(job));
            } else {
                job();
            }
        } else if self.server.is_connected() {
            let rid = self
                .server
                .get(host, &bl_net::api::paths::track_loudness(id));
            self.requests.insert(rid, Req::Loudness(key));
        } else {
            return;
        }
        self.loudness.insert(id.to_string(), None);
    }

    fn poll_loudness(&mut self, host: &mut dyn Host) {
        for (id, res) in self.loudness_box.take() {
            match res {
                Ok(l) => {
                    let text = format!(
                        "{} {}",
                        l.integrated.map_or("-".to_string(), |v| v.to_string()),
                        l.peak
                    );
                    let key = self.loudness_storage_key(&id);
                    host.storage_write(&key, text.as_bytes());
                    self.loudness.insert(id.clone(), Some(l));
                }
                Err(e) => {
                    host.log(&format!("loudness of {id}: {e}"));
                    // Played without normalization (not retried this session).
                    self.loudness.insert(
                        id.clone(),
                        Some(Loudness {
                            integrated: None,
                            peak: 0.0,
                        }),
                    );
                }
            }
            let gain = self.track_gain(host, &id);
            if self.current.as_ref().is_some_and(|t| t.id == id) {
                self.player.set_gain(gain, false);
            }
            if self.preloaded.1.as_deref() == Some(id.as_str()) {
                self.player.set_next_gain(gain);
            }
        }
    }

    /// Re-applies the gains after normalization was switched on or off.
    pub(crate) fn apply_normalization(&mut self, host: &mut dyn Host) {
        if let Some(id) = self.current.as_ref().map(|t| t.id.clone()) {
            let gain = self.track_gain(host, &id);
            self.player.set_gain(gain, false);
        }
        if let Some(id) = self.preloaded.1.clone() {
            let gain = self.track_gain(host, &id);
            self.player.set_next_gain(gain);
        }
    }

    /// The preloaded track became audible: move the queue along without
    /// reloading the player.
    fn on_track_advanced(&mut self, host: &mut dyn Host) {
        let expected = self.preloaded.1.take();
        if self.settings.repeat == Repeat::Item && expected.as_ref() == self.queue.current() {
            self.engine.reset_schedule();
            self.revs.queue += 1;
            return;
        }
        let next = self.queue.next(self.settings.repeat).cloned();
        if next.is_some() && next == expected {
            self.start_current(host, true, false);
            match self.settings.background_mode {
                BackgroundMode::OnTrackFinish => self.next_background(host),
                BackgroundMode::OnTrackChange
                    if self.settings.background_repeat != Repeat::Item =>
                {
                    self.next_background(host)
                }
                _ => {}
            }
        } else {
            // The queue changed in the meantime: play what it says.
            self.play_current(host, true);
        }
    }

    pub(crate) fn play_tracks(&mut self, host: &mut dyn Host, ids: Vec<String>, start: usize) {
        if ids.is_empty() {
            return;
        }
        let shuffle = self.queue.shuffle;
        self.queue.shuffle = false;
        self.queue.replace(ids, start, &mut self.rng);
        if shuffle {
            self.queue.set_shuffle(true, &mut self.rng);
        }
        self.play_current(host, true);
    }

    pub(crate) fn enqueue(&mut self, host: &mut dyn Host, ids: Vec<String>, next: bool) {
        let n = ids.len();
        let was_empty = self.queue.is_empty();
        if next && !was_empty {
            self.queue.play_next(ids);
        } else {
            self.queue.push(ids, &mut self.rng);
        }
        if was_empty {
            self.queue.set_position(0);
            self.play_current(host, true);
        }
        self.revs.queue += 1;
        self.toast(format!("{n} {}", self.tr("added to the queue")), false);
        self.settings_changed();
    }

    pub(crate) fn next_track(&mut self, host: &mut dyn Host, user: bool) {
        let repeat = if user && self.settings.repeat == Repeat::Item {
            Repeat::Queue
        } else {
            self.settings.repeat
        };
        let repeat = if repeat == Repeat::Item {
            Repeat::None
        } else {
            repeat
        };
        if self.queue.next(repeat).is_some() {
            self.play_current(host, true);
            if self.settings.background_mode == BackgroundMode::OnTrackChange
                && self.settings.background_repeat != Repeat::Item
            {
                self.next_background(host);
            }
        } else if user {
            self.player.stop(host);
        }
    }

    pub(crate) fn prev_track(&mut self, host: &mut dyn Host) {
        if self.player.position(host) > 4.0 {
            self.player.seek(host, 0.0);
            return;
        }
        let repeat = if self.settings.repeat == Repeat::Queue {
            Repeat::Queue
        } else {
            Repeat::None
        };
        if self.queue.prev(repeat).is_some() {
            self.play_current(host, true);
            if self.settings.background_mode == BackgroundMode::OnTrackChange
                && self.settings.background_repeat != Repeat::Item
            {
                self.prev_background(host);
            }
        }
    }

    pub(crate) fn toggle_play(&mut self, host: &mut dyn Host) {
        if self.playing_airplay() {
            // A speaker cannot pause its sender: stop receiving.
            self.release_airplay(host);
            return;
        }
        match self.player.state() {
            PlayState::Stopped => {
                if self.queue.current().is_none() && !self.queue.is_empty() {
                    self.queue.set_position(0);
                }
                if self.queue.current().is_some() {
                    let resume = self.resume_at.take();
                    self.play_current(host, true);
                    // Continue the track that played when the app closed.
                    if let Some(p) = resume {
                        self.player.seek(host, p);
                    }
                }
            }
            _ => self.player.toggle(host),
        }
    }

    fn on_track_ended(&mut self, host: &mut dyn Host) {
        if self.settings.repeat == Repeat::Item {
            self.player.seek(host, 0.0);
            self.player.play(host);
            return;
        }
        if self.queue.next(self.settings.repeat).is_some() {
            self.play_current(host, true);
            match self.settings.background_mode {
                BackgroundMode::OnTrackFinish => self.next_background(host),
                BackgroundMode::OnTrackChange
                    if self.settings.background_repeat != Repeat::Item =>
                {
                    self.next_background(host)
                }
                _ => {}
            }
        } else {
            self.revs.queue += 1;
        }
    }

    pub(crate) fn set_volume(&mut self, v: f32) {
        self.settings.volume = v.clamp(0.0, 1.0);
        self.settings.muted = false;
        self.player.set_volume(self.settings.volume);
        self.settings_changed();
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.settings.muted = muted;
        self.player
            .set_volume(if muted { 0.0 } else { self.settings.volume });
        self.settings_changed();
    }

    /// Playback position in seconds.
    pub fn position(&self, host: &dyn Host) -> f64 {
        if let Some(p) = self.airplay_position() {
            return p;
        }
        self.player.position(host)
    }

    pub fn is_playing(&self) -> bool {
        self.player.state() == PlayState::Playing
    }

    // ---- analysis ----------------------------------------------------------------

    pub(crate) fn request_analysis_for_current(&mut self) {
        // Deferred: the next tick calls the host-aware variant.
        self.analysis_key.clear();
        self.analysis_requested = true;
    }

    fn request_analysis_for_current_with(&mut self, host: &mut dyn Host) {
        if !self.visible {
            // Analysed when the visuals are shown again (saves battery).
            self.analysis_requested = true;
            return;
        }
        self.analysis_requested = false;
        let Some(track) = self.current.clone() else {
            return;
        };
        if track.id.starts_with(crate::airplay::ID_PREFIX) {
            // Live stream: the visuals follow the audio as it plays.
            return;
        }
        let cfg = self.preset.analysis_config();
        let key = format!(
            "{}|{}|{}|{:016x}",
            track.id,
            track.size,
            track.modified,
            cfg.cache_key()
        );
        if key == self.analysis_key {
            return;
        }
        self.analysis_key = key.clone();
        self.analysis = None;
        self.revs.analysis += 1;
        let storage_key = format!("analysis/{:016x}.blan", library::fnv(&key));
        if let Some(blob) = host.storage_read(&storage_key) {
            if let Some(a) = bl_analysis::decode(&blob) {
                self.analysis = Some(Arc::new(a));
                self.analysis_status = AnalysisStatus::Ready;
                return;
            }
        }
        self.analysis_status = AnalysisStatus::Working;
        let analyze = move |src: Box<dyn bl_media::Source>,
                            ext: Option<String>,
                            cfg: bl_analysis::AnalysisConfig| {
            decode_mono(src, ext.as_deref())
                .and_then(|(mono, sr)| {
                    bl_analysis::analyze(&mono, sr, &cfg, &mut |_| true)
                        .ok_or_else(|| "cancelled".to_string())
                })
                .map(|a| bl_analysis::encode(&a))
        };
        if let Some(path) = library::local_path(&track.id).map(str::to_string) {
            let mailbox = self.analysis_box.clone();
            let ext = extension(&path);
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    let res = std::fs::File::open(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|f| analyze(Box::new(std::io::BufReader::new(f)), ext, cfg));
                    mailbox.post((key, res));
                }));
            } else {
                let res = host
                    .fs_read(&path)
                    .and_then(|d| analyze(Box::new(std::io::Cursor::new(d)), ext, cfg));
                mailbox.post((key, res));
            }
        } else if let Some(name) = track.id.strip_prefix("mem:") {
            if let Some(data) = self.memory_files.get(name).cloned() {
                let res = analyze(Box::new(std::io::Cursor::new(data)), extension(name), cfg);
                self.analysis_box.post((key, res));
            }
        } else if track.id.starts_with(crate::vault::ID_PREFIX) {
            // Encrypted: only this device can analyse it. Without streaming
            // the file is analysed once it has been downloaded.
            if let Some(opener) = self.vault_opener(host, &track.id) {
                let mailbox = self.analysis_box.clone();
                let job = move || {
                    let res = opener().and_then(|(src, ext)| analyze(src, ext, cfg));
                    mailbox.post((key, res));
                };
                if host.has_threads() {
                    host.spawn(Box::new(job));
                } else {
                    job();
                }
            }
        } else if self.server.is_connected() {
            let cfg_json = Value::object()
                .with("analysisConfig", self.preset.analysis.clone())
                .with("noveltyConfigs", self.preset.novelty.clone())
                .to_json();
            let path = self.server.analysis_path(&track.id, &cfg_json);
            let id = self.server.get(host, &path);
            self.requests.insert(id, Req::Analysis(key));
        } else {
            self.analysis_status = AnalysisStatus::Failed;
        }
    }

    fn poll_analysis(&mut self, host: &mut dyn Host) {
        for (key, res) in self.analysis_box.take() {
            if key != self.analysis_key {
                continue;
            }
            match res
                .ok()
                .and_then(|blob| bl_analysis::decode(&blob).map(|a| (a, blob)))
            {
                Some((a, blob)) => {
                    host.storage_write(
                        &format!("analysis/{:016x}.blan", library::fnv(&key)),
                        &blob,
                    );
                    self.analysis = Some(Arc::new(a));
                    self.analysis_status = AnalysisStatus::Ready;
                }
                None => self.analysis_status = AnalysisStatus::Failed,
            }
            self.revs.analysis += 1;
        }
    }

    // ---- covers --------------------------------------------------------------

    /// URL of a track's cover for the UI, if known without blocking:
    /// server URL, extracted local file, or "" (none / not yet extracted).
    pub(crate) fn cover_url_cached(&self, t: &Track) -> String {
        if t.id.starts_with(crate::airplay::ID_PREFIX) {
            self.airplay.cover_url().to_string()
        } else if library::is_local(&t.id) {
            match self.covers.get(&t.id) {
                Some(Some(path)) => file_url(path),
                _ => String::new(),
            }
        } else if t.id.starts_with("mem:") {
            String::new()
        } else if t.has_cover && self.server.is_connected() {
            self.server
                .image_url(&bl_net::api::paths::track_cover(&t.id, 512))
        } else {
            String::new()
        }
    }

    /// Starts extracting the covers of local tracks (embedded picture or a
    /// cover file next to it) into the cache; emits `cover` events.
    pub(crate) fn request_covers(&mut self, host: &mut dyn Host, ids: Vec<String>) {
        let mut jobs = Vec::new();
        for id in ids {
            if self.covers.contains_key(&id) || !library::is_local(&id) {
                continue;
            }
            let Some(path) = library::local_path(&id).map(str::to_string) else {
                continue;
            };
            // Covers are shared per directory when there is no embedded one;
            // the cache key is the file itself to keep things simple.
            let key = format!("covers/{:016x}", library::fnv(&id));
            let Some(base) = host.storage_path(&key) else {
                continue;
            };
            self.covers.insert(id.clone(), None);
            jobs.push((id, path, base));
        }
        if jobs.is_empty() {
            return;
        }
        let mailbox = self.cover_box.clone();
        let job = move || {
            for (id, path, base) in jobs {
                let out = ["jpg", "png", "img"]
                    .iter()
                    .map(|e| format!("{base}.{e}"))
                    .find(|p| std::path::Path::new(p).exists())
                    .or_else(|| {
                        let bytes = local_cover_bytes(&path)?;
                        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                            "png"
                        } else if bytes.starts_with(&[0xff, 0xd8]) {
                            "jpg"
                        } else {
                            "img"
                        };
                        let file = format!("{base}.{ext}");
                        if let Some(parent) = std::path::Path::new(&file).parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::write(&file, bytes).ok().map(|_| file)
                    });
                mailbox.post((id, out));
            }
        };
        if host.has_threads() {
            host.spawn(Box::new(job));
        } else {
            job();
        }
    }

    fn poll_covers(&mut self) {
        for (id, path) in self.cover_box.take() {
            let url = path.as_deref().map(file_url).unwrap_or_default();
            self.covers.insert(id.clone(), path);
            if !url.is_empty() {
                self.emit(
                    Value::object()
                        .with("type", "cover")
                        .with("id", id)
                        .with("url", url),
                );
            }
        }
    }

    // ---- backgrounds -----------------------------------------------------------

    pub(crate) fn decode_background_async(
        &mut self,
        host: &mut dyn Host,
        key: String,
        data: Vec<u8>,
        animate: bool,
    ) {
        let mailbox = self.image_box.clone();
        host.spawn(Box::new(move || {
            let result = std::panic::catch_unwind(|| {
                bl_image::decode_max(&data, 2560).map_err(|e| e.to_string())
            })
            .unwrap_or_else(|_| Err("image decoder failed".to_string()));
            mailbox.post(Decoded {
                key,
                result,
                animate,
            });
        }));
    }

    /// Loads a background from a source string: `file:<path>`,
    /// `srv:<image id>`, `mem:<name>`, `cover:<track id>` or the built-in default.
    pub(crate) fn load_background(&mut self, host: &mut dyn Host, source: &str) {
        if self.bg_pending.as_deref() == Some(source)
            || (self.bg_source.as_deref() == Some(source) && self.bg_pending.is_none())
        {
            return;
        }
        self.bg_pending = Some(source.to_string());
        let animate = self.bg_source.is_some();
        if source == DEFAULT_BG {
            let seed = self.rng.next_u64();
            let img = default_background(960, 540, seed);
            self.image_box.post(Decoded {
                key: source.to_string(),
                result: Ok(img),
                animate,
            });
        } else if let Some(path) = source.strip_prefix(LOCAL_PREFIX) {
            let mailbox = self.image_box.clone();
            let (path, key) = (path.to_string(), source.to_string());
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    let result = std::fs::read(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|d| {
                            std::panic::catch_unwind(|| {
                                bl_image::decode_max(&d, 2560).map_err(|e| e.to_string())
                            })
                            .unwrap_or_else(|_| Err("image decoder failed".into()))
                        });
                    mailbox.post(Decoded {
                        key,
                        result,
                        animate,
                    });
                }));
            } else {
                let result = host
                    .fs_read(&path)
                    .and_then(|d| bl_image::decode_max(&d, 2560).map_err(|e| e.to_string()));
                mailbox.post(Decoded {
                    key,
                    result,
                    animate,
                });
            }
        } else if let Some(id) = source.strip_prefix("srv:") {
            let rid = self.server.get(host, &bl_net::api::paths::image(id, 2560));
            self.requests.insert(rid, Req::BgImage(source.to_string()));
        } else if let Some(id) = source.strip_prefix(crate::vault::BG_PREFIX) {
            // Locked or not listed yet: shown once the vault opens.
            if !self.vault_load_background(host, id) {
                self.bg_pending = None;
                self.load_background(host, DEFAULT_BG);
            }
        } else if let Some(name) = source.strip_prefix("mem:") {
            if let Some(data) = self.memory_files.get(name).cloned() {
                self.decode_background_async(host, source.to_string(), data, animate);
            }
        } else if let Some(track_id) = source.strip_prefix("cover:") {
            let track_id = track_id.to_string();
            if let Some(path) = library::local_path(&track_id).map(str::to_string) {
                let mailbox = self.image_box.clone();
                let key = source.to_string();
                let job = move || {
                    let result = local_cover_bytes(&path)
                        .ok_or_else(|| "no cover".to_string())
                        .and_then(|d| {
                            std::panic::catch_unwind(|| {
                                bl_image::decode_max(&d, 1600).map_err(|e| e.to_string())
                            })
                            .unwrap_or_else(|_| Err("decoder failed".into()))
                        });
                    mailbox.post(Decoded {
                        key,
                        result,
                        animate,
                    });
                };
                if host.has_threads() {
                    host.spawn(Box::new(job));
                } else {
                    job();
                }
            } else if self.server.is_connected() {
                let rid = self
                    .server
                    .get(host, &bl_net::api::paths::track_cover(&track_id, 1024));
                self.requests.insert(rid, Req::BgImage(source.to_string()));
            } else {
                self.bg_pending = None;
                self.load_background(host, DEFAULT_BG);
            }
        } else {
            self.bg_pending = None;
        }
    }

    fn show_cover_background(&mut self, host: &mut dyn Host) {
        let source = match &self.current {
            Some(t) if t.has_cover || library::is_local(&t.id) => format!("cover:{}", t.id),
            _ => DEFAULT_BG.to_string(),
        };
        self.load_background(host, &source);
    }

    /// Ensures the background matches the background queue.
    pub(crate) fn show_queue_background(&mut self, host: &mut dyn Host) {
        match self.bg_queue.current().cloned() {
            Some(s) => self.load_background(host, &s),
            None => {
                if self.settings.cover_fallback && self.current.is_some() {
                    self.show_cover_background(host);
                } else {
                    self.load_background(host, DEFAULT_BG);
                }
            }
        }
        self.revs.backgrounds += 1;
    }

    pub(crate) fn next_background(&mut self, host: &mut dyn Host) {
        if self
            .bg_queue
            .next(self.settings.background_repeat.max_queue())
            .is_some()
        {
            self.show_queue_background(host);
            self.settings_changed();
        }
    }

    pub(crate) fn prev_background(&mut self, host: &mut dyn Host) {
        if self
            .bg_queue
            .prev(self.settings.background_repeat.max_queue())
            .is_some()
        {
            self.show_queue_background(host);
            self.settings_changed();
        }
    }

    pub(crate) fn add_backgrounds(
        &mut self,
        host: &mut dyn Host,
        sources: Vec<String>,
        make_current: bool,
    ) {
        if sources.is_empty() {
            return;
        }
        let first = self.bg_queue.push(sources, &mut self.rng);
        if make_current || self.bg_queue.current().is_none() {
            self.bg_queue.set_current(first);
        }
        self.show_queue_background(host);
        self.settings_changed();
    }

    fn poll_images(&mut self, host: &mut dyn Host, gl: &Gl) {
        for d in self.image_box.take() {
            if self.bg_pending.as_deref() != Some(d.key.as_str()) {
                continue;
            }
            self.bg_pending = None;
            match d.result {
                Ok(img) => {
                    let id = self.next_image_id;
                    self.next_image_id += 1;
                    if let Some(gfx) = &mut self.gfx {
                        gfx.vis
                            .set_image(gl, id, img.width, img.height, &img.pixels);
                    }
                    let aspect = img.width as f32 / img.height.max(1) as f32;
                    self.engine
                        .set_background(Some(ImageRef { id, aspect }), d.animate);
                    self.bg_source = Some(d.key);
                }
                Err(e) => {
                    host.log(&format!("background {}: {e}", d.key));
                    if d.key.starts_with("cover:") {
                        self.load_background(host, DEFAULT_BG);
                    } else {
                        self.toast(
                            format!("{}: {e}", self.tr("Could not load background image")),
                            true,
                        );
                    }
                }
            }
        }
        if let Some(gfx) = &mut self.gfx {
            let live = self.engine.live_images();
            gfx.vis.retain_images(gl, &live);
        }
    }

    // ---- playlists -------------------------------------------------------------

    pub(crate) fn save_local_playlists(&mut self, host: &mut dyn Host) {
        let v = Value::object().with(
            "playlists",
            Value::Array(self.local_playlists.iter().map(Playlist::to_json).collect()),
        );
        host.storage_write("playlists.json", v.to_json_pretty().as_bytes());
        self.revs.playlists += 1;
    }

    pub(crate) fn upload_image(&mut self, host: &mut dyn Host, name: &str, data: Vec<u8>) {
        let mut headers = self.server.headers();
        headers.push(("X-Filename".into(), bl_net::http::percent_encode(name)));
        headers.push(("Content-Type".into(), "application/octet-stream".into()));
        let id = host.http_request(crate::host::HttpRequest {
            method: "POST".into(),
            url: self.server.url(bl_net::api::paths::IMAGES),
            headers,
            body: data,
        });
        self.requests.insert(id, Req::Upload(name.to_string()));
    }

    // ---- local library ---------------------------------------------------------

    pub(crate) fn rescan_local(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        library::start_local_scan(
            self.settings.library_folders.clone(),
            self.library.local.clone(),
            self.library.scan.clone(),
        );
    }

    fn poll_scan(&mut self, host: &mut dyn Host) {
        let result = self
            .library
            .scan
            .lock()
            .ok()
            .and_then(|mut p| p.result.take());
        if let Some(tracks) = result {
            let n = tracks.len();
            self.library.set_local(tracks);
            host.storage_write(
                "library-local.json",
                self.library.local_cache_json().as_bytes(),
            );
            self.restore_current_track();
            self.toast(
                format!(
                    "{}: {n} {}",
                    self.tr("Library scan finished"),
                    self.tr("tracks")
                ),
                false,
            );
        }
    }

    /// Opens files handed over by the UI (file dialog, drag and drop,
    /// command line): audio is queued and played, images become
    /// backgrounds, presets are imported.
    pub(crate) fn open_paths(&mut self, host: &mut dyn Host, paths: Vec<String>, play: bool) {
        // Files picked for the encrypted storage (Android's picker hands
        // them over as copied local files).
        if std::mem::take(&mut self.vault.expect_upload) {
            self.vault_upload_paths(host, paths);
            return;
        }
        let mut audio = Vec::new();
        let mut images = Vec::new();
        for p in paths {
            let ext = extension(&p).unwrap_or_default();
            if bl_media::is_supported_extension(&ext) {
                audio.push(format!("{LOCAL_PREFIX}{p}"));
            } else if library::is_image_extension(&ext) {
                images.push(format!("{LOCAL_PREFIX}{p}"));
            } else if ext == "hfx" {
                if let Ok(d) = host.fs_read(&p) {
                    let name = std::path::Path::new(&p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.import_preset_bytes(&name, &d);
                }
            }
        }
        if !audio.is_empty() {
            if play {
                let first = self.queue.push(audio, &mut self.rng);
                self.queue.set_current(first);
                self.play_current(host, true);
            } else {
                self.enqueue(host, audio, false);
            }
        }
        if !images.is_empty() {
            self.add_backgrounds(host, images, true);
        }
    }

    /// Like [`Core::open_paths`] for files given as bytes (browser).
    pub fn open_data(&mut self, host: &mut dyn Host, name: String, data: Vec<u8>) {
        if std::mem::take(&mut self.vault.expect_upload) {
            self.vault_upload_data(host, name, data);
            return;
        }
        let ext = extension(&name).unwrap_or_default();
        if ext == "hfx" {
            let stem = name.trim_end_matches(".hfx").to_string();
            self.import_preset_bytes(&stem, &data);
            return;
        }
        let is_audio = bl_media::is_supported_extension(&ext);
        let is_image = library::is_image_extension(&ext);
        if !is_audio && !is_image {
            return;
        }
        self.memory_files.insert(name.clone(), data);
        if is_audio {
            let first = self.queue.push(vec![format!("mem:{name}")], &mut self.rng);
            self.queue.set_current(first);
            self.play_current(host, true);
        } else {
            self.add_backgrounds(host, vec![format!("mem:{name}")], true);
        }
    }

    // ---- tick and render ----------------------------------------------------------

    /// Advances everything that does not need GL: network, background jobs,
    /// playback, live analysis, settings persistence. Call regularly (every
    /// frame, and from a timer while the window is hidden).
    pub fn tick(&mut self, host: &mut dyn Host) {
        self.time = host.now();
        if !self.started {
            self.started = true;
            let files = std::mem::take(&mut self.launch.files);
            if !files.is_empty() {
                let paths = files.iter().map(|f| absolute(f)).collect();
                self.open_paths(host, paths, true);
            } else {
                self.restore_current_track();
            }
            for b in std::mem::take(&mut self.launch.backgrounds) {
                let src = if b.contains(':')
                    && !b.starts_with('/')
                    && b.len() > 2
                    && !b[1..].starts_with(":\\")
                {
                    b
                } else {
                    format!("{LOCAL_PREFIX}{}", absolute(&b))
                };
                self.bg_queue.push(vec![src], &mut self.rng);
                if self.bg_queue.current().is_none() {
                    self.bg_queue.set_position(0);
                }
            }
        }
        for (id, res) in host.http_poll() {
            self.handle_http(host, id, res);
        }
        self.poll_vault(host);
        if let Some((name, items)) = self.pending_playlist_items.clone() {
            if let Some(mut pl) = self
                .server_playlists
                .iter()
                .find(|p| p.name == name)
                .cloned()
            {
                pl.items = items;
                self.pending_playlist_items = None;
                self.update_playlist(host, &pl);
            }
        }
        self.poll_analysis(host);
        self.poll_loudness(host);
        self.poll_scan(host);
        self.poll_covers();
        if self.analysis_requested {
            self.request_analysis_for_current_with(host);
        }
        self.update_airplay(host);
        self.poll_outputs(host);
        self.player.update(host);
        for ev in self.player.take_events() {
            match ev {
                // A live AirPlay stream ends through the receiver.
                PlayerEvent::Ended if self.playing_airplay() => {}
                PlayerEvent::Ended => self.on_track_ended(host),
                PlayerEvent::Advanced { .. } => self.on_track_advanced(host),
                PlayerEvent::Reload(at) => {
                    let playing = self.player.state() == PlayState::Playing;
                    self.play_current(host, playing);
                    self.player.seek(host, at);
                }
                PlayerEvent::Error(e) => {
                    let msg = format!("{}: {e}", self.tr("Playback error"));
                    self.toast(msg, true);
                }
                PlayerEvent::Loaded { .. } => {
                    if self.live.sample_rate() != self.player.device_rate() {
                        self.live = RealtimeAnalyzer::new(
                            self.player.device_rate(),
                            &self.preset.analysis_config(),
                        );
                    }
                }
            }
        }
        self.sync_preload(host);
        // The position survives the app being closed while playing.
        if self.player.state() == PlayState::Playing && self.time - self.position_saved_at > 30.0 {
            self.position_saved_at = self.time;
            self.settings_dirty_at.get_or_insert(self.time);
        }
        if let Some(t) = self.settings_dirty_at {
            if self.time - t > 2.0 {
                self.save_settings(host);
            }
        }
        let state = self.state(host);
        let json = state.to_json();
        if json != self.last_state {
            self.last_state = json;
            self.emit(Value::object().with("type", "state").with("state", state));
        }
    }

    pub fn gl_init(&mut self, gl: &Gl, host: &mut dyn Host) -> Result<(), String> {
        let vis = Visualizer::new(gl)?;
        let batch = Batch2d::new(gl)?;
        host.log(&format!(
            "OpenGL: {} / {}",
            gl.get_string(bl_gl::VERSION),
            gl.get_string(bl_gl::RENDERER)
        ));
        self.gfx = Some(Gfx { vis, batch });
        self.bg_source = None;
        self.bg_pending = None;
        self.show_queue_background(host);
        Ok(())
    }

    /// Frees GL resources (context loss).
    pub fn gl_lost(&mut self) {
        self.gfx = None;
        self.bg_source = None;
    }

    /// Renders the visualizer into framebuffer `fbo` (size in physical
    /// pixels). `dpr` is the device pixel ratio, `dt` the frame time.
    pub fn render(
        &mut self,
        gl: &Gl,
        host: &mut dyn Host,
        fbo: u32,
        w: u32,
        h: u32,
        dpr: f32,
        dt: f64,
    ) {
        self.last_dt = dt;
        let (w, h) = (w.max(1), h.max(1));
        self.poll_images(host, gl);
        let playing = self.player.state() == PlayState::Playing;
        let position = self.player.position(host);
        // Live analysis from the samples played since the last frame.
        if playing {
            let rate = self.player.device_rate().max(1) as f64;
            let new = (((position - self.last_live_pos) * rate) as usize).min(self.live_buf.len());
            if new > 0 && position > self.last_live_pos {
                let mut tmp = vec![0.0f32; self.live_buf.len()];
                self.player.recent_samples(&mut tmp);
                self.live.push(&tmp[tmp.len() - new..]);
            }
        }
        self.last_live_pos = position;
        let mut live = [0.0f32; 3];
        for (i, v) in self.live.novelty().iter().take(3).enumerate() {
            live[i] = *v;
        }
        let k = 1.0 - (-12.0 * dt as f32).exp();
        self.menu_blur += (self.blur_target - self.menu_blur) * k;
        if (self.menu_blur - self.blur_target).abs() < 0.01 {
            self.menu_blur = self.blur_target;
        }
        let rs = self.settings.render_scale.clamp(0.25, 1.0);
        let render_size = (((w as f32) * rs) as u32, ((h as f32) * rs) as u32);
        let frame_input = FrameInput {
            dt,
            playing,
            // The effects may run ahead of (or behind) the reported position.
            position: (position + self.settings.sync_offset_ms as f64 / 1000.0).max(0.0),
            analysis: None,
            live_novelty: live,
            live_energy: self.live.energy(),
            view_aspect: w as f32 / h as f32,
            menu_blur: self.menu_blur,
        };
        let overlay = self.overlay;
        if self.gfx.is_none() {
            return;
        }
        let analysis = self.analysis.clone();
        crate::visual::update_overlay(self, overlay.rect[2]);
        let frame_input = FrameInput {
            analysis: analysis.as_deref(),
            ..frame_input
        };
        let frame = self.engine.update(&frame_input);
        let Some(gfx) = &mut self.gfx else { return };
        gfx.vis.render(
            gl,
            &frame,
            render_size,
            Target::Framebuffer(fbo, w, h),
            dpr * rs,
        );
        if overlay.alpha > 0.01
            && overlay.rect[2] > 1.0
            && self.settings.visualizer != VisualizerKind::None
        {
            let lw = w as f32 / dpr;
            let lh = h as f32 / dpr;
            gfx.batch.begin(lw, lh, dpr);
            let r = Rect::new(
                overlay.rect[0],
                overlay.rect[1],
                overlay.rect[2],
                overlay.rect[3],
            );
            let paused = !playing;
            crate::visual::draw(
                &self.settings,
                &self.spectrum,
                &self.waveform,
                &mut gfx.batch,
                r,
                overlay.alpha * if paused { 0.6 } else { 1.0 },
            );
            gfx.batch.end_to(gl, fbo, w as i32, h as i32);
        }
    }

    pub fn shutdown(&mut self, host: &mut dyn Host) {
        self.save_settings(host);
        self.player.stop(host);
    }
}

impl Repeat {
    /// Repeat mode for queue navigation (Item behaves like Queue when
    /// navigating manually).
    pub fn max_queue(self) -> Repeat {
        match self {
            Repeat::Item => Repeat::Queue,
            r => r,
        }
    }
}

pub(crate) fn file_url(path: &str) -> String {
    let p = path.replace('\\', "/");
    let mut out = String::from("file://");
    if !p.starts_with('/') {
        out.push('/');
    }
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn absolute(p: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(a) = std::fs::canonicalize(p) {
            let s = a.to_string_lossy().into_owned();
            // Strip the Windows verbatim prefix.
            return s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s);
        }
    }
    p.to_string()
}

/// Cover art for a local file: embedded picture or a cover image next to it.
pub(crate) fn local_cover_bytes(path: &str) -> Option<Vec<u8>> {
    if let Ok(f) = std::fs::File::open(path) {
        let mut r = std::io::BufReader::new(f);
        let ext = extension(path);
        if let Ok(info) = bl_media::read_info(&mut r, ext.as_deref(), true) {
            if let Some(p) = info
                .pictures
                .iter()
                .find(|p| p.kind == 3)
                .or(info.pictures.first())
            {
                return Some(p.data.clone());
            }
        }
    }
    let dir = std::path::Path::new(path).parent()?;
    let rd = std::fs::read_dir(dir).ok()?;
    let mut candidates: Vec<std::path::PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(name.as_str(), "cover" | "folder" | "front" | "album")
                && matches!(ext.as_str(), "jpg" | "jpeg" | "png")
        })
        .collect();
    candidates.sort();
    candidates.first().and_then(|p| std::fs::read(p).ok())
}

#[cfg(not(target_arch = "wasm32"))]
fn http_stream(
    url: &str,
    headers: &[(String, String)],
) -> Result<Box<dyn bl_media::Source>, String> {
    let s = bl_net::client::HttpStream::open(url, headers)?;
    Ok(Box::new(std::io::BufReader::with_capacity(128 * 1024, s)))
}

#[cfg(target_arch = "wasm32")]
fn http_stream(
    _url: &str,
    _headers: &[(String, String)],
) -> Result<Box<dyn bl_media::Source>, String> {
    Err("streaming is not available in the browser".into())
}
