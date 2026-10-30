//! Beatlumen server: serves music libraries from local folders to
//! Beatlumen clients (native apps and the web client) over HTTP.
//!
//! The server connects only to remote sources the admin configures.
//!
//! Module overview:
//! * [`config`]: command line / JSON configuration.
//! * [`library`]: library index, album grouping, incremental scanner and the
//!   index cache.
//! * [`routes`]: the HTTP API (see `README.md` for the endpoint list).
//! * [`auth`], [`playlists`], [`presets`], [`images`], [`covers`],
//!   [`analysis`], [`web`]: the individual services.
//! * [`vault`]: user accounts and end-to-end encrypted files.
//! * [`util`], [`log`], [`sha256`], [`random`]: helpers.
//!
//! [`start`] binds the listener, loads the cached index, starts the scanner
//! thread and the HTTP server and returns a [`Server`] handle.

#![deny(unsafe_code)]

pub mod log;

pub mod admin;
pub mod admin_web;
pub mod analysis;
pub mod auth;
pub mod config;
pub mod covers;
pub mod devices;
pub mod extensions;
pub mod images;
pub mod library;
pub mod lyrics;
pub mod lyrics_sync;
pub mod models;
pub mod playlists;
pub mod presets;
pub mod push;
pub mod random;
pub mod release;
pub mod routes;
pub mod sha256;
pub mod sources;
pub mod tls;
pub mod userdata;
pub mod util;
pub mod vault;
pub mod web;
pub mod webauthn;

pub use config::Config;

use library::{Entry, Index};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Concurrent thumbnail generations (bounds decoder memory use).
const MAX_THUMBNAILS: usize = 2;

/// Locations inside the data folder.
pub struct DataPaths {
    pub root: PathBuf,
    pub library: PathBuf,
    pub tokens: PathBuf,
    pub playlists: PathBuf,
    pub presets: PathBuf,
    pub uploads: PathBuf,
    pub covers: PathBuf,
    pub analysis: PathBuf,
    pub image_cache: PathBuf,
    /// Imported song texts.
    pub lyrics: PathBuf,
}

impl DataPaths {
    fn new(root: PathBuf) -> DataPaths {
        let cache = root.join("cache");
        DataPaths {
            library: root.join("library.json"),
            tokens: root.join("tokens.json"),
            playlists: root.join("playlists.json"),
            presets: root.join("presets"),
            uploads: root.join("uploads"),
            covers: cache.join("covers"),
            analysis: cache.join("analysis"),
            image_cache: cache.join("images"),
            lyrics: root.join("lyrics"),
            root,
        }
    }

