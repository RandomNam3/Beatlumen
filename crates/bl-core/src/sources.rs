//! Music sources the app uses by itself, without a Beatlumen server:
//! Subsonic/OpenSubsonic, Jellyfin, WebDAV/Nextcloud and DLNA media
//! servers (`bl_sources`). Their songs join the library with ids
//! `src:<source>:<key>`; they play as seekable streams, so analysis,
//! seeking and covers work like for local files.
//!
//! * `sources.json` (app storage) lists the sources without credentials.
//! * The credentials are kept in the platform's protected store (Android
//!   Keystore, Windows DPAPI, the Secret Service) as one entry; platforms
//!   without one keep them in the app's private storage.
//! * `sources/<id>.json` caches each listing, so songs are there at start
//!   and unchanged files of WebDAV folders are not read again.

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::player::SourceOpener;
use bl_json::Value;
use bl_net::api::Track;
use bl_sources::{MusicSource, RemoteTrack, SourceConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(crate) const ID_PREFIX: &str = "src:";
const LIST_KEY: &str = "sources.json";
/// Key of the credentials in the protected store (and the fallback file).
const SECRET_KEY: &str = "sources";
const SECRET_FILE: &str = "sources-logins.json";
const MAX_SOURCES: usize = 16;
const KINDS: [&str; 5] = ["subsonic", "jellyfin", "webdav", "sftp", "dlna"];

#[derive(Clone)]
pub(crate) struct AppSource {
    id: String,
    name: String,
    enabled: bool,
    /// Without the secret (kept apart).
    cfg: SourceConfig,
}

#[derive(Clone, Default)]
struct Status {
    syncing: bool,
    error: String,
    synced: f64,
}

type Conns = Arc<Mutex<HashMap<String, Arc<dyn MusicSource>>>>;

#[derive(Default)]
pub(crate) struct SourcesState {
    list: Vec<AppSource>,
    secrets: HashMap<String, String>,
    /// The credentials arrived (or there is no protected store).
    ready: bool,
    loaded: bool,
    conns: Conns,
    tracks: HashMap<String, Vec<RemoteTrack>>,
    status: HashMap<String, Status>,
    listed: Mailbox<(String, Result<Vec<RemoteTrack>, String>)>,
    found: Option<Vec<(String, String)>>,
    found_box: Mailbox<Vec<(String, String)>>,
    searching: bool,
    pub(crate) rev: u64,
}

fn track_json(t: &RemoteTrack) -> Value {
    Value::object()
        .with("key", t.key.as_str())
        .with("title", t.title.as_str())
        .with("artists", t.artists.clone())
        .with("album", t.album.as_str())
        .with("albumArtist", t.album_artist.as_str())
        .with("track", t.track as u64)
        .with("disc", t.disc as u64)
        .with("year", t.year as u64)
        .with("genre", t.genre.as_str())
        .with("durationMs", t.duration_ms)
        .with("size", t.size)
        .with("modified", t.modified)
        .with("ext", t.ext.as_str())
        .with("cover", t.has_cover)
}

fn track_from(v: &Value) -> Option<RemoteTrack> {
    Some(RemoteTrack {
        key: v.get("key").as_str()?.to_string(),
        title: v.str_or("title", "").to_string(),
        artists: v
            .get("artists")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect(),
        album: v.str_or("album", "").to_string(),
        album_artist: v.str_or("albumArtist", "").to_string(),
        track: v.get("track").as_u64().unwrap_or(0) as u32,
        disc: v.get("disc").as_u64().unwrap_or(0) as u32,
        year: v.get("year").as_u64().unwrap_or(0) as u32,
        genre: v.str_or("genre", "").to_string(),
        duration_ms: v.get("durationMs").as_u64().unwrap_or(0),
        size: v.get("size").as_u64().unwrap_or(0),
        modified: v.get("modified").as_u64().unwrap_or(0),
        ext: v.str_or("ext", "").to_string(),
        has_cover: v.bool_or("cover", false),
    })
}

/// (source id, key) of a source track id.
pub(crate) fn split_id(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(ID_PREFIX)?.split_once(':')
}

fn library_track(s: &AppSource, t: &RemoteTrack) -> Track {
    let clean = |v: &str, fallback: &str| -> String {
        let v = v.replace('/', "-");
        match v.trim() {
            "" => fallback.to_string(),
            v => v.to_string(),
        }
    };
    let title = if t.title.trim().is_empty() {
        t.key.rsplit('/').next().unwrap_or(&t.key).to_string()
    } else {
        t.title.clone()
    };
    let album_artist = if t.album_artist.is_empty() {
        t.artists.first().cloned().unwrap_or_default()
    } else {
        t.album_artist.clone()
    };
    let path = if matches!(s.cfg.kind.as_str(), "webdav" | "sftp") {
        format!("{}/{}", s.name, t.key)
    } else {
        format!(
            "{}/{}/{}/{}",
            s.name,
            clean(&album_artist, "-"),
            clean(&t.album, "-"),
            clean(&title, &t.key)
        )
    };
    let album_key = if t.album.is_empty() {
        format!("src:{}", s.id)
    } else {
        format!(
            "{}\u{1}{}",
            album_artist.to_lowercase(),
            t.album.to_lowercase()
        )
    };
    Track {
        id: format!("{ID_PREFIX}{}:{}", s.id, t.key),
        album_id: format!("la:{:016x}", crate::library::fnv(&album_key)),
        title,
        artists: t.artists.clone(),
        album_artist,
        album: t.album.clone(),
        genre: t.genre.clone(),
        year: (t.year > 0).then_some(t.year as i32),
        track: (t.track > 0).then_some(t.track),
        disc: (t.disc > 0).then_some(t.disc),
        duration_ms: t.duration_ms,
        codec: t.ext.clone(),
        size: t.size,
        path,
        modified: t.modified,
        has_cover: t.has_cover,
        ..Default::default()
    }
}

/// A connection to a source, made once and shared.
fn connection(conns: &Conns, id: &str, cfg: &SourceConfig) -> Result<Arc<dyn MusicSource>, String> {
    if let Some(c) = conns.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
        return Ok(c.clone());
    }
    let c: Arc<dyn MusicSource> = Arc::from(bl_sources::connect(cfg)?);
    conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.to_string(), c.clone());
    Ok(c)
}

