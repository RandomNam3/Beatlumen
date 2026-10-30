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
const KINDS: [&str; 6] = ["subsonic", "jellyfin", "webdav", "sftp", "dlna", "web"];

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
    /// Internet radio stations: (name, url).
    stations: Vec<(String, String)>,
    /// Station addresses from playlist files or shared links (not typed in
    /// by the user): they may only reach the internet, so a crafted file
    /// cannot make the app send requests into the local network.
    pub(crate) public_only: std::collections::HashSet<String>,
    /// The playing station's title updates (set on the player's thread).
    radio_titles: Arc<Mutex<Option<bl_sources::radio::Titles>>>,
    radio_title: Option<String>,
}

pub(crate) const RADIO_PREFIX: &str = "radio:";
const STATIONS_KEY: &str = "radio.json";
const MAX_STATIONS: usize = 500;

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
    let path = if matches!(s.cfg.kind.as_str(), "webdav" | "sftp" | "web") {
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
        self.sources.stations = host
            .storage_read(STATIONS_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                let url = v.get("url").as_str()?.to_string();
                Some((
                    v.str_or("name", &url).to_string(),
                    url,
                    v.bool_or("public", false),
                ))
            })
            .take(MAX_STATIONS)
            .map(|(name, url, public)| {
                if public {
                    self.sources.public_only.insert(url.clone());
                }
                (name, url)
            })
            .collect();
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
        self.poll_radio_title();
        if let Some(found) = self.sources.found_box.take().pop() {
            self.sources.found = Some(found);
            self.sources.searching = false;
            self.sources.rev += 1;
        }
    }

    /// A station as a track (title: its name until the stream says more).
    pub(crate) fn radio_track(&self, id: &str) -> Option<Track> {
        let url = id.strip_prefix(RADIO_PREFIX)?;
        let name = self
            .sources
            .stations
            .iter()
            .find(|(_, u)| u == url)
            .map_or_else(|| url.to_string(), |(n, _)| n.clone());
        Some(Track {
            id: id.to_string(),
            title: name.clone(),
            album: name,
            path: url.to_string(),
            codec: "radio".into(),
            ..Default::default()
        })
    }

    /// A station address that came from a file or link, not from the user:
    /// it may only reach the internet (unless the user added it).
    pub(crate) fn mark_public_only(&mut self, url: &str) {
        if !self.sources.stations.iter().any(|(_, u)| u == url) {
            self.sources.public_only.insert(url.to_string());
        }
    }

    /// Plays a station live (the decoder runs on the player's thread).
    pub(crate) fn play_radio(&mut self, host: &mut dyn Host, id: &str, autoplay: bool) {
        let Some(url) = id.strip_prefix(RADIO_PREFIX).map(str::to_string) else {
            return;
        };
        let slot = self.sources.radio_titles.clone();
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.sources.radio_title = None;
        self.player.set_gain(Some(1.0), true);
        let public = self.sources.public_only.contains(&url);
        self.player.load_decoder(
            host,
            Box::new(move || {
                let stream = if public {
                    bl_sources::radio::open_stream_public(&url)?
                } else {
                    bl_sources::radio::open_stream(&url)?
                };
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.titles.clone());
                bl_sources::radio::open_decoder(stream)
            }),
            autoplay,
        );
    }

    /// Shows the station's current song ("Artist - Title").
    fn poll_radio_title(&mut self) {
        let Some(cur) = self
            .current
            .as_ref()
            .filter(|t| t.id.starts_with(RADIO_PREFIX))
        else {
            return;
        };
        let title = self
            .sources
            .radio_titles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|t| t.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .filter(|t| !t.trim().is_empty());
        if title == self.sources.radio_title {
            return;
        }
        self.sources.radio_title = title.clone();
        let mut t = self.radio_track(&cur.id).unwrap_or_else(|| cur.clone());
        if let Some(text) = title {
            match bl_sources::playlist::split_artist_title(&text) {
                (Some(artist), Some(title)) if !title.is_empty() => {
                    t.artists = vec![artist];
                    t.title = title;
                }
                _ => t.title = text,
            }
        }
        self.current = Some(t);
        self.revs.queue += 1;
    }

    fn save_stations(&mut self, host: &mut dyn Host) {
        let v = Value::Array(
            self.sources
                .stations
                .iter()
                .map(|(n, u)| {
                    Value::object()
                        .with("name", n.as_str())
                        .with("url", u.as_str())
                        .with("public", self.sources.public_only.contains(u))
                })
                .collect(),
        );
        host.storage_write(STATIONS_KEY, v.to_json().as_bytes());
        self.sources.rev += 1;
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
        let stations: Vec<Value> = self
            .sources
            .stations
            .iter()
            .map(|(n, u)| {
                Value::object()
                    .with("name", n.as_str())
                    .with("url", u.as_str())
                    .with("id", format!("{RADIO_PREFIX}{u}"))
            })
            .collect();
        Value::object()
            .with("rev", self.sources.rev)
            .with("available", true)
            .with("stations", stations)
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
                // The stored secret only goes to the same login at the same
                // place: a changed address asks for it again.
                if let Some(i) = existing {
                    if !same_login(&self.sources.list[i].cfg, &s.cfg) {
                        self.sources.secrets.remove(&id);
                    }
                }
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
            "radio.add" => {
                let url = c.str_or("url", "").trim().to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() > 2048
                {
                    return err(self.tr("The address must start with https:// or http://"));
                }
                let name: String = c.str_or("name", "").trim().chars().take(100).collect();
                let name = if name.is_empty() { url.clone() } else { name };
                let full = self.sources.stations.len() >= MAX_STATIONS;
                // Typed in (or confirmed) by the user: local addresses too.
                self.sources.public_only.remove(&url);
                match self.sources.stations.iter_mut().find(|(_, u)| *u == url) {
                    Some(s) => s.0 = name,
                    None if full => return err("too many stations"),
                    None => self.sources.stations.push((name, url.clone())),
                }
                self.save_stations(host);
                Value::object().with("id", format!("{RADIO_PREFIX}{url}"))
            }
            "radio.remove" => {
                let url = c.str_or("url", "").to_string();
                self.sources.stations.retain(|(_, u)| *u != url);
                self.save_stations(host);
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

    /// Opens a playlist file (M3U/M3U8, PLS, XSPF): its songs become a
    /// playlist, plain stream addresses become radio stations. Entries
    /// resolve only inside the music folders or the playlist's own folder,
    /// or to library songs with the same path.
    pub(crate) fn import_playlist_file(&mut self, host: &mut dyn Host, path: &str, play: bool) {
        use bl_sources::playlist::{self, Location};
        let ext = path.rsplit('.').next().map(str::to_ascii_lowercase);
        let entries = match host
            .fs_read(path)
            .and_then(|d| playlist::parse(&d, ext.as_deref()))
        {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("{}: {e}", self.tr("Cannot read the playlist"));
                self.toast(msg, true);
                return;
            }
        };
        let file = std::path::Path::new(path);
        let dir = file
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let name = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.tr("Playlist").to_string());
        let mut roots: Vec<std::path::PathBuf> = self
            .settings
            .library_folders
            .iter()
            .map(std::path::PathBuf::from)
            .collect();
        roots.push(dir.clone());
        // Library songs by their path (lower case, '/' separated).
        let by_path: HashMap<String, String> = self
            .library
            .tracks
            .iter()
            .filter(|t| !t.path.is_empty())
            .map(|t| (t.path.replace('\\', "/").to_lowercase(), t.id.clone()))
            .collect();
        let mut ids = Vec::new();
        let mut streams = Vec::new();
        for e in &entries {
            match playlist::resolve(&e.location, &dir) {
                Some(Location::Url(u)) if u.starts_with("http://") || u.starts_with("https://") => {
                    streams.push((e.title.clone().unwrap_or_else(|| u.clone()), u));
                }
                Some(Location::Path(p)) => {
                    let s = p.to_string_lossy().to_string();
                    if playlist::within(&p, &roots) && host.fs_metadata(&s).is_some() {
                        ids.push(format!("{}{s}", crate::library::LOCAL_PREFIX));
                        continue;
                    }
                    // The same song in the library (e.g. on the server):
                    // the longest matching end of the path.
                    let norm = e.location.replace('\\', "/").to_lowercase();
                    let found = norm
                        .match_indices('/')
                        .map(|(i, _)| &norm[i + 1..])
                        .chain(std::iter::once(norm.as_str()))
                        .filter(|tail| tail.contains('/'))
                        .find_map(|tail| {
                            by_path
                                .iter()
                                .find(|(k, _)| k.ends_with(tail))
                                .map(|(_, id)| id.clone())
                        });
                    ids.extend(found);
                }
                _ => {}
            }
        }
        if !ids.is_empty() {
            let n = ids.len();
            self.create_playlist(
                host,
                &name,
                bl_net::api::PlaylistKind::Track,
                ids.clone(),
                false,
            );
            let msg = format!(
                "{} \"{name}\": {n} {}",
                self.tr("Playlist imported"),
                self.tr("songs")
            );
            self.toast(msg, false);
            if play {
                self.play_tracks(host, ids, 0);
            }
        } else if !streams.is_empty() {
            let first = format!("{RADIO_PREFIX}{}", streams[0].1);
            let n = streams.len();
            for (title, url) in streams {
                if self.sources.stations.len() < MAX_STATIONS
                    && !self.sources.stations.iter().any(|(_, u)| *u == url)
                {
                    let title: String = title.chars().take(100).collect();
                    self.sources.public_only.insert(url.clone());
                    self.sources.stations.push((title, url));
                }
            }
            self.save_stations(host);
            let msg = format!("{n} {}", self.tr("radio stations added"));
            self.toast(msg, false);
            if play {
                self.play_tracks(host, vec![first], 0);
            }
        } else {
            let msg = self
                .tr("The playlist has no songs that can be found here")
                .to_string();
            self.toast(msg, true);
        }
    }

    /// Writes a playlist as `<folder>/<name>.m3u8`: local songs with their
    /// paths, other library songs with their library path.
    pub(crate) fn export_playlist(&mut self, id: &str, folder: &str) -> Value {
        let Some(p) = self.find_playlist(id).cloned() else {
            return Value::object().with("error", "unknown playlist");
        };
        let entries: Vec<bl_sources::playlist::Entry> = p
            .items
            .iter()
            .map(|item| {
                let t = self.track_for(item);
                let location = crate::library::local_path(item)
                    .map(str::to_string)
                    .or_else(|| item.strip_prefix(RADIO_PREFIX).map(str::to_string))
                    .unwrap_or_else(|| t.path.clone());
                bl_sources::playlist::Entry {
                    location,
                    title: Some(t.title.clone()),
                    artist: t.artists.first().cloned(),
                    duration_secs: (t.duration_ms > 0).then(|| t.duration_ms as f64 / 1000.0),
                }
            })
            .filter(|e| !e.location.is_empty())
            .collect();
        let safe: String = p
            .name
            .chars()
            .map(|c| {
                if "/\\:*?\"<>|".contains(c) || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let file = std::path::Path::new(folder).join(format!("{}.m3u8", safe.trim()));
        match std::fs::write(&file, bl_sources::playlist::write_m3u8(&entries)) {
            Ok(()) => {
                let msg = format!("{}: {}", self.tr("Saved"), file.display());
                self.toast(msg, false);
                Value::object().with("path", file.to_string_lossy().to_string())
            }
            Err(e) => {
                let msg = format!("{}: {e}", self.tr("Cannot save the playlist"));
                self.toast(msg, true);
                Value::object().with("error", e.to_string())
            }
        }
    }
}
/// Whether two source settings log in as the same user at the same place
/// (kind, user and the scheme, host and port of the address).
fn same_login(a: &SourceConfig, b: &SourceConfig) -> bool {
    fn origin(url: &str) -> String {
        let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        format!("{scheme}://{authority}").to_ascii_lowercase()
    }
    a.kind == b.kind && a.user == b.user && origin(&a.url) == origin(&b.url)
}