    fn create(&self) -> std::io::Result<()> {
        for d in [
            &self.root,
            &self.presets,
            &self.uploads,
            &self.covers,
            &self.analysis,
            &self.image_cache,
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct ScanFlags {
    requested: bool,
    running: bool,
    last_finished: Option<Instant>,
}

/// How the server restarts itself (set by the program; tests have none).
pub type RestartHandler = Box<dyn Fn() + Send + Sync>;

/// Shared server state.
pub struct State {
    /// The configuration; settings made in the admin interface replace it.
    config: RwLock<Arc<Config>>,
    pub started: Instant,
    /// Settings changed that take effect after a restart.
    restart_pending: AtomicBool,
    restart_handler: OnceLock<RestartHandler>,
    pub admin: admin::Admin,
    pub models: Arc<models::Models>,
    pub devices: devices::Devices,
    /// Remote music sources (their songs are part of the library).
    pub sources: Arc<sources::Sources>,
    /// Playlist files in the music folders (read-only playlists).
    pub folder_playlists: RwLock<Vec<bl_net::api::Playlist>>,
    /// Absolute music folder paths (index = root index in track ids).
    pub roots: Vec<PathBuf>,
    pub data: DataPaths,
    library: RwLock<Arc<Index>>,
    cache_dirty: AtomicBool,
    scan: Mutex<ScanFlags>,
    scan_cv: Condvar,
    pub auth: auth::Auth,
    pub playlists: playlists::Playlists,
    pub presets: presets::Presets,
    pub images: images::Images,
    pub lyrics: lyrics::Lyrics,
    pub lyrics_sync: Arc<lyrics_sync::LyricsSync>,
    pub user_data: userdata::UserData,
    pub analyzer: analysis::Analyzer,
    pub thumbs: util::Semaphore,
    pub web: web::WebClient,
    pub vault: vault::Vault,
    pub extensions: extensions::Extensions,
}

impl State {
    /// The current configuration.
    pub fn config(&self) -> Arc<Config> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Takes a new configuration from the admin interface: live settings
    /// apply now, `restart` marks the others as waiting for a restart.
    pub fn apply_config(&self, cfg: Config, restart: bool) {
        log::set_verbose(cfg.verbose);
        self.vault.set_registration(cfg.registration);
        self.vault
            .set_quota(cfg.vault_quota_mb.saturating_mul(1024 * 1024));
        self.analyzer.set_max_minutes(cfg.analysis_max_minutes);
        self.lyrics_sync
            .configure(cfg.speech_model.clone(), cfg.auto_sync_lyrics);
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(cfg);
        if restart {
            self.restart_pending.store(true, Ordering::Relaxed);
        }
        // The rescan interval may have changed.
        self.scan_cv.notify_all();
    }

    pub fn restart_pending(&self) -> bool {
        self.restart_pending.load(Ordering::Relaxed)
    }

    /// Sets how the server restarts itself (see [`restart_process`]).
    pub fn set_restart_handler(&self, h: RestartHandler) {
        let _ = self.restart_handler.set(h);
    }

    pub fn can_restart(&self) -> bool {
        self.restart_handler.get().is_some()
    }

    /// Restarts the server shortly (after the answer went out); false when
    /// it cannot restart itself.
    pub fn restart(self: &Arc<Self>) -> bool {
        if !self.can_restart() {
            return false;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            me.save_cache();
            info!("restarting");
            if let Some(h) = me.restart_handler.get() {
                h();
            }
        });
        true
    }

    /// The current library snapshot.
    pub fn library(&self) -> Arc<Index> {
        self.library
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// True while a scan runs or is pending.
    pub fn scanning(&self) -> bool {
        let f = self.scan.lock().unwrap_or_else(|e| e.into_inner());
        f.running || f.requested
    }

    /// Asks the scanner thread for a rescan (coalesced).
    pub fn request_scan(&self) {
        self.scan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requested = true;
        self.scan_cv.notify_all();
    }

    /// Called when a library file vanished: rescans unless a scan finished
    /// within the last minute.
    pub fn file_missing(&self) {
        let recent = self
            .scan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_finished
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        if !recent {
            self.request_scan();
        }
    }

    pub fn info(&self) -> bl_net::api::ServerInfo {
        let idx = self.library();
        bl_net::api::ServerInfo {
            name: self.config().name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            api_version: bl_net::api::API_VERSION,
            auth_required: self.auth.required(),
            track_count: idx.entries.len() as u64,
            scanning: self.scanning(),
            revision: idx.revision,
            source_url: self.config().source_url.clone(),
            web_integrity: self.web.integrity.status.to_string(),
            passkeys: self.vault.rp.is_some(),
            speech: self.lyrics_sync.available(),
        }
    }

    /// Replaces the library when `entries` differ from it. Returns true if
    /// a new revision was published.
    fn publish(&self, entries: Vec<Entry>) -> bool {
        let cur = self.library();
        if !cur.differs(&entries) {
            return false;
        }
        let idx = Index::new(cur.revision + 1, self.roots.clone(), entries);
        *self.library.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(idx);
        self.cache_dirty.store(true, Ordering::Relaxed);
        true
    }

    fn save_cache(&self) {
        if self.cache_dirty.swap(false, Ordering::Relaxed) {
            if let Err(e) = library::save_cache(&self.data.library, &self.library()) {
                warn!(
                    "cannot write library cache {}: {e}",
                    self.data.library.display()
                );
                self.cache_dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Publishes the library again with the current songs of the remote
    /// sources (after a source was listed, changed or removed).
    pub fn sources_changed(&self) {
        let mut entries: Vec<Entry> = self
            .library()
            .entries
            .iter()
            .filter(|e| e.remote.is_none())
            .cloned()
            .collect();
        entries.extend(self.sources.entries());
        if self.publish(entries) {
            self.save_cache();
        }
    }

    fn run_scan(&self) {
        let t0 = Instant::now();
        let old = self.library();
        let old_rev = old.revision;
        let (mut entries, stats) = library::scan(
            &self.roots,
            &old,
            self.config().scan_threads,
            library::WALK_STALL,
            &mut |mut partial| {
                partial.extend(self.sources.entries());
                if self.publish(partial) {
                    debug!("published intermediate scan results");
                }
            },
        );
        drop(old);
        entries.extend(self.sources.entries());
        self.publish(entries);
        let lists = library::folder_playlists(&stats.playlists, &self.library());
        *self
            .folder_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner()) = lists;
        self.save_cache();
        let idx = self.library();
        info!(
            "scan finished in {:.1} s: {} tracks, {} albums in {} folders ({} files read, {} without readable tags, {} unreadable){}",
            t0.elapsed().as_secs_f32(),
            idx.entries.len(),
            idx.albums.len(),
            stats.dirs,
            stats.read,
            stats.tag_errors,
            stats.skipped,
            if idx.revision != old_rev { format!(", revision {}", idx.revision) } else { String::new() }
        );
        self.prune_caches(&idx);
    }

    /// Removes cached covers/analyses of tracks that no longer exist or
    /// changed, and scaled images of removed images.
    fn prune_caches(&self, idx: &Index) {
        self.extensions.prune_temp();
        let mut removed = 0usize;
        let list = |dir: &PathBuf| -> Vec<(String, PathBuf)> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
                .collect()
        };
        let stale_tmp = |p: &PathBuf| {
            std::fs::metadata(p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(3600))
        };
        for (name, path) in list(&self.data.covers) {
            let keep = match covers::parse_cache_name(&name) {
                Some((id, _, stamp)) => idx
                    .track(id)
                    .is_some_and(|e| covers::cover_stamp(idx, e) == stamp),
                None => name.ends_with(".tmp") && !stale_tmp(&path),
            };
            if !keep && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        for (name, path) in list(&self.data.analysis) {
            let keep = match name.strip_suffix(".ocan").and_then(|b| b.split_once('_')) {
                Some((id, _)) => idx.track(id).is_some_and(|e| {
                    std::fs::metadata(&path)
                        .map(|m| util::mtime_ns(&m) >= e.mtime_ns)
                        .unwrap_or(false)
                }),
                None => name.ends_with(".tmp") && !stale_tmp(&path),
            };
            if !keep && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        let images = self.images.index();
        let current: std::collections::HashMap<&str, &images::ImageItem> = images
            .items
            .iter()
            .map(|i| (i.entry.id.as_str(), i))
            .collect();
        for (name, path) in list(&self.data.image_cache) {
            let keep = match covers::parse_cache_name(&name) {
                Some((id, size, _)) => current
                    .get(id)
                    .is_some_and(|it| self.images.scaled_path(it, size) == path),
                None => name.ends_with(".tmp") && !stale_tmp(&path),
            };
            if !keep && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            debug!("removed {removed} stale cache files");
        }
    }

    fn scanner_loop(self: Arc<Self>) {
        let mut first = true;
        loop {
            {
                let mut f = self.scan.lock().unwrap_or_else(|e| e.into_inner());
                while !f.requested {
                    let interval = self.config().rescan_interval;
                    if interval == 0 {
                        f = self.scan_cv.wait(f).unwrap_or_else(|e| e.into_inner());
                        continue;
                    }
                    let iv = Duration::from_secs(interval);
                    let since = f.last_finished.map(|t| t.elapsed()).unwrap_or(iv);
                    if since >= iv {
                        f.requested = true;
                        break;
                    }
                    f = self
                        .scan_cv
                        .wait_timeout(f, iv - since)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                f.requested = false;
                f.running = true;
            }
            // Sources are listed at start anyway; later rescans list them
            // again too.
            if !first {
                self.sources.request_all();
            }
            first = false;
            let _ = util::catch("library scan", || self.run_scan());
            let mut f = self.scan.lock().unwrap_or_else(|e| e.into_inner());
            f.running = false;
            f.last_finished = Some(Instant::now());
        }
    }
}

/// A running server.
pub struct Server {
    pub addr: SocketAddr,
    pub state: Arc<State>,
    http: JoinHandle<()>,
}

impl Server {
    /// Blocks until the HTTP server stops (it normally runs forever).
    pub fn join(self) {
        let _ = self.http.join();
    }

    /// Waits until no scan is running or pending (for tests and tools).
    pub fn wait_for_scan(&self, timeout: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if !self.state.scanning() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

fn absolute(p: &std::path::Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Asks the server configured by `config` for its info page (container
/// health checks). True when it answers with 200 within three seconds.
pub fn health_check(config: &Config) -> bool {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let Some(mut addr) = config
        .bind
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        return false;
    };
    // A wildcard address is reached through the loopback interface.
    if addr.ip().is_unspecified() {
        addr = match addr {
            SocketAddr::V4(a) => SocketAddr::from(([127, 0, 0, 1], a.port())),
            SocketAddr::V6(a) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, a.port())),
        };
    }
    let timeout = Duration::from_secs(3);
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        bl_net::api::paths::INFO
    );
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    let mut n = 0;
    while n < head.len() {
        match s.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    head[..n].starts_with(b"HTTP/1.1 200")
}

/// Replaces the process with a new start of the same program and
/// arguments (Unix), or starts it anew and exits (Windows): the admin
/// interface's restart.
pub fn restart_process() {
    let Ok(exe) = std::env::current_exe() else {
        error!("cannot restart: the program's path is unknown");
        return;
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(&exe).args(&args).exec();
        error!("cannot restart: {e}");
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&exe).args(&args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => error!("cannot restart: {e}"),
        }
    }
}

/// Starts the server: prepares the data folder, loads the cached library,
/// binds the listener and starts the scanner and HTTP threads.
pub fn start(config: Config) -> Result<Server, String> {
    log::set_verbose(config.verbose);
    let data = DataPaths::new(absolute(&config.data_dir));
    data.create()
        .map_err(|e| format!("cannot create data folder {}: {e}", data.root.display()))?;
    let mut roots: Vec<PathBuf> = config.music_dirs.iter().map(|p| absolute(p)).collect();
    let extensions = extensions::Extensions::new(&config, &data.root)?;
    if extensions.enabled() {
        // Songs imported through extensions form one more music folder.
        roots.push(extensions.imports.clone());
    }
    let image_dirs: Vec<PathBuf> = config.image_dirs.iter().map(|p| absolute(p)).collect();

    let index = match library::load_cache(&data.library, &roots) {
        Some((rev, entries)) => {
            let idx = Index::new(rev, roots.clone(), entries);
            info!(
                "loaded cached library: {} tracks, revision {}",
                idx.entries.len(),
                idx.revision
            );
            idx
        }
        // Seed fresh revisions with the clock so that clients never confuse
        // a new library with one cached under the same revision number.
        None => Index::new(util::now_secs(), roots.clone(), Vec::new()),
    };

    let listener = TcpListener::bind(&config.bind)
        .map_err(|e| format!("cannot listen on {}: {e}", config.bind))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    let web = web::WebClient::resolve(config.web_dir.as_deref());
    tls::install();
    let rp = match &config.public_url {
        Some(url) => Some(webauthn::RelyingParty::new(
            url,
            &config.name,
            &config.all_android_certs(),
        )?),
        None => None,
    };
    let push = push::Push::open(
        &data.root.join("vault"),
        &config.source_url,
        config.push_allow_private,
    )?;
    let vault = vault::Vault::open(
        &data.root,
        config.vault_pepper_file.as_deref(),
        config.vault_quota_mb.saturating_mul(1024 * 1024),
        config.registration,
        push,
        rp,
    )?;
    let admin = admin::Admin::open(&data.root, config.admin_password_file.as_deref())?;
    // The songs of remote sources from their cached listings.
    let sources = Arc::new(sources::Sources::load(
        &data.root,
        vault.server_key("beatlumen source logins v1"),
    ));
    let index = if sources.entries().is_empty() {
        index
    } else {
        let mut entries: Vec<Entry> = index
            .entries
            .into_iter()
            .filter(|e| e.remote.is_none())
            .collect();
        entries.extend(sources.entries());
        Index::new(index.revision, roots.clone(), entries)
    };
    let secret = match (&config.password, admin.server_password()) {
        (Some(p), _) => Some(auth::Secret::plain(p)),
        (None, Some(v)) => Some(auth::Secret::Verifier(v)),
        (None, None) => None,
    };
    let models = Arc::new(match &config.model_source {
        Some(src) => models::Models::with_source(data.root.join("models"), src.clone()),
        None => models::Models::new(data.root.join("models")),
    });
    let state = Arc::new(State {
        auth: auth::Auth::new(secret, data.tokens.clone()),
        started: Instant::now(),
        restart_pending: AtomicBool::new(false),
        restart_handler: OnceLock::new(),
        admin,
        models,
        devices: devices::Devices::default(),
        playlists: playlists::Playlists::load(data.playlists.clone()),
        presets: presets::Presets::new(data.presets.clone()),
        images: images::Images::new(
            image_dirs.clone(),
            data.uploads.clone(),
            data.image_cache.clone(),
        ),
        lyrics: lyrics::Lyrics::new(data.lyrics.clone()),
        lyrics_sync: Arc::new(lyrics_sync::LyricsSync::new(
            config.speech_model.clone(),
            config.auto_sync_lyrics,
            data.lyrics.clone(),
        )),
        user_data: userdata::UserData::default(),
        analyzer: analysis::Analyzer::new(
            data.analysis.clone(),
            config.analysis_max_minutes,
            sources.clone(),
        ),
        sources,
        folder_playlists: RwLock::new(Vec::new()),
        thumbs: util::Semaphore::new(MAX_THUMBNAILS),
        library: RwLock::new(Arc::new(index)),
        cache_dirty: AtomicBool::new(false),
        scan: Mutex::new(ScanFlags {
            requested: true,
            ..Default::default()
        }),
        scan_cv: Condvar::new(),
        roots,
        data,
        web,
        vault,
        extensions,
        config: RwLock::new(Arc::new(config)),
    });
    state.extensions.attach(&state);
    state.lyrics_sync.attach(&state);

    info!(
        "Beatlumen server {} listening on http://{addr}",
        env!("CARGO_PKG_VERSION")
    );
    for r in &state.roots {
        info!("music folder: {}", r.display());
    }
    if state.roots.is_empty() {
        warn!("no music folders configured (use --music DIR)");
    }
    for d in &image_dirs {
        info!("image folder: {}", d.display());
    }
    info!("data folder: {}", state.data.root.display());
    info!("web client: {}", state.web.describe());
    let cfg = state.config();
    if state.auth.required() {
        info!("password protection enabled");
        if cfg
            .password
            .as_ref()
            .is_some_and(|p| p.chars().count() < 12)
        {
            warn!("the password is short: use at least 12 characters if the server is reachable from the internet");
        }
    } else if !addr.ip().is_loopback() {
        warn!(
            "no password set: anyone who can reach port {} can access the library (use --password)",
            addr.port()
        );
    }

    let weak = Arc::downgrade(&state);
    let lister = state.sources.clone();
    std::thread::Builder::new()
        .name("sources".into())
        .spawn(move || {
            lister.run(move || {
                if let Some(s) = weak.upgrade() {
                    s.sources_changed();
                }
            })
        })
        .map_err(|e| format!("cannot start the source listing: {e}"))?;

    let scanner = state.clone();
    std::thread::Builder::new()
        .name("scanner".into())
        .spawn(move || scanner.scanner_loop())
        .map_err(|e| format!("cannot start scanner: {e}"))?;

    state.admin.announce(&state.data.root, addr);

    let handler_state = state.clone();
    let handler: Arc<bl_net::server::Handler> =
        Arc::new(move |req| routes::handle(&handler_state, req));
    // Small bodies everywhere except image uploads.
    let server_config = bl_net::server::ServerConfig {
        max_body: 4 * 1024 * 1024,
        large_bodies: vec![("POST", "/api/images", images::MAX_UPLOAD + 1024 * 1024)],
        ..Default::default()
    };
    let http = std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            if let Err(e) = bl_net::server::serve(listener, handler, server_config) {
                error!("HTTP server stopped: {e}");
            }
        })
        .map_err(|e| format!("cannot start HTTP server: {e}"))?;
    Ok(Server { addr, state, http })
}