fn random_id(host: &dyn Host) -> String {
    let a = host.random_seed();
    let b = (host.unix_time() * 1e6) as u64;
    format!("{:016x}", a ^ b.rotate_left(17))
}

impl Core {
    /// Reads the sources and their cached listings; asks the protected
    /// store for the credentials.
    pub(crate) fn load_sources(&mut self, host: &mut dyn Host) {
        if self.sources.loaded {
            return;
        }
        self.sources.loaded = true;
        let list: Vec<AppSource> = host
            .storage_read(LIST_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                let id = v.get("id").as_str()?.to_string();
                if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                let mut cfg = SourceConfig::from_json(v.get("config"));
                cfg.secret.clear();
                Some(AppSource {
                    id,
                    name: v.str_or("name", "").to_string(),
                    enabled: v.bool_or("enabled", true),
                    cfg,
                })
            })
            .take(MAX_SOURCES)
            .collect();
        for s in &list {
            let cached: Vec<RemoteTrack> = host
                .storage_read(&format!("sources/{}.json", s.id))
                .and_then(|d| bl_json::parse_bytes(&d).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(track_from)
                .collect();
            self.sources.tracks.insert(s.id.clone(), cached);
        }
        self.sources.list = list;
        self.publish_sources();
        if self.sources.list.is_empty() {
            self.sources.ready = true;
        } else if !(host.has_secret_store() && host.secret_load(SECRET_KEY)) {
            let secrets = host
                .storage_read(SECRET_FILE)
                .and_then(|d| bl_json::parse_bytes(&d).ok())
                .unwrap_or(Value::Null);
            self.sources_secrets(host, &secrets);
        }
    }

    /// The credentials from the protected store (`vault.secret`).
    pub(crate) fn sources_secret(&mut self, host: &mut dyn Host, value: &str) {
        let secrets = bl_net::base64_decode(value)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .unwrap_or(Value::Null);
        self.sources_secrets(host, &secrets);
    }

