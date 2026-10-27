//! The application: state, services and the per-frame loop.

use crate::host::{Button, Cursor, Event, FrameOutput, Host, HttpResponse, Key};
use crate::jobs::{decode_mono, extension, Mailbox};
use crate::library::{self, LibraryState, LOCAL_PREFIX};
use crate::player::{PlayState, Player, PlayerEvent, SourceOpener};
use crate::queue::Queue;
use crate::server::{ServerClient, ServerState};
use crate::settings::{BackgroundMode, Repeat, Settings};
use crate::view::{Dialog, Page, Toast, View};
use oc_analysis::realtime::RealtimeAnalyzer;
use oc_analysis::TrackAnalysis;
use oc_fx::{Engine, FrameInput, ImageRef, Preset, Rng};
use oc_gl::Gl;
use oc_net::api::{ImageEntry, Library, Playlist, PlaylistKind, ServerInfo, Track};
use oc_render::{Batch2d, Target, Texture, Visualizer};
use std::collections::HashMap;
use std::sync::Arc;

/// Command line / launch options.
#[derive(Debug, Clone, Default)]
pub struct LaunchArgs {
    pub files: Vec<String>,
    pub data_dir: Option<std::path::PathBuf>,
    pub window_size: Option<(u32, u32)>,
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
                "-h" | "--help" => {
                    return Err("usage: openchroma [options] [audio files...]\n\
                        \n  --server URL        connect to an OpenChroma server\
                        \n  --background PATH   add a background image (repeatable)\
                        \n  --preset NAME       activate a preset\
                        \n  --data DIR          settings/data directory\
                        \n  --size WxH          initial window size\
                        \n  --fullscreen        start in fullscreen"
                        .into())
                }
                "--server" => a.server = it.next(),
                "--background" | "--bg" => a.backgrounds.extend(it.next()),
                "--preset" => a.preset = it.next(),
                "--data" => a.data_dir = it.next().map(Into::into),
                "--fullscreen" => a.fullscreen = true,
                "--size" => {
                    let v = it.next().ok_or("--size needs a value like 1280x720")?;
                    let (w, h) = v.split_once('x').ok_or("--size needs a value like 1280x720")?;
                    a.window_size = Some((w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?));
                }
                s if s.starts_with("--") => return Err(format!("unknown option {s}")),
                _ => a.files.push(arg),
            }
        }
        Ok(a)
    }
}

/// HTTP request purposes, for dispatching responses.
#[derive(Debug, Clone)]
enum Req {
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
    TrackDownload { track_id: String, autoplay: bool, generation: u64 },
    Cover(String),
    BgImage(String),
    Upload(String),
    Rescan,
}

pub(crate) struct Gfx {
    pub vis: Visualizer,
    pub batch: Batch2d,
}

/// A decoded image delivered by a worker.
pub(crate) struct Decoded {
    pub key: String,
    pub result: Result<oc_image::Image, String>,
    pub purpose: ImagePurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImagePurpose {
    Background { animate: bool },
    Cover,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CoverState {
    Loading,
    Ready,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalysisStatus {
    None,
    Working,
    Ready,
    Failed,
}

/// Where the active preset came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetOrigin {
    Builtin,
    User,
    Server,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetEntry {
    pub name: String,
    pub origin: PresetOrigin,
}

pub struct App {
    pub settings: Settings,
    settings_dirty_at: Option<f64>,
    pub(crate) rng: Rng,
    pub(crate) gfx: Option<Gfx>,
    /// Cover textures by key with their last use time.
    pub(crate) textures: HashMap<String, (Texture, f64)>,
    pub(crate) ui: Option<oc_ui::Context>,
    pub(crate) input: oc_ui::Input,
    pub(crate) fb_size: (u32, u32),
    pub(crate) dpi: f32,
    pub(crate) scale: f32,
    pub(crate) engine: Engine,
    pub preset: Preset,
    pub(crate) preset_origin: PresetOrigin,
    pub(crate) preset_dirty: bool,
    pub(crate) preset_list: Vec<PresetEntry>,
    pub(crate) undo: Vec<Preset>,
    pub(crate) redo: Vec<Preset>,
    last_edit: (String, f64),
    pub(crate) player: Player,
    pub(crate) current: Option<Track>,
    pub(crate) play_generation: u64,
    pub(crate) queue: Queue,
    pub(crate) bg_queue: Queue,
    pub(crate) library: LibraryState,
    pub(crate) analysis: Option<Arc<TrackAnalysis>>,
    analysis_key: String,
    pub(crate) analysis_status: AnalysisStatus,
    analysis_box: Mailbox<(String, Result<Vec<u8>, String>)>,
    pub(crate) live: RealtimeAnalyzer,
    live_buf: Vec<f32>,
    last_live_pos: f64,
    pub(crate) spectrum: Vec<f32>,
    pub(crate) waveform: Vec<f32>,
    next_image_id: u64,
    pub(crate) bg_source: Option<String>,
    bg_pending: Option<String>,
    image_box: Mailbox<Decoded>,
    pub(crate) covers: HashMap<String, CoverState>,
    pub(crate) server: ServerClient,
    requests: HashMap<u64, Req>,
    pub(crate) server_playlists: Vec<Playlist>,
    pub(crate) local_playlists: Vec<Playlist>,
    pub(crate) server_images: Vec<ImageEntry>,
    pub(crate) server_presets: Vec<String>,
    pub(crate) view: View,
    pub(crate) menu_blur: f32,
    pub(crate) fps: f32,
    pub(crate) last_pointer_move: f64,
    launch: LaunchArgs,
    pub(crate) last_dt: f64,
    pub(crate) time: f64,
    pub(crate) download_progress: Option<String>,
}

const DEFAULT_BG: &str = "builtin:default";

fn font_data() -> Vec<u8> {
    include_bytes!("../../../assets/fonts/Inter-Variable.ttf").to_vec()
}

/// Procedurally generated application icon (RGBA, size × size).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5 - c) / c;
            let dy = (y as f32 + 0.5 - c) / c;
            let r = (dx * dx + dy * dy).sqrt();
            let a = ((1.0 - r) * size as f32 * 0.5).clamp(0.0, 1.0);
            let ang = dy.atan2(dx);
            let t = (ang / std::f32::consts::TAU + 0.5).fract();
            // Hue sweep from rose to violet to blue.
            let (rr, gg, bb) = hue(330.0 + t * 120.0);
            let ring = ((r - 0.62).abs() < 0.17) as u8 as f32;
            let inner = (r < 0.28) as u8 as f32;
            let k = (ring * 0.95 + inner * 0.85).min(1.0);
            let base = 0.12;
            let i = ((y * size + x) * 4) as usize;
            out[i] = ((base + (rr - base) * k) * 255.0) as u8;
            out[i + 1] = ((base + (gg - base) * k) * 255.0) as u8;
            out[i + 2] = ((base + 0.04 + (bb - base) * k) * 255.0) as u8;
            out[i + 3] = (a * 255.0) as u8;
        }
    }
    out
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

/// Default background: soft coloured light blobs on a dark gradient.
fn default_background(w: u32, h: u32, seed: u64) -> oc_image::Image {
    let mut rng = Rng::new(seed);
    let blobs: Vec<(f32, f32, f32, (f32, f32, f32))> =
        (0..6).map(|_| (rng.f32(), rng.f32(), 0.25 + rng.f32() * 0.45, hue(250.0 + rng.f32() * 140.0))).collect();
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
    oc_image::Image { width: w, height: h, pixels: px }
}

/// System fonts with CJK coverage used as fallbacks (native platforms).
fn fallback_font_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/system/fonts/NotoSansCJK-Regular.ttc",
        "/system/fonts/DroidSansFallback.ttf",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\segoeui.ttf",
    ]
}