    fn sources_secrets(&mut self, host: &mut dyn Host, secrets: &Value) {
        for s in &self.sources.list {
            if let Some(v) = secrets.get(&s.id).as_str() {
                self.sources.secrets.insert(s.id.clone(), v.to_string());
            }
        }
        self.sources.ready = true;
        self.sources.rev += 1;
        // Listed again at each start (in the background).
        let ids: Vec<String> = self
            .sources
            .list
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.sync_source(host, &id);
        }
    }

    fn save_sources(&mut self, host: &mut dyn Host) {
        let list = Value::Array(
            self.sources
                .list
                .iter()
                .map(|s| {
                    Value::object()
                        .with("id", s.id.as_str())
                        .with("name", s.name.as_str())
                        .with("enabled", s.enabled)
                        .with("config", s.cfg.to_json())
                })
                .collect(),
        );
        host.storage_write(LIST_KEY, list.to_json().as_bytes());
        let mut secrets = Value::object();
        for (id, v) in &self.sources.secrets {
            secrets = secrets.with(id, v.as_str());
        }
        let data = secrets.to_json();
        if host.has_secret_store() {
            host.secret_store(SECRET_KEY, data.as_bytes());
            host.storage_delete(SECRET_FILE);
        } else {
            host.storage_write(SECRET_FILE, data.as_bytes());
        }
        self.sources.rev += 1;
    }

    /// Puts the songs of the enabled sources into the library.
    fn publish_sources(&mut self) {
        let mut tracks = Vec::new();
        for s in self.sources.list.iter().filter(|s| s.enabled) {
            for t in self.sources.tracks.get(&s.id).into_iter().flatten() {
                tracks.push(library_track(s, t));
            }
        }
        self.library.set_sources(tracks);
        self.sources.rev += 1;
    }

    fn source_config(&self, id: &str) -> Option<SourceConfig> {
        let s = self.sources.list.iter().find(|s| s.id == id)?;
        let mut cfg = s.cfg.clone();
        cfg.secret = self.sources.secrets.get(id).cloned().unwrap_or_default();
        Some(cfg)
    }

    /// Lists a source again on a background thread.
    fn sync_source(&mut self, host: &mut dyn Host, id: &str) {
        let Some(cfg) = self.source_config(id) else {
            return;
        };
        if !host.has_threads() {
            return;
        }
        let st = self.sources.status.entry(id.to_string()).or_default();
        if st.syncing {
            return;
        }
        st.syncing = true;
        self.sources.rev += 1;
        let old: HashMap<String, RemoteTrack> = self
            .sources
            .tracks
            .get(id)
            .into_iter()
            .flatten()
            .map(|t| (t.key.clone(), t.clone()))
            .collect();
        let (conns, mailbox, id) = (
            self.sources.conns.clone(),
            self.sources.listed.clone(),
            id.to_string(),
        );
        host.spawn(Box::new(move || {
            let known = |key: &str, size: u64, modified: u64| {
                old.get(key)
                    .filter(|t| t.size == size && t.modified == modified && modified > 0)
                    .cloned()
            };
            let res = connection(&conns, &id, &cfg).and_then(|c| c.list(&known, &mut |_| true));
            if res.is_err() {
                conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            }
            mailbox.post((id, res));
        }));
    }

    pub(crate) fn poll_sources(&mut self, host: &mut dyn Host) {
        let mut changed = false;
        for (id, res) in self.sources.listed.take() {
            let st = self.sources.status.entry(id.clone()).or_default();
            st.syncing = false;
            self.sources.rev += 1;
            if !self.sources.list.iter().any(|s| s.id == id) {
                continue;
            }
            match res {
                Ok(list) => {
                    st.error.clear();
                    st.synced = host.unix_time();
                    let v = Value::Array(list.iter().map(track_json).collect());
                    host.storage_write(&format!("sources/{id}.json"), v.to_json().as_bytes());
                    self.sources.tracks.insert(id, list);
                    changed = true;
                }
                Err(e) => st.error = e,
            }
        }
        if changed {
            self.publish_sources();
        }
        if let Some(found) = self.sources.found_box.take().pop() {
            self.sources.found = Some(found);
            self.sources.searching = false;
            self.sources.rev += 1;
        }
    }

    /// Streams a source's song (on the player's thread).
    pub(crate) fn source_opener(&self, id: &str) -> Option<SourceOpener> {
        let (src, key) = split_id(id)?;
        let cfg = self.source_config(src)?;
        let ext = self
            .library
            .track(id)
            .map(|t| t.codec.clone())
            .filter(|e| !e.is_empty())
            .or_else(|| Some(bl_sources::ext_of(key)).filter(|e| !e.is_empty()));
        let (conns, src, key) = (self.sources.conns.clone(), src.to_string(), key.to_string());
        Some(Box::new(move || {
            let open = || connection(&conns, &src, &cfg).and_then(|c| c.open(&key));
            let s = open().or_else(|_| {
                // The connection may have expired: once more with a new one.
                conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&src);
                open()
            })?;
            Ok((
                Box::new(std::io::BufReader::with_capacity(256 * 1024, s))
                    as Box<dyn bl_media::Source>,
                ext,
            ))
        }))
    }

    /// Reports a play to the source (scrobbling, play counts).
    pub(crate) fn source_played(&self, host: &mut dyn Host, id: &str) {
        let Some((src, key)) = split_id(id) else {
            return;
        };
        let Some(cfg) = self.source_config(src) else {
            return;
        };
        if !host.has_threads() {
            return;
        }
        let (conns, src, key) = (self.sources.conns.clone(), src.to_string(), key.to_string());
        host.spawn(Box::new(move || {
            if let Ok(c) = connection(&conns, &src, &cfg) {
                c.played(&key);
            }
        }));
    }

    /// Fetches covers of source songs into the cover cache.
    pub(crate) fn request_source_covers(&mut self, host: &mut dyn Host, ids: Vec<String>) {
        let mut jobs = Vec::new();
        for id in ids {
            let Some((src, key)) = split_id(&id) else {
                continue;
            };
            let Some(cfg) = self.source_config(src) else {
                continue;
            };
            let Some(base) =
                host.storage_path(&format!("covers/{:016x}", crate::library::fnv(&id)))
            else {
                continue;
            };
            let (src, key) = (src.to_string(), key.to_string());
            self.covers.insert(id.clone(), None);
            jobs.push((id, src, key, cfg, base));
        }
        if jobs.is_empty() || !host.has_threads() {
            return;
        }
        let (conns, mailbox) = (self.sources.conns.clone(), self.cover_box.clone());
        host.spawn(Box::new(move || {
            for (id, src, key, cfg, base) in jobs {
                let cached = ["jpg", "png", "img"]
                    .iter()
                    .map(|e| format!("{base}.{e}"))
                    .find(|p| std::path::Path::new(p).exists());
                let out = cached.or_else(|| {
                    let bytes = connection(&conns, &src, &cfg).ok()?.cover(&key, 512)?;
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
        }));
    }

    pub(crate) fn sources_json(&self) -> Value {
        let list: Vec<Value> = self
            .sources
            .list
            .iter()
            .map(|s| {
                let st = self.sources.status.get(&s.id).cloned().unwrap_or_default();
                Value::object()
                    .with("id", s.id.as_str())
                    .with("name", s.name.as_str())
                    .with("kind", s.cfg.kind.as_str())
                    .with("url", s.cfg.url.as_str())
                    .with("user", s.cfg.user.as_str())
                    .with("path", s.cfg.path.as_str())
                    .with("pin", s.cfg.pin.as_str())
                    .with(
                        "untrustedKey",
                        bl_sources::sftp::pin_in_error(&st.error)
                            .filter(|_| st.error.contains("unknown host key"))
                            .unwrap_or_default(),
                    )
                    .with("enabled", s.enabled)
                    .with("hasSecret", self.sources.secrets.contains_key(&s.id))
                    .with(
                        "songs",
                        self.sources.tracks.get(&s.id).map_or(0, Vec::len) as u64,
                    )
                    .with("syncing", st.syncing)
                    .with("error", st.error.as_str())
                    .with("synced", st.synced)
            })
            .collect();
        let found: Vec<Value> = self
            .sources
            .found
            .iter()
            .flatten()
            .map(|(name, url)| {
                Value::object()
                    .with("name", name.as_str())
                    .with("url", url.as_str())
            })
            .collect();
        Value::object()
            .with("rev", self.sources.rev)
            .with("available", true)
            .with("list", list)
            .with("searching", self.sources.searching)
            .with("found", found)
    }

    /// `sources.*` commands.
    pub(crate) fn sources_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let id = c.str_or("id", "").to_string();
        let err = |m: &str| Value::object().with("error", m);
        match name {
            "sources.save" => {
                if !self.sources.ready {
                    // The stored logins have not arrived yet.
                    return err(self.tr("Please try again in a moment"));
                }
                let mut cfg = SourceConfig::from_json(c);
                cfg.kind = cfg.kind.trim().to_ascii_lowercase();
                if !KINDS.contains(&cfg.kind.as_str()) {
                    return err("unknown kind");
                }
                let scheme_ok = match cfg.kind.as_str() {
                    "sftp" => cfg.url.starts_with("sftp://"),
                    _ => cfg.url.starts_with("http://") || cfg.url.starts_with("https://"),
                };
                if !scheme_ok {
                    return err(self.tr("The address must start with https:// or http://"));
                }
                let secret = std::mem::take(&mut cfg.secret);
                let existing = self.sources.list.iter().position(|s| s.id == id);
                if !id.is_empty() && existing.is_none() {
                    return err("no such source");
                }
                if existing.is_none() && self.sources.list.len() >= MAX_SOURCES {
                    return err("too many sources");
                }
                let id = if id.is_empty() { random_id(host) } else { id };
                let name: String = c.str_or("name", "").trim().chars().take(64).collect();
                let s = AppSource {
                    id: id.clone(),
                    name: if name.is_empty() {
                        cfg.kind.clone()
                    } else {
                        name
                    },
                    enabled: c.bool_or("enabled", true),
                    cfg,
                };
                match existing {
                    Some(i) => self.sources.list[i] = s,
                    None => self.sources.list.push(s),
                }
                if !secret.is_empty() {
                    self.sources.secrets.insert(id.clone(), secret);
                }
                self.sources
                    .conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                self.save_sources(host);
                self.publish_sources();
                self.sync_source(host, &id);
                Value::object().with("id", id)
            }
            "sources.remove" => {
                let n = self.sources.list.len();
                self.sources.list.retain(|s| s.id != id);
                if n == self.sources.list.len() {
                    return err("no such source");
                }
                self.sources.secrets.remove(&id);
                self.sources.tracks.remove(&id);
                self.sources.status.remove(&id);
                self.sources
                    .conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                host.storage_delete(&format!("sources/{id}.json"));
                self.save_sources(host);
                self.publish_sources();
                Value::object().with("ok", true)
            }
            "sources.trust" => {
                // SFTP: keeps the server key the last attempt reported.
                let fp = self
                    .sources
                    .status
                    .get(&id)
                    .and_then(|st| bl_sources::sftp::pin_in_error(&st.error));
                let Some(i) = self.sources.list.iter().position(|s| s.id == id) else {
                    return err("no such source");
                };
                let Some(fp) = fp else {
                    return err("no server key to trust");
                };
                self.sources.list[i].cfg.pin = fp;
                if let Some(st) = self.sources.status.get_mut(&id) {
                    st.error.clear();
                }
                self.save_sources(host);
                self.sync_source(host, &id);
                Value::object().with("ok", true)
            }
            "sources.sync" => {
                self.sync_source(host, &id);
                Value::object().with("ok", true)
            }
            "sources.discover" => {
                if !self.sources.searching && host.has_threads() {
                    self.sources.searching = true;
                    self.sources.found = None;
                    self.sources.rev += 1;
                    let mailbox = self.sources.found_box.clone();
                    host.spawn(Box::new(move || {
                        let found = bl_sources::dlna::discover(std::time::Duration::from_secs(3))
                            .into_iter()
                            .map(|f| (f.name, f.location))
                            .collect();
                        mailbox.post(found);
                    }));
                }
                Value::object().with("ok", true)
            }
            _ => err("unknown command"),
        }
    }
}