impl App {
    pub fn new(host: &mut dyn Host, launch: LaunchArgs) -> App {
        let settings = host.storage_read(Settings::STORAGE_KEY).and_then(|d| oc_json::parse_bytes(&d).ok()).map(|v| Settings::from_json(&v)).unwrap_or_default();
        let mut rng = Rng::new(host.random_seed());
        let mut fonts = oc_ui::FontSystem::new(font_data()).expect("bundled font is valid");
        if host.has_filesystem() {
            for p in fallback_font_paths() {
                if let Ok(data) = host.fs_read(p) {
                    if fonts.add_fallback(data) {
                        break;
                    }
                }
            }
        }
        let ui = oc_ui::Context::new(fonts, oc_ui::Theme::dark(settings.accent_hue));
        let mut player = Player::new();
        player.set_volume(if settings.muted { 0.0 } else { settings.volume });
        let queue = Queue::new(settings.audio_queue.clone(), settings.audio_index, settings.shuffle, &mut rng);
        let bg_queue = Queue::new(settings.backgrounds.clone(), settings.background_index, settings.background_shuffle, &mut rng);
        let mut library = LibraryState::default();
        if let Some(d) = host.storage_read("library-local.json") {
            library.load_local_cache(&d);
        }
        let local_playlists = host
            .storage_read("playlists.json")
            .and_then(|d| oc_json::parse_bytes(&d).ok())
            .and_then(|v| v.get("playlists").as_array().map(|a| a.iter().map(Playlist::from_json).collect()))
            .unwrap_or_default();
        let preset = oc_fx::preset::builtin("Balanced", &mut rng).unwrap_or_default();
        let analysis_cfg = preset.analysis_config();
        let server_url = launch.server.clone().unwrap_or_else(|| settings.server_url.clone());
        let mut server = ServerClient::new(&server_url, &settings.server_token);
        if host.platform() == "web" && server.base.is_empty() {
            // Same origin as the page.
            server.base = String::new();
        }
        let mut app = App {
            settings,
            settings_dirty_at: None,
            engine: Engine::new(rng.next_u64()),
            rng,
            gfx: None,
            textures: HashMap::new(),
            ui: Some(ui),
            input: oc_ui::Input::default(),
            fb_size: (1, 1),
            dpi: 1.0,
            scale: 1.0,
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
            queue,
            bg_queue,
            library,
            analysis: None,
            analysis_key: String::new(),
            analysis_status: AnalysisStatus::None,
            analysis_box: Mailbox::default(),
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
            server,
            requests: HashMap::new(),
            server_playlists: Vec::new(),
            local_playlists,
            server_images: Vec::new(),
            server_presets: Vec::new(),
            view: View::default(),
            menu_blur: 0.0,
            fps: 0.0,
            last_pointer_move: 0.0,
            launch,
            last_dt: 0.0,
            time: 0.0,
            download_progress: None,
        };
        app.refresh_preset_list(host);
        let wanted = app.launch.preset.clone().unwrap_or_else(|| app.settings.preset.clone());
        app.load_preset_by_name(host, &wanted);
        app.engine.set_preset(Some(app.preset.clone()));
        if host.platform() == "web" || app.settings.use_server || app.launch.server.is_some() {
            app.connect_server(host);
        }
        if app.settings.library_folders.is_empty() {
            // Nothing to scan.
        } else if host.has_threads() {
            app.rescan_local();
        }
        app
    }

    // ---- settings -----------------------------------------------------------

    pub(crate) fn settings_changed(&mut self) {
        self.settings_dirty_at = Some(self.time);
    }

    fn sync_settings_state(&mut self) {
        self.settings.audio_queue = self.queue.items.clone();
        self.settings.audio_index = self.queue.current_index();
        self.settings.shuffle = self.queue.shuffle;
        self.settings.backgrounds = self.bg_queue.items.clone();
        self.settings.background_index = self.bg_queue.current_index();
        self.settings.background_shuffle = self.bg_queue.shuffle;
        self.settings.volume = self.player.volume().max(if self.settings.muted { self.settings.volume } else { 0.0 });
    }

    pub(crate) fn save_settings(&mut self, host: &mut dyn Host) {
        self.sync_settings_state();
        host.storage_write(Settings::STORAGE_KEY, self.settings.to_json().to_json_pretty().as_bytes());
        self.settings_dirty_at = None;
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>, error: bool) {
        let t = Toast { text: text.into(), error, until: self.time + if error { 6.0 } else { 3.5 } };
        self.view.toasts.push(t);
        if self.view.toasts.len() > 4 {
            self.view.toasts.remove(0);
        }
    }

    pub(crate) fn tr<'a>(&self, s: &'a str) -> &'a str {
        crate::strings::tr(&self.settings.language, s)
    }

    // ---- server ---------------------------------------------------------------

    pub(crate) fn connect_server(&mut self, host: &mut dyn Host) {
        if host.platform() != "web" && self.server.base.is_empty() {
            self.server.state = ServerState::Disabled;
            return;
        }
        let id = self.server.request_info(host);
        self.requests.insert(id, Req::Info);
    }

    pub(crate) fn server_login(&mut self, host: &mut dyn Host, password: &str) {
        let id = self.server.request_login(host, password);
        self.requests.insert(id, Req::Login);
    }

    pub(crate) fn refresh_server_data(&mut self, host: &mut dyn Host) {
        let reqs = [
            (oc_net::api::paths::LIBRARY, Req::Library),
            (oc_net::api::paths::PLAYLISTS, Req::Playlists),
            (oc_net::api::paths::IMAGES, Req::Images),
            (oc_net::api::paths::PRESETS, Req::Presets),
        ];
        for (p, r) in reqs {
            let id = self.server.get(host, p);
            self.requests.insert(id, r);
        }
    }

    pub(crate) fn server_rescan(&mut self, host: &mut dyn Host) {
        let id = self.server.send(host, "POST", oc_net::api::paths::RESCAN, Vec::new(), "application/json");
        self.requests.insert(id, Req::Rescan);
    }

    fn handle_http(&mut self, host: &mut dyn Host, id: u64, res: Result<HttpResponse, String>) {
        let Some(req) = self.requests.remove(&id) else { return };
        let ok_json = |r: &Result<HttpResponse, String>| -> Option<oc_json::Value> {
            match r {
                Ok(resp) if (200..300).contains(&resp.status) => oc_json::parse_bytes(&resp.body).ok(),
                _ => None,
            }
        };
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
            Req::Login => match ok_json(&res).and_then(|v| v.get("token").as_str().map(str::to_string)) {
                Some(token) => {
                    self.server.token = token.clone();
                    self.settings.server_token = token;
                    self.settings_changed();
                    self.connect_server(host);
                }
                None => {
                    self.toast(self.tr("Login failed"), true);
                }
            },
            Req::Library => {
                if let Some(v) = ok_json(&res) {
                    self.library.set_remote(Library::from_json(&v));
                    self.restore_current_track();
                }
            }
            Req::Playlists => {
                if let Some(v) = ok_json(&res) {
                    self.server_playlists = v.as_array().map(|a| a.iter().map(Playlist::from_json).collect()).unwrap_or_default();
                }
            }
            Req::PlaylistSaved => {
                let id = self.server.get(host, oc_net::api::paths::PLAYLISTS);
                self.requests.insert(id, Req::Playlists);
            }
            Req::Images => {
                if let Some(v) = ok_json(&res) {
                    self.server_images = v.as_array().map(|a| a.iter().map(ImageEntry::from_json).collect()).unwrap_or_default();
                }
            }
            Req::Presets => {
                if let Some(v) = ok_json(&res) {
                    self.server_presets = v.as_array().map(|a| a.iter().filter_map(|p| p.get("name").as_str().map(str::to_string)).collect()).unwrap_or_default();
                    self.refresh_preset_list(host);
                    if self.preset_origin == PresetOrigin::Builtin && self.settings.preset != self.preset.name {
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
                    let id = self.server.get(host, oc_net::api::paths::PRESETS);
                    self.requests.insert(id, Req::Presets);
                } else {
                    self.toast(self.tr("Saving the preset failed"), true);
                }
            }
            Req::Analysis(key) => {
                if key != self.analysis_key {
                    return;
                }
                match res {
                    Ok(r) if r.status == 200 => self.analysis_box.post((key, Ok(r.body))),
                    Ok(r) => self.analysis_box.post((key, Err(format!("HTTP {}", r.status)))),
                    Err(e) => self.analysis_box.post((key, Err(e))),
                }
            }
            Req::TrackDownload { track_id, autoplay, generation } => {
                self.download_progress = None;
                if generation != self.play_generation {
                    return;
                }
                match res {
                    Ok(r) if r.status == 200 => {
                        let ext = self.library.track(&track_id).map(|t| t.codec.clone());
                        let data = r.body;
                        let opener: SourceOpener = Box::new(move || Ok((Box::new(std::io::Cursor::new(data)) as Box<dyn oc_media::Source>, ext)));
                        self.player.load(host, opener, autoplay);
                    }
                    Ok(r) => self.toast(format!("{}: HTTP {}", self.tr("Could not load track"), r.status), true),
                    Err(e) => self.toast(format!("{}: {e}", self.tr("Could not load track")), true),
                }
            }
            Req::Cover(key) => match res {
                Ok(r) if r.status == 200 => self.decode_image_async(host, key, r.body, 256, ImagePurpose::Cover),
                _ => {
                    self.covers.insert(key, CoverState::Missing);
                }
            },
            Req::BgImage(source) => match res {
                Ok(r) if r.status == 200 => {
                    let animate = self.bg_source.is_some();
                    self.decode_image_async(host, source, r.body, 2560, ImagePurpose::Background { animate });
                }
                _ => {
                    self.toast(self.tr("Could not load background image"), true);
                    self.bg_pending = None;
                }
            },
            Req::Upload(name) => {
                if matches!(&res, Ok(r) if (200..300).contains(&r.status)) {
                    self.toast(format!("{} {name}", self.tr("Uploaded")), false);
                    let id = self.server.get(host, oc_net::api::paths::IMAGES);
                    self.requests.insert(id, Req::Images);
                } else {
                    self.toast(format!("{} {name}", self.tr("Upload failed:")), true);
                }
            }
            Req::Rescan => self.toast(self.tr("Server library rescan started"), false),
        }
    }

    // ---- presets ----------------------------------------------------------------

    pub(crate) fn refresh_preset_list(&mut self, host: &mut dyn Host) {
        let mut list: Vec<PresetEntry> = oc_fx::preset::BUILTIN.iter().map(|(n, _)| PresetEntry { name: n.to_string(), origin: PresetOrigin::Builtin }).collect();
        for key in host.storage_list("presets/") {
            if let Some(name) = key.strip_prefix("presets/").and_then(|n| n.strip_suffix(".hfx")) {
                list.push(PresetEntry { name: name.to_string(), origin: PresetOrigin::User });
            }
        }
        for name in &self.server_presets {
            list.push(PresetEntry { name: name.clone(), origin: PresetOrigin::Server });
        }
        self.preset_list = list;
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
            let id = self.server.get(host, &oc_net::api::paths::preset(name));
            self.requests.insert(id, Req::PresetGet(name.to_string()));
            return;
        }
        if let Some(p) = oc_fx::preset::builtin(name, &mut self.rng) {
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
        self.view.selected_effect = None;
        self.engine.set_preset(Some(self.preset.clone()));
        if analysis_changed {
            self.live = RealtimeAnalyzer::new(self.player.device_rate().max(8000), &self.preset.analysis_config());
            self.analysis_key.clear();
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
        self.preset_dirty = true;
        self.engine.set_preset(Some(self.preset.clone()));
        if analysis_changed {
            self.live = RealtimeAnalyzer::new(self.player.device_rate().max(8000), &self.preset.analysis_config());
            self.analysis_key.clear();
            self.request_analysis_for_current();
        }
    }

    /// Like [`App::preset_edited`] but merges rapid consecutive edits of
    /// the same field (slider drags, typing) into one undo step.
    pub(crate) fn preset_edited_coalesced(&mut self, before: Preset, key: &str) {
        let merge = self.last_edit.0 == key && self.time - self.last_edit.1 < 1.0 && !self.undo.is_empty();
        self.last_edit = (key.to_string(), self.time);
        if merge {
            let analysis_changed = before.analysis_config() != self.preset.analysis_config();
            self.redo.clear();
            self.preset_dirty = true;
            self.engine.set_preset(Some(self.preset.clone()));
            if analysis_changed {
                self.analysis_key.clear();
                self.request_analysis_for_current();
            }
        } else {
            self.preset_edited(before);
        }
    }

    pub(crate) fn undo_preset(&mut self) {
        if let Some(p) = self.undo.pop() {
            let cur = std::mem::replace(&mut self.preset, p);
            self.redo.push(cur);
            self.preset_dirty = true;
            self.engine.set_preset(Some(self.preset.clone()));
            self.analysis_key.clear();
            self.request_analysis_for_current();
        }
    }

    pub(crate) fn redo_preset(&mut self) {
        if let Some(p) = self.redo.pop() {
            let cur = std::mem::replace(&mut self.preset, p);
            self.undo.push(cur);
            self.preset_dirty = true;
            self.engine.set_preset(Some(self.preset.clone()));
            self.analysis_key.clear();
            self.request_analysis_for_current();
        }
    }

    /// Saves the preset under `name` (user storage, or the server when
    /// `to_server`).
    pub(crate) fn save_preset_as(&mut self, host: &mut dyn Host, name: &str, to_server: bool) {
        let name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect::<String>().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.preset.name = name.clone();
        let json = self.preset.to_string_pretty();
        if to_server && self.server.is_connected() {
            let id = self.server.send(host, "PUT", &oc_net::api::paths::preset(&name), json.into_bytes(), "application/json");
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
    }

    pub(crate) fn delete_preset(&mut self, host: &mut dyn Host, entry: &PresetEntry) {
        match entry.origin {
            PresetOrigin::User => host.storage_delete(&format!("presets/{}.hfx", entry.name)),
            PresetOrigin::Server => {
                let id = self.server.send(host, "DELETE", &oc_net::api::paths::preset(&entry.name), Vec::new(), "application/json");
                self.requests.insert(id, Req::PresetSaved(entry.name.clone()));
            }
            PresetOrigin::Builtin => return,
        }
        self.refresh_preset_list(host);
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

    // ---- playback ------------------------------------------------------------

    pub(crate) fn current_track_id(&self) -> Option<String> {
        self.queue.current().cloned()
    }

    /// Track metadata for a queue entry (library or a bare local path).
    pub(crate) fn track_for(&self, id: &str) -> Track {
        if let Some(t) = self.library.track(id) {
            return t.clone();
        }
        if let Some(p) = library::local_path(id) {
            return library::local_track(p, 0, 0, None);
        }
        Track { id: id.to_string(), title: id.to_string(), ..Default::default() }
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
    }

    /// Starts playing the queue's current item.
    pub(crate) fn play_current(&mut self, host: &mut dyn Host, autoplay: bool) {
        let Some(id) = self.queue.current().cloned() else {
            self.player.stop(host);
            self.current = None;
            return;
        };
        let track = self.track_for(&id);
        self.play_generation += 1;
        self.current = Some(track.clone());
        self.engine.reset_schedule();
        self.analysis = None;
        self.analysis_status = AnalysisStatus::None;
        self.live.reset();
        if let Some(path) = library::local_path(&id).map(str::to_string) {
            let ext = extension(&path);
            if host.has_threads() {
                let opener: SourceOpener = Box::new(move || {
                    let f = std::fs::File::open(&path).map_err(|e| format!("{path}: {e}"))?;
                    Ok((Box::new(std::io::BufReader::with_capacity(64 * 1024, f)) as Box<dyn oc_media::Source>, ext))
                });
                self.player.load(host, opener, autoplay);
            } else {
                match host.fs_read(&path) {
                    Ok(data) => {
                        let opener: SourceOpener = Box::new(move || Ok((Box::new(std::io::Cursor::new(data)) as Box<dyn oc_media::Source>, ext)));
                        self.player.load(host, opener, autoplay);
                    }
                    Err(e) => self.toast(e, true),
                }
            }
        } else if let Some(name) = id.strip_prefix("mem:") {
            // Files dropped into the browser are kept in memory.
            if let Some(data) = self.view.memory_files.get(name).cloned() {
                let ext = extension(name);
                let opener: SourceOpener = Box::new(move || Ok((Box::new(std::io::Cursor::new(data)) as Box<dyn oc_media::Source>, ext)));
                self.player.load(host, opener, autoplay);
            }
        } else {
            // Server track.
            let url = self.server.track_stream_url(&id);
            let ext = if track.codec.is_empty() { None } else { Some(track.codec.clone()) };
            if host.has_threads() && !host.audio_is_push() {
                let headers = self.server.headers();
                let opener: SourceOpener = Box::new(move || {
                    let s = oc_net_stream(&url, &headers)?;
                    Ok((s, ext))
                });
                self.player.load(host, opener, autoplay);
            } else {
                self.player.stop(host);
                let req = crate::host::HttpRequest { method: "GET".into(), url, headers: self.server.headers(), body: Vec::new() };
                let rid = host.http_request(req);
                self.requests.insert(rid, Req::TrackDownload { track_id: id.clone(), autoplay, generation: self.play_generation });
                self.download_progress = Some(track.title.clone());
            }
        }
        self.request_analysis_for_current_with(host);
        if self.settings.cover_fallback && self.bg_queue.is_empty() {
            self.show_cover_background(host);
        }
        let title = format!("{} – {}", track.title, track.artist_line());
        host.set_title(&format!("{title} · OpenChroma"));
        self.settings_changed();
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

    pub(crate) fn enqueue(&mut self, host: &mut dyn Host, ids: Vec<String>) {
        let n = ids.len();
        let was_empty = self.queue.is_empty();
        self.queue.push(ids, &mut self.rng);
        if was_empty {
            self.queue.set_position(0);
            self.play_current(host, true);
        }
        self.toast(format!("{n} {}", self.tr("added to the queue")), false);
        self.settings_changed();
    }

    pub(crate) fn next_track(&mut self, host: &mut dyn Host, user: bool) {
        let repeat = if user && self.settings.repeat == Repeat::Item { Repeat::Queue } else { self.settings.repeat };
        let repeat = if repeat == Repeat::Item { Repeat::None } else { repeat };
        if self.queue.next(repeat).is_some() {
            self.play_current(host, true);
            if self.settings.background_mode == BackgroundMode::OnTrackChange && self.settings.background_repeat != Repeat::Item {
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
        let repeat = if self.settings.repeat == Repeat::Queue { Repeat::Queue } else { Repeat::None };
        if self.queue.prev(repeat).is_some() {
            self.play_current(host, true);
            if self.settings.background_mode == BackgroundMode::OnTrackChange && self.settings.background_repeat != Repeat::Item {
                self.prev_background(host);
            }
        }
    }

    pub(crate) fn toggle_play(&mut self, host: &mut dyn Host) {
        match self.player.state() {
            PlayState::Stopped => {
                if self.queue.current().is_none() && !self.queue.is_empty() {
                    self.queue.set_position(0);
                }
                if self.queue.current().is_some() {
                    self.play_current(host, true);
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
                BackgroundMode::OnTrackChange if self.settings.background_repeat != Repeat::Item => self.next_background(host),
                _ => {}
            }
        }
    }

    pub(crate) fn set_volume(&mut self, v: f32) {
        self.settings.volume = v.clamp(0.0, 1.0);
        self.settings.muted = false;
        self.player.set_volume(self.settings.volume);
        self.settings_changed();
    }

    pub(crate) fn toggle_mute(&mut self) {
        self.settings.muted = !self.settings.muted;
        self.player.set_volume(if self.settings.muted { 0.0 } else { self.settings.volume });
        self.settings_changed();
    }

    // ---- analysis --------------------------------------------------------------

    fn request_analysis_for_current(&mut self) {
        // Deferred: the next frame calls the host-aware variant.
        self.analysis_key.clear();
        self.view.analysis_requested = true;
    }

    fn request_analysis_for_current_with(&mut self, host: &mut dyn Host) {
        self.view.analysis_requested = false;
        let Some(track) = self.current.clone() else { return };
        let cfg = self.preset.analysis_config();
        let key = format!("{}|{}|{}|{:016x}", track.id, track.size, track.modified, cfg.cache_key());
        if key == self.analysis_key {
            return;
        }
        self.analysis_key = key.clone();
        self.analysis = None;
        let storage_key = format!("analysis/{:016x}.ocan", library::fnv(&key));
        if let Some(blob) = host.storage_read(&storage_key) {
            if let Some(a) = oc_analysis::decode(&blob) {
                self.analysis = Some(Arc::new(a));
                self.analysis_status = AnalysisStatus::Ready;
                return;
            }
        }
        self.analysis_status = AnalysisStatus::Working;
        if let Some(path) = library::local_path(&track.id).map(str::to_string) {
            let mailbox = self.analysis_box.clone();
            let ext = extension(&path);
            let job_key = key.clone();
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    let res = std::fs::File::open(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|f| decode_mono(Box::new(std::io::BufReader::new(f)), ext.as_deref()))
                        .and_then(|(mono, sr)| oc_analysis::analyze(&mono, sr, &cfg, &mut |_| true).ok_or_else(|| "cancelled".to_string()))
                        .map(|a| oc_analysis::encode(&a));
                    mailbox.post((job_key, res));
                }));
            } else {
                let res = host
                    .fs_read(&path)
                    .and_then(|d| decode_mono(Box::new(std::io::Cursor::new(d)), ext.as_deref()))
                    .and_then(|(mono, sr)| oc_analysis::analyze(&mono, sr, &cfg, &mut |_| true).ok_or_else(|| "cancelled".to_string()))
                    .map(|a| oc_analysis::encode(&a));
                mailbox.post((job_key, res));
            }
        } else if let Some(name) = track.id.strip_prefix("mem:") {
            if let Some(data) = self.view.memory_files.get(name).cloned() {
                let ext = extension(name);
                let res = decode_mono(Box::new(std::io::Cursor::new(data)), ext.as_deref())
                    .and_then(|(mono, sr)| oc_analysis::analyze(&mono, sr, &cfg, &mut |_| true).ok_or_else(|| "cancelled".to_string()))
                    .map(|a| oc_analysis::encode(&a));
                self.analysis_box.post((key, res));
            }
        } else if self.server.is_connected() {
            let cfg_json = oc_json::Value::object().with("analysisConfig", self.preset.analysis.clone()).with("noveltyConfigs", self.preset.novelty.clone()).to_json();
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
            match res.ok().and_then(|blob| oc_analysis::decode(&blob).map(|a| (a, blob))) {
                Some((a, blob)) => {
                    host.storage_write(&format!("analysis/{:016x}.ocan", library::fnv(&key)), &blob);
                    self.analysis = Some(Arc::new(a));
                    self.analysis_status = AnalysisStatus::Ready;
                }
                None => self.analysis_status = AnalysisStatus::Failed,
            }
        }
    }

    // ---- images ------------------------------------------------------------

    fn decode_image_async(&mut self, host: &mut dyn Host, key: String, data: Vec<u8>, max_dim: u32, purpose: ImagePurpose) {
        let mailbox = self.image_box.clone();
        host.spawn(Box::new(move || {
            let result = std::panic::catch_unwind(|| oc_image::decode_max(&data, max_dim).map_err(|e| e.to_string()))
                .unwrap_or_else(|_| Err("image decoder failed".to_string()));
            mailbox.post(Decoded { key, result, purpose });
        }));
    }

    /// Loads a background from a source string: `file:<path>`,
    /// `srv:<image id>`, `mem:<name>`, `cover:<track id>` or the built-in default.
    pub(crate) fn load_background(&mut self, host: &mut dyn Host, source: &str) {
        if self.bg_pending.as_deref() == Some(source) || (self.bg_source.as_deref() == Some(source) && self.bg_pending.is_none()) {
            return;
        }
        self.bg_pending = Some(source.to_string());
        let animate = self.bg_source.is_some();
        if source == DEFAULT_BG {
            let seed = self.rng.next_u64();
            let img = default_background(960, 540, seed);
            self.image_box.post(Decoded { key: source.to_string(), result: Ok(img), purpose: ImagePurpose::Background { animate } });
        } else if let Some(path) = source.strip_prefix(LOCAL_PREFIX) {
            let mailbox = self.image_box.clone();
            let (path, key) = (path.to_string(), source.to_string());
            if host.has_threads() {
                host.spawn(Box::new(move || {
                    let result = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|d| {
                        std::panic::catch_unwind(|| oc_image::decode_max(&d, 2560).map_err(|e| e.to_string())).unwrap_or_else(|_| Err("image decoder failed".into()))
                    });
                    mailbox.post(Decoded { key, result, purpose: ImagePurpose::Background { animate } });
                }));
            } else {
                let result = host.fs_read(&path).and_then(|d| oc_image::decode_max(&d, 2560).map_err(|e| e.to_string()));
                mailbox.post(Decoded { key, result, purpose: ImagePurpose::Background { animate } });
            }
        } else if let Some(id) = source.strip_prefix("srv:") {
            let rid = self.server.get(host, &oc_net::api::paths::image(id, 2560));
            self.requests.insert(rid, Req::BgImage(source.to_string()));
        } else if let Some(name) = source.strip_prefix("mem:") {
            if let Some(data) = self.view.memory_files.get(name).cloned() {
                self.decode_image_async(host, source.to_string(), data, 2560, ImagePurpose::Background { animate });
            }
        } else if let Some(track_id) = source.strip_prefix("cover:") {
            let track_id = track_id.to_string();
            if let Some(path) = library::local_path(&track_id).map(str::to_string) {
                let mailbox = self.image_box.clone();
                let key = source.to_string();
                let job = move || {
                    let result = local_cover_bytes(&path)
                        .ok_or_else(|| "no cover".to_string())
                        .and_then(|d| std::panic::catch_unwind(|| oc_image::decode_max(&d, 1600).map_err(|e| e.to_string())).unwrap_or_else(|_| Err("decoder failed".into())));
                    mailbox.post(Decoded { key, result, purpose: ImagePurpose::Background { animate } });
                };
                if host.has_threads() {
                    host.spawn(Box::new(job));
                } else {
                    job();
                }
            } else if self.server.is_connected() {
                let rid = self.server.get(host, &oc_net::api::paths::track_cover(&track_id, 1024));
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
    }

    pub(crate) fn next_background(&mut self, host: &mut dyn Host) {
        if self.bg_queue.next(self.settings.background_repeat.max_queue()).is_some() {
            self.show_queue_background(host);
            self.settings_changed();
        }
    }

    pub(crate) fn prev_background(&mut self, host: &mut dyn Host) {
        if self.bg_queue.prev(self.settings.background_repeat.max_queue()).is_some() {
            self.show_queue_background(host);
            self.settings_changed();
        }
    }

    pub(crate) fn add_backgrounds(&mut self, host: &mut dyn Host, sources: Vec<String>, make_current: bool) {
        if sources.is_empty() {
            return;
        }
        let first = self.bg_queue.push(sources, &mut self.rng);
        if make_current || self.bg_queue.current().is_none() {
            self.bg_queue.set_current(first);
            self.show_queue_background(host);
        }
        self.settings_changed();
    }

    /// Requests a cover thumbnail (track or album id) for list display.
    pub(crate) fn request_cover(&mut self, host: &mut dyn Host, key: &str, track_id: &str) {
        if self.covers.contains_key(key) {
            return;
        }
        self.covers.insert(key.to_string(), CoverState::Loading);
        if let Some(path) = library::local_path(track_id).map(str::to_string) {
            let mailbox = self.image_box.clone();
            let key = key.to_string();
            let job = move || {
                let result = local_cover_bytes(&path)
                    .ok_or_else(|| "no cover".to_string())
                    .and_then(|d| std::panic::catch_unwind(|| oc_image::decode_max(&d, 256).map_err(|e| e.to_string())).unwrap_or_else(|_| Err("decoder failed".into())));
                mailbox.post(Decoded { key, result, purpose: ImagePurpose::Cover });
            };
            if host.has_threads() {
                host.spawn(Box::new(job));
            } else {
                job();
            }
        } else if self.server.is_connected() {
            let path = if let Some(album) = key.strip_prefix("album:") {
                format!("/api/albums/{album}/cover?size=256")
            } else {
                oc_net::api::paths::track_cover(track_id, 256)
            };
            let rid = self.server.get(host, &path);
            self.requests.insert(rid, Req::Cover(key.to_string()));
        } else {
            self.covers.insert(key.to_string(), CoverState::Missing);
        }
    }

    /// Requests a thumbnail of a server background image.
    pub(crate) fn request_image_thumb(&mut self, host: &mut dyn Host, key: &str, image_id: &str) {
        if self.covers.contains_key(key) || !self.server.is_connected() {
            return;
        }
        self.covers.insert(key.to_string(), CoverState::Loading);
        let rid = self.server.get(host, &oc_net::api::paths::image(image_id, 384));
        self.requests.insert(rid, Req::Cover(key.to_string()));
    }

    /// GL texture of a loaded cover.
    pub(crate) fn cover_texture(&mut self, key: &str) -> Option<(u32, u32, u32)> {
        let time = self.time;
        let (t, last) = self.textures.get_mut(key)?;
        *last = time;
        Some((t.id, t.width, t.height))
    }

    fn poll_images(&mut self, host: &mut dyn Host, gl: &Gl) {
        for d in self.image_box.take() {
            match d.purpose {
                ImagePurpose::Background { animate } => {
                    if self.bg_pending.as_deref() != Some(d.key.as_str()) {
                        continue;
                    }
                    self.bg_pending = None;
                    match d.result {
                        Ok(img) => {
                            let id = self.next_image_id;
                            self.next_image_id += 1;
                            if let Some(gfx) = &mut self.gfx {
                                gfx.vis.set_image(gl, id, img.width, img.height, &img.pixels);
                            }
                            let aspect = img.width as f32 / img.height.max(1) as f32;
                            self.engine.set_background(Some(ImageRef { id, aspect }), animate);
                            self.bg_source = Some(d.key);
                        }
                        Err(e) => {
                            host.log(&format!("background {}: {e}", d.key));
                            if d.key.starts_with("cover:") {
                                self.load_background(host, DEFAULT_BG);
                            } else {
                                self.toast(format!("{}: {e}", self.tr("Could not load background image")), true);
                            }
                        }
                    }
                }
                ImagePurpose::Cover => match d.result {
                    Ok(img) => {
                        let tex = Texture::from_rgba(gl, img.width, img.height, &img.pixels, true);
                        if let Some((old, _)) = self.textures.insert(d.key.clone(), (tex, self.time)) {
                            old.delete(gl);
                        }
                        self.covers.insert(d.key, CoverState::Ready);
                    }
                    Err(_) => {
                        self.covers.insert(d.key, CoverState::Missing);
                    }
                },
            }
        }
        // Evict covers not used for a while (keeps GPU memory bounded).
        if self.textures.len() > 300 {
            let mut entries: Vec<(String, f64)> = self.textures.iter().map(|(k, (_, t))| (k.clone(), *t)).collect();
            entries.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in entries.iter().take(self.textures.len() - 250) {
                if let Some((t, _)) = self.textures.remove(k) {
                    t.delete(gl);
                }
                self.covers.remove(k);
            }
        }
        if let Some(gfx) = &mut self.gfx {
            let live = self.engine.live_images();
            gfx.vis.retain_images(gl, &live);
        }
    }

    // ---- playlists -------------------------------------------------------------

    pub(crate) fn save_local_playlists(&self, host: &mut dyn Host) {
        let v = oc_json::Value::object().with("playlists", oc_json::Value::Array(self.local_playlists.iter().map(Playlist::to_json).collect()));
        host.storage_write("playlists.json", v.to_json_pretty().as_bytes());
    }

    pub(crate) fn create_playlist(&mut self, host: &mut dyn Host, name: &str, kind: PlaylistKind, items: Vec<String>, on_server: bool) {
        if on_server && self.server.is_connected() {
            let body = oc_json::Value::object().with("name", name).with("kind", kind.name()).to_json().into_bytes();
            // Create, then (if items) update via a second request once we know the id.
            let id = self.server.send(host, "POST", oc_net::api::paths::PLAYLISTS, body, "application/json");
            self.requests.insert(id, Req::PlaylistSaved);
            if !items.is_empty() {
                self.view.pending_playlist_items = Some((name.to_string(), items));
            }
        } else {
            let id = format!("local-{:016x}", self.rng.next_u64());
            self.local_playlists.push(Playlist { id, name: name.to_string(), kind, items, modified: host.unix_time() as u64 });
            self.save_local_playlists(host);
        }
    }

    pub(crate) fn update_playlist(&mut self, host: &mut dyn Host, pl: &Playlist) {
        if pl.id.starts_with("local-") {
            if let Some(p) = self.local_playlists.iter_mut().find(|p| p.id == pl.id) {
                *p = pl.clone();
            }
            self.save_local_playlists(host);
        } else {
            let body = oc_json::Value::object().with("name", pl.name.as_str()).with("items", pl.items.clone()).to_json().into_bytes();
            let id = self.server.send(host, "PUT", &oc_net::api::paths::playlist(&pl.id), body, "application/json");
            self.requests.insert(id, Req::PlaylistSaved);
            if let Some(p) = self.server_playlists.iter_mut().find(|p| p.id == pl.id) {
                *p = pl.clone();
            }
        }
    }

    pub(crate) fn delete_playlist(&mut self, host: &mut dyn Host, id: &str) {
        if id.starts_with("local-") {
            self.local_playlists.retain(|p| p.id != id);
            self.save_local_playlists(host);
        } else {
            let rid = self.server.send(host, "DELETE", &oc_net::api::paths::playlist(id), Vec::new(), "application/json");
            self.requests.insert(rid, Req::PlaylistSaved);
            self.server_playlists.retain(|p| p.id != id);
        }
    }

    pub(crate) fn upload_image(&mut self, host: &mut dyn Host, name: &str, data: Vec<u8>) {
        let mut headers = self.server.headers();
        headers.push(("X-Filename".into(), oc_net::http::percent_encode(name)));
        headers.push(("Content-Type".into(), "application/octet-stream".into()));
        let id = host.http_request(crate::host::HttpRequest { method: "POST".into(), url: self.server.url(oc_net::api::paths::IMAGES), headers, body: data });
        self.requests.insert(id, Req::Upload(name.to_string()));
    }

    // ---- local library ----------------------------------------------------------

    pub(crate) fn rescan_local(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        library::start_local_scan(self.settings.library_folders.clone(), self.library.local.clone(), self.library.scan.clone());
    }

    fn poll_scan(&mut self, host: &mut dyn Host) {
        let result = self.library.scan.lock().ok().and_then(|mut p| p.result.take());
        if let Some(tracks) = result {
            let n = tracks.len();
            self.library.set_local(tracks);
            host.storage_write("library-local.json", self.library.local_cache_json().as_bytes());
            self.restore_current_track();
            self.toast(format!("{}: {n} {}", self.tr("Library scan finished"), self.tr("tracks")), false);
        }
    }

    // ---- GL ----------------------------------------------------------------------

    pub fn gl_init(&mut self, gl: &Gl, host: &mut dyn Host) -> Result<(), String> {
        let vis = Visualizer::new(gl)?;
        let batch = Batch2d::new(gl)?;
        host.log(&format!("OpenGL: {} / {}", gl.get_string(oc_gl::VERSION), gl.get_string(oc_gl::RENDERER)));
        self.gfx = Some(Gfx { vis, batch });
        self.textures.clear();
        self.covers.clear();
        // Initial background.
        for b in self.launch.backgrounds.clone() {
            let src = if b.contains(':') && !b.starts_with('/') && b.len() > 2 && !b[1..].starts_with(":\\") { b } else { format!("{LOCAL_PREFIX}{b}") };
            self.bg_queue.push(vec![src], &mut self.rng);
        }
        if !self.launch.backgrounds.is_empty() && self.bg_queue.current().is_none() {
            self.bg_queue.set_position(0);
        }
        self.bg_source = None;
        self.bg_pending = None;
        self.show_queue_background(host);
        // Launch files.
        let files = std::mem::take(&mut self.launch.files);
        if !files.is_empty() {
            let ids: Vec<String> = files.iter().map(|f| format!("{LOCAL_PREFIX}{}", absolute(f))).collect();
            self.play_tracks(host, ids, 0);
        } else {
            self.restore_current_track();
        }
        if self.launch.fullscreen {
            host.set_fullscreen(true);
        }
        Ok(())
    }

    /// Frees GL resources (context loss on Android).
    pub fn gl_lost(&mut self) {
        self.gfx = None;
        self.textures.clear();
        self.covers.clear();
        self.bg_source = None;
    }

    // ---- events ----------------------------------------------------------------------

    pub fn event(&mut self, ev: Event, host: &mut dyn Host) {
        let s = self.scale.max(0.1);
        let inp = &mut self.input;
        match ev {
            Event::PointerMove { x, y } => {
                let p = (x / s, y / s);
                if let Some(old) = inp.pointer {
                    inp.drag_delta.0 += p.0 - old.0;
                    inp.drag_delta.1 += p.1 - old.1;
                }
                inp.pointer = Some(p);
                inp.touch = false;
                self.last_pointer_move = self.time;
            }
            Event::PointerDown { x, y, button } => {
                let b = match button {
                    Button::Left => 0,
                    Button::Right => 1,
                    Button::Middle => 2,
                };
                inp.pointer = Some((x / s, y / s));
                inp.down[b] = true;
                inp.pressed[b] = true;
                if b == 0 {
                    if self.time - inp.press_time < 0.35 && ((x / s - inp.press_pos.0).abs() + (y / s - inp.press_pos.1).abs()) < 8.0 {
                        inp.double_click = true;
                    }
                    inp.press_pos = (x / s, y / s);
                    inp.press_time = self.time;
                }
                inp.touch = false;
                self.last_pointer_move = self.time;
            }
            Event::PointerUp { x, y, button } => {
                let b = match button {
                    Button::Left => 0,
                    Button::Right => 1,
                    Button::Middle => 2,
                };
                inp.pointer = Some((x / s, y / s));
                inp.down[b] = false;
                inp.released[b] = true;
            }
            Event::PointerLeave => {
                if !inp.down.iter().any(|&d| d) {
                    inp.pointer = None;
                }
                self.last_pointer_move = -100.0;
            }
            Event::Wheel { dx, dy } => {
                inp.scroll.0 += dx * 48.0;
                inp.scroll.1 += dy * 48.0;
                self.last_pointer_move = self.time;
            }
            Event::WheelPixels { dx, dy } => {
                inp.scroll.0 += dx / s;
                inp.scroll.1 += dy / s;
                self.last_pointer_move = self.time;
            }
            Event::Touch { id: _, phase, x, y } => {
                let p = (x / s, y / s);
                match phase {
                    crate::host::TouchPhase::Start => {
                        inp.pointer = Some(p);
                        inp.down[0] = true;
                        inp.pressed[0] = true;
                        inp.press_pos = p;
                        inp.press_time = self.time;
                    }
                    crate::host::TouchPhase::Move => {
                        if let Some(old) = inp.pointer {
                            inp.drag_delta.0 += p.0 - old.0;
                            inp.drag_delta.1 += p.1 - old.1;
                        }
                        inp.pointer = Some(p);
                    }
                    crate::host::TouchPhase::End | crate::host::TouchPhase::Cancel => {
                        inp.pointer = Some(p);
                        inp.down[0] = false;
                        inp.released[0] = phase == crate::host::TouchPhase::End;
                        self.view.touch_release_clear = true;
                    }
                }
                inp.touch = true;
                self.last_pointer_move = self.time;
            }
            Event::Key { key, pressed, mods, .. } => {
                inp.mods = oc_ui::Mods { shift: mods.shift, ctrl: mods.ctrl || mods.meta, alt: mods.alt };
                if pressed {
                    let ui_key = match key {
                        Key::Escape => Some(oc_ui::Key::Escape),
                        Key::Enter => Some(oc_ui::Key::Enter),
                        Key::Tab => Some(oc_ui::Key::Tab),
                        Key::Backspace => Some(oc_ui::Key::Backspace),
                        Key::Delete => Some(oc_ui::Key::Delete),
                        Key::Left => Some(oc_ui::Key::Left),
                        Key::Right => Some(oc_ui::Key::Right),
                        Key::Up => Some(oc_ui::Key::Up),
                        Key::Down => Some(oc_ui::Key::Down),
                        Key::Home => Some(oc_ui::Key::Home),
                        Key::End => Some(oc_ui::Key::End),
                        Key::PageUp => Some(oc_ui::Key::PageUp),
                        Key::PageDown => Some(oc_ui::Key::PageDown),
                        Key::Space => Some(oc_ui::Key::Space),
                        Key::Char('A') => Some(oc_ui::Key::A),
                        Key::Char('C') => Some(oc_ui::Key::C),
                        Key::Char('V') => Some(oc_ui::Key::V),
                        Key::Char('X') => Some(oc_ui::Key::X),
                        Key::Char('Z') => Some(oc_ui::Key::Z),
                        Key::Char('Y') => Some(oc_ui::Key::Y),
                        _ => None,
                    };
                    if let Some(k) = ui_key {
                        inp.keys.push(oc_ui::KeyPress { key: k, mods: inp.mods });
                    }
                    if key == Key::Char('V') && (mods.ctrl || mods.meta) {
                        inp.paste = host.clipboard_get();
                    }
                    self.view.pending_keys.push((key, mods));
                }
            }
            Event::Text(t) => inp.text.push_str(&t),
            Event::Focus(_) => {}
            Event::DropFiles(paths) => self.handle_dropped_paths(host, paths),
            Event::DropData(files) => self.handle_dropped_data(host, files),
            Event::Suspend => self.save_settings(host),
            Event::Resume => {}
            Event::CloseRequested => {
                self.save_settings(host);
                host.quit();
            }
        }
    }

    fn handle_dropped_paths(&mut self, host: &mut dyn Host, paths: Vec<String>) {
        let mut audio = Vec::new();
        let mut images = Vec::new();
        for p in paths {
            let ext = extension(&p).unwrap_or_default();
            if oc_media::is_supported_extension(&ext) {
                audio.push(format!("{LOCAL_PREFIX}{p}"));
            } else if library::is_image_extension(&ext) {
                images.push(format!("{LOCAL_PREFIX}{p}"));
            } else if ext == "hfx" {
                if let Ok(d) = host.fs_read(&p) {
                    let name = std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    self.import_preset_bytes(&name, &d);
                }
            }
        }
        if !audio.is_empty() {
            let first = self.queue.push(audio, &mut self.rng);
            self.queue.set_current(first);
            self.play_current(host, true);
        }
        if !images.is_empty() {
            self.add_backgrounds(host, images, true);
        }
    }

    fn handle_dropped_data(&mut self, host: &mut dyn Host, files: Vec<(String, Vec<u8>)>) {
        let mut audio = Vec::new();
        let mut images = Vec::new();
        for (name, data) in files {
            let ext = extension(&name).unwrap_or_default();
            if ext == "hfx" {
                let stem = name.trim_end_matches(".hfx").to_string();
                self.import_preset_bytes(&stem, &data);
                continue;
            }
            if library::is_image_extension(&ext) && self.view.upload_next_drop && self.server.is_connected() {
                self.upload_image(host, &name, data);
                continue;
            }
            let is_audio = oc_media::is_supported_extension(&ext);
            let is_image = library::is_image_extension(&ext);
            if !is_audio && !is_image {
                continue;
            }
            self.view.memory_files.insert(name.clone(), data);
            if is_audio {
                audio.push(format!("mem:{name}"));
            } else {
                images.push(format!("mem:{name}"));
            }
        }
        self.view.upload_next_drop = false;
        if !audio.is_empty() {
            let first = self.queue.push(audio, &mut self.rng);
            self.queue.set_current(first);
            self.play_current(host, true);
        }
        if !images.is_empty() {
            self.add_backgrounds(host, images, true);
        }
    }

    fn handle_shortcuts(&mut self, host: &mut dyn Host) {
        let keys = std::mem::take(&mut self.view.pending_keys);
        let typing = self.view.text_input_active;
        for (key, m) in keys {
            let ctrl = m.ctrl || m.meta;
            match key {
                Key::Escape => {
                    if self.view.dialog.is_some() {
                        self.view.dialog = None;
                    } else if self.view.popup_open || typing {
                    } else {
                        self.view.menu_open = !self.view.menu_open;
                    }
                }
                Key::Back => {
                    if self.view.dialog.is_some() {
                        self.view.dialog = None;
                    } else if self.view.menu_open {
                        if !self.view.go_back() {
                            self.view.menu_open = false;
                        }
                    } else {
                        host.quit();
                    }
                }
                Key::Space if !typing => self.toggle_play(host),
                Key::MediaPlayPause => self.toggle_play(host),
                Key::MediaNext => self.next_track(host, true),
                Key::MediaPrev => self.prev_track(host),
                Key::MediaStop => self.player.stop(host),
                Key::VolumeUp => self.set_volume(self.settings.volume + 0.05),
                Key::VolumeDown => self.set_volume(self.settings.volume - 0.05),
                Key::VolumeMute => self.toggle_mute(),
                Key::Right if ctrl && m.alt => self.next_background(host),
                Key::Left if ctrl && m.alt => self.prev_background(host),
                Key::Right if ctrl => self.next_track(host, true),
                Key::Left if ctrl => self.prev_track(host),
                Key::Right if !typing => {
                    let p = self.player.position(host);
                    self.player.seek(host, p + 5.0);
                }
                Key::Left if !typing => {
                    let p = self.player.position(host);
                    self.player.seek(host, (p - 5.0).max(0.0));
                }
                Key::Up if ctrl => self.set_volume(self.settings.volume + 0.05),
                Key::Down if ctrl => self.set_volume(self.settings.volume - 0.05),
                Key::Enter if m.alt && ctrl => self.view.theater = !self.view.theater,
                Key::Enter if m.alt => {
                    let f = host.is_fullscreen();
                    host.set_fullscreen(!f);
                }
                Key::F(11) => {
                    let f = host.is_fullscreen();
                    host.set_fullscreen(!f);
                }
                Key::Char('P') if m.alt => {
                    self.settings.overlay_pinned = !self.settings.overlay_pinned;
                    self.settings_changed();
                }
                Key::Char('S') if ctrl && m.alt => {
                    let s = !self.bg_queue.shuffle;
                    self.bg_queue.set_shuffle(s, &mut self.rng);
                    self.settings_changed();
                }
                Key::Char('R') if ctrl && m.alt => {
                    self.settings.background_repeat = self.settings.background_repeat.next();
                    self.settings_changed();
                }
                Key::Char('S') if ctrl && !typing => {
                    let s = !self.queue.shuffle;
                    self.queue.set_shuffle(s, &mut self.rng);
                    self.settings_changed();
                }
                Key::Char('R') if ctrl && !typing => {
                    self.settings.repeat = self.settings.repeat.next();
                    self.settings_changed();
                }
                Key::Char('F') if ctrl => {
                    self.view.menu_open = true;
                    self.view.page = Page::Library;
                    self.view.focus_search = true;
                }
                Key::Char('S') if m.alt && !typing => {
                    if m.shift || self.preset_origin == PresetOrigin::Builtin {
                        self.view.dialog = Some(Dialog::SavePresetAs(self.preset.name.clone()));
                    } else {
                        let name = self.preset.name.clone();
                        let srv = self.preset_origin == PresetOrigin::Server;
                        self.save_preset_as(host, &name, srv);
                    }
                }
                Key::Char('Z') if ctrl && !typing => self.undo_preset(),
                Key::Char('Y') if ctrl && !typing => self.redo_preset(),
                Key::Equals if ctrl => {
                    self.settings.ui_zoom = (self.settings.ui_zoom + 0.05).min(2.0);
                    self.settings_changed();
                }
                Key::Minus if ctrl => {
                    self.settings.ui_zoom = (self.settings.ui_zoom - 0.05).max(0.5);
                    self.settings_changed();
                }
                Key::Char('0') if ctrl => {
                    self.settings.ui_zoom = 1.0;
                    self.settings_changed();
                }
                _ => {}
            }
        }
    }

    // ---- frame -----------------------------------------------------------------------

    /// Renders one frame. `fb_w`/`fb_h` is the drawable size in physical
    /// pixels, `dpi_scale` the platform's UI scale factor.
    pub fn frame(&mut self, gl: &Gl, host: &mut dyn Host, fb_w: u32, fb_h: u32, dpi_scale: f32, dt: f64) -> FrameOutput {
        self.time = host.now();
        self.last_dt = dt;
        if dt > 0.0 {
            self.fps = self.fps * 0.95 + (1.0 / dt as f32) * 0.05;
        }
        self.fb_size = (fb_w.max(1), fb_h.max(1));
        self.dpi = dpi_scale;
        self.scale = (dpi_scale * self.settings.ui_zoom).clamp(0.4, 6.0);
        // Network and background jobs.
        for (id, res) in host.http_poll() {
            self.handle_http(host, id, res);
        }
        if let Some((name, items)) = self.view.pending_playlist_items.clone() {
            if let Some(pl) = self.server_playlists.iter().find(|p| p.name == name).cloned() {
                let mut pl = pl;
                pl.items = items;
                self.view.pending_playlist_items = None;
                self.update_playlist(host, &pl);
            }
        }
        self.poll_analysis(host);
        self.poll_images(host, gl);
        self.poll_scan(host);
        if self.view.analysis_requested {
            self.request_analysis_for_current_with(host);
        }
        // Player.
        self.player.update(host);
        for ev in self.player.take_events() {
            match ev {
                PlayerEvent::Ended => self.on_track_ended(host),
                PlayerEvent::Error(e) => {
                    let msg = format!("{}: {e}", self.tr("Playback error"));
                    self.toast(msg, true);
                }
                PlayerEvent::Loaded { .. } => {
                    if self.live.sample_rate() != self.player.device_rate() {
                        self.live = RealtimeAnalyzer::new(self.player.device_rate(), &self.preset.analysis_config());
                    }
                }
            }
        }
        self.handle_shortcuts(host);
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
        // Menu blur animation (spring-like).
        let target_blur = if self.view.menu_open { 2.0 } else { 0.0 };
        let k = 1.0 - (-12.0 * dt as f32).exp();
        self.menu_blur += (target_blur - self.menu_blur) * k;
        if (self.menu_blur - target_blur).abs() < 0.01 {
            self.menu_blur = target_blur;
        }
        // Render the visualizer.
        let (w, h) = self.fb_size;
        let rs = self.settings.render_scale.clamp(0.25, 1.0);
        let render_size = (((w as f32) * rs) as u32, ((h as f32) * rs) as u32);
        let frame_input = FrameInput {
            dt,
            playing,
            position,
            analysis: self.analysis.as_deref(),
            live_novelty: live,
            view_aspect: w as f32 / h.max(1) as f32,
            menu_blur: self.menu_blur,
        };
        let frame = self.engine.update(&frame_input);
        if let Some(gfx) = &mut self.gfx {
            gfx.vis.render(gl, &frame, render_size, Target::Screen(w, h), self.dpi * rs);
        }
        // User interface.
        self.ui_frame(gl, host);
        // Persist settings (debounced).
        if let Some(t) = self.settings_dirty_at {
            if self.time - t > 2.0 {
                self.save_settings(host);
            }
        }
        FrameOutput { animating: true, wake_in: 0.0 }
    }

    fn ui_frame(&mut self, gl: &Gl, host: &mut dyn Host) {
        let (Some(mut gfx), Some(mut ctx)) = (self.gfx.take(), self.ui.take()) else { return };
        let logical = (self.fb_size.0 as f32 / self.scale, self.fb_size.1 as f32 / self.scale);
        ctx.fonts.set_scale(self.scale);
        ctx.theme = oc_ui::Theme::dark(self.settings.accent_hue);
        self.input.time = self.time;
        self.input.dt = self.last_dt as f32;
        gfx.batch.begin(logical.0, logical.1, self.scale);
        let input = std::mem::take(&mut self.input);
        {
            let mut ui = ctx.begin(&mut gfx.batch, &input);
            crate::screens::draw(self, &mut ui, host);
            ui.finish();
        }
        if let Some(text) = ctx.copy_out.take() {
            host.clipboard_set(&text);
        }
        host.set_text_input(ctx.text_input_active);
        self.view.text_input_active = ctx.text_input_active;
        self.view.popup_open = ctx.has_popup();
        let cursor = if self.chrome_hidden() && !self.view.menu_open {
            Cursor::Hidden
        } else if ctx.wants_text_cursor {
            Cursor::Text
        } else if ctx.wants_pointer_cursor {
            Cursor::Pointer
        } else {
            Cursor::Default
        };
        host.set_cursor(cursor);
        self.input = input;
        self.input.end_frame();
        if self.view.touch_release_clear {
            self.view.touch_release_clear = false;
            self.input.pointer = None;
        }
        ctx.fonts.flush(gl, &gfx.batch);
        let (fw, fh) = self.fb_size;
        gfx.batch.end(gl, fw as i32, fh as i32);
        self.gfx = Some(gfx);
        self.ui = Some(ctx);
        // Drop expired toasts.
        let now = self.time;
        self.view.toasts.retain(|t| t.until > now);
    }

    /// Whether the chrome (player bar etc.) is currently hidden.
    pub(crate) fn chrome_hidden(&self) -> bool {
        if self.view.menu_open || self.settings.overlay_pinned || self.view.dialog.is_some() {
            return false;
        }
        if self.view.theater {
            return true;
        }
        let idle = self.time - self.last_pointer_move > 3.0;
        idle && !self.input.down[0]
    }

    pub fn shutdown(&mut self, host: &mut dyn Host) {
        self.save_settings(host);
        self.player.stop(host);
    }

    // Accessors used by platform layers.
    pub fn is_playing(&self) -> bool {
        self.player.state() == PlayState::Playing
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
fn local_cover_bytes(path: &str) -> Option<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(f) = std::fs::File::open(path) {
            let mut r = std::io::BufReader::new(f);
            let ext = extension(path);
            if let Ok(info) = oc_media::read_info(&mut r, ext.as_deref(), true) {
                if let Some(p) = info.pictures.iter().find(|p| p.kind == 3).or(info.pictures.first()) {
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
                let name = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                let ext = p.extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                matches!(name.as_str(), "cover" | "folder" | "front" | "album") && matches!(ext.as_str(), "jpg" | "jpeg" | "png")
            })
            .collect();
        candidates.sort();
        candidates.first().and_then(|p| std::fs::read(p).ok())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn oc_net_stream(url: &str, headers: &[(String, String)]) -> Result<Box<dyn oc_media::Source>, String> {
    let s = oc_net::client::HttpStream::open(url, headers)?;
    Ok(Box::new(std::io::BufReader::with_capacity(128 * 1024, s)))
}

#[cfg(target_arch = "wasm32")]
fn oc_net_stream(_url: &str, _headers: &[(String, String)]) -> Result<Box<dyn oc_media::Source>, String> {
    Err("streaming is not available in the browser".into())
}
