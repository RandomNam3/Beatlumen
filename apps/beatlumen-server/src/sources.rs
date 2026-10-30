//! Remote music sources set up in the admin interface (Subsonic, Jellyfin,
//! WebDAV/Nextcloud, SFTP, DLNA media servers). The server lists their
//! songs in the background (at start, after a change, with every rescan)
//! and serves them like its own: the apps see one library and play through
//! the server, which reads the original files with range requests, so
//! analysis and seeking work as for local files.
//!
//! * `sources.json` holds the sources, readable only by the server's user.
//!   The credentials (which the server must be able to use) are encrypted
//!   with a key derived from the vault pepper, so the file alone, e.g. in
//!   a backup without the pepper file, does not reveal them.
//! * `sources/<id>.json` caches each source's songs, so the library is
//!   complete at start and unchanged files of file sources are not read
//!   again.
//! * Song ids are the FNV hash of `src:<source id>:<key>`, stable across
//!   listings.

use crate::library::{Entry, REMOTE_ROOT};
use crate::util::{self, id_of};
use bl_json::Value;
use bl_net::api::Track;
use bl_sources::{MusicSource, RemoteTrack, SourceConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

const MAX_SOURCES: usize = 32;

#[derive(Clone)]
pub struct SourceRec {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub cfg: SourceConfig,
}

#[derive(Clone, Default)]
pub struct Status {
    pub syncing: bool,
    pub found: usize,
    pub error: String,
    /// Unix time of the last complete listing.
    pub synced: u64,
}

pub struct Sources {
    path: PathBuf,
    /// Encrypts the credentials in `sources.json`.
    key: bl_vault::Secret,
    cache_dir: PathBuf,
    list: RwLock<Vec<SourceRec>>,
    conns: Mutex<HashMap<String, Arc<dyn MusicSource>>>,
    tracks: RwLock<HashMap<String, Vec<RemoteTrack>>>,
    status: Mutex<HashMap<String, Status>>,
    /// Sources to list now (ids).
    wanted: Mutex<Vec<String>>,
    wake: Condvar,
    /// Songs reported as played recently.
    played: Mutex<HashMap<String, Instant>>,
    /// Nextcloud logins waiting for the user's approval.
    logins: Mutex<HashMap<String, NextcloudPending>>,
}

struct NextcloudPending {
    login: bl_sources::webdav::NextcloudLogin,
    started: Instant,
    name: String,
    path: String,
}

/// Nextcloud keeps a login open for 20 minutes.
const LOGIN_TIME: Duration = Duration::from_secs(20 * 60);

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

/// Binds an encrypted login to its source.
fn aad(id: &str) -> Vec<u8> {
    format!("beatlumen source login v1\0{id}").into_bytes()
}

/// The song's path as the apps show it: file sources keep their folders,
/// media servers are sorted by artist and album.
fn display_path(source: &SourceRec, t: &RemoteTrack) -> String {
    if matches!(source.cfg.kind.as_str(), "webdav" | "sftp" | "web") {
        return format!("{}/{}", source.name, t.key);
    }
    let clean = |s: &str, fallback: &str| -> String {
        let s: String = s.chars().map(|c| if c == '/' { '-' } else { c }).collect();
        match s.trim() {
            "" => fallback.to_string(),
            s => s.to_string(),
        }
    };
    let artist = if t.album_artist.is_empty() {
        t.artists.first().map_or("", String::as_str)
    } else {
        &t.album_artist
    };
    let mut name = clean(&t.title, &t.key);
    if !t.ext.is_empty() {
        name = format!("{name}.{}", t.ext);
    }
    format!(
        "{}/{}/{}/{name}",
        source.name,
        clean(artist, "-"),
        clean(&t.album, "-")
    )
}

/// The library entry of a source's song.
pub fn entry(source: &SourceRec, t: &RemoteTrack) -> Entry {
    let id = id_of(&format!("src:{}:{}", source.id, t.key));
    let artists = if t.artists.is_empty() {
        Vec::new()
    } else {
        t.artists.clone()
    };
    let track = Track {
        id,
        title: if t.title.trim().is_empty() {
            t.key.rsplit('/').next().unwrap_or(&t.key).to_string()
        } else {
            t.title.clone()
        },
        album_artist: if t.album_artist.is_empty() {
            artists.first().cloned().unwrap_or_default()
        } else {
            t.album_artist.clone()
        },
        artists,
        album: t.album.clone(),
        genre: t.genre.clone(),
        year: (t.year > 0).then_some(t.year as i32),
        track: (t.track > 0).then_some(t.track),
        disc: (t.disc > 0).then_some(t.disc),
        duration_ms: t.duration_ms,
        codec: t.ext.clone(),
        size: t.size,
        path: display_path(source, t),
        modified: t.modified,
        ..Default::default()
    };
    Entry {
        track,
        root: REMOTE_ROOT,
        rel: format!("{}/{}", source.id, t.key),
        abs: PathBuf::new(),
        mtime_ns: t.modified.saturating_mul(1_000_000_000),
        embedded_cover: Some(t.has_cover),
        dir_cover_file: None,
        dir_cover_track: None,
        remote: Some((source.id.clone(), t.key.clone())),
    }
}

impl Sources {
    pub fn load(data: &std::path::Path, key: bl_vault::Secret) -> Sources {
        let path = data.join("sources.json");
        let cache_dir = data.join("sources");
        let mut plain = false;
        let list: Vec<SourceRec> = std::fs::read(&path)
            .ok()
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                let id = v.get("id").as_str().filter(|i| util::is_id(i))?.to_string();
                let mut cfg = SourceConfig::from_json(v.get("config"));
                plain |= !cfg.secret.is_empty();
                if let Some(sealed) = v.get("config").get("sealed").as_str() {
                    match util::unhex(sealed)
                        .and_then(|d| bl_vault::aead::open(&key, &aad(&id), &d).ok())
                        .and_then(|d| String::from_utf8(d).ok())
                    {
                        Some(s) => cfg.secret = s,
                        None => crate::warn!(
                            "cannot decrypt the login of source {id}: was the pepper file changed? Enter it again in the admin interface"
                        ),
                    }
                }
                Some(SourceRec {
                    id,
                    name: v.str_or("name", "").to_string(),
                    enabled: v.bool_or("enabled", true),
                    cfg,
                })
            })
            .take(MAX_SOURCES)
            .collect();
        let mut tracks = HashMap::new();
        for s in &list {
            let cached: Vec<RemoteTrack> = std::fs::read(cache_dir.join(format!("{}.json", s.id)))
                .ok()
                .and_then(|d| bl_json::parse_bytes(&d).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(track_from)
                .collect();
            tracks.insert(s.id.clone(), cached);
        }
        let wanted = list
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.clone())
            .collect();
        let sources = Sources {
            path,
            key,
            cache_dir,
            list: RwLock::new(list),
            conns: Mutex::new(HashMap::new()),
            tracks: RwLock::new(tracks),
            status: Mutex::new(HashMap::new()),
            wanted: Mutex::new(wanted),
            wake: Condvar::new(),
            played: Mutex::new(HashMap::new()),
            logins: Mutex::new(HashMap::new()),
        };
        if plain {
            // Written by hand or by an older version: encrypt the logins.
            let _ = sources.save();
        }
        sources
    }

    fn save(&self) -> Result<(), String> {
        let list = self.list.read().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for s in list.iter() {
            let mut cfg = s.cfg.clone();
            let secret = std::mem::take(&mut cfg.secret);
            let mut config = cfg.to_json();
            if !secret.is_empty() {
                let sealed = bl_vault::aead::seal(&self.key, &aad(&s.id), secret.as_bytes())
                    .map_err(|e| format!("cannot encrypt: {e:?}"))?;
                config = config.with("sealed", util::hex(&sealed));
            }
            out.push(
                Value::object()
                    .with("id", s.id.as_str())
                    .with("name", s.name.as_str())
                    .with("enabled", s.enabled)
                    .with("config", config),
            );
        }
        let v = Value::Array(out);
        util::write_atomic(&self.path, v.to_json_pretty().as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))?;
        util::make_private(&self.path);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<SourceRec> {
        self.list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// The library entries of all enabled sources.
    pub fn entries(&self) -> Vec<Entry> {
        let list = self.list.read().unwrap_or_else(|e| e.into_inner());
        let tracks = self.tracks.read().unwrap_or_else(|e| e.into_inner());
        list.iter()
            .filter(|s| s.enabled)
            .flat_map(|s| {
                tracks
                    .get(&s.id)
                    .into_iter()
                    .flatten()
                    .map(move |t| entry(s, t))
            })
            .collect()
    }

    /// The sources for the admin interface (no secrets).
    pub fn admin_json(&self) -> Value {
        let list = self.list.read().unwrap_or_else(|e| e.into_inner());
        let tracks = self.tracks.read().unwrap_or_else(|e| e.into_inner());
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        Value::Array(
            list.iter()
                .map(|s| {
                    let st = status.get(&s.id).cloned().unwrap_or_default();
                    Value::object()
                        .with("id", s.id.as_str())
                        .with("name", s.name.as_str())
                        .with("enabled", s.enabled)
                        .with("kind", s.cfg.kind.as_str())
                        .with("url", s.cfg.url.as_str())
                        .with("user", s.cfg.user.as_str())
                        .with("path", s.cfg.path.as_str())
                        .with("pin", s.cfg.pin.as_str())
                        .with("hasSecret", !s.cfg.secret.is_empty())
                        .with("songs", tracks.get(&s.id).map_or(0, Vec::len) as u64)
                        .with("syncing", st.syncing)
                        .with("found", st.found as u64)
                        .with("error", st.error.as_str())
                        .with("synced", st.synced)
                })
                .collect(),
        )
    }

    /// Adds or changes a source after checking that it connects. A
    /// missing secret keeps the stored one.
    pub fn put(
        &self,
        id: Option<&str>,
        name: &str,
        enabled: bool,
        mut cfg: SourceConfig,
    ) -> Result<String, (u16, String)> {
        let existing = id.and_then(|i| self.get(i));
        if id.is_some() && existing.is_none() {
            return Err((404, "no such source".into()));
        }
        if cfg.secret.is_empty() {
            if let Some(e) = &existing {
                cfg.secret = e.cfg.secret.clone();
            }
        }
        if !["subsonic", "jellyfin", "webdav", "sftp", "dlna", "web"].contains(&cfg.kind.as_str()) {
            return Err((400, format!("unknown kind \"{}\"", cfg.kind)));
        }
        if cfg.url.is_empty() {
            return Err((400, "the address is missing".into()));
        }
        let conn: Option<Arc<dyn MusicSource>> = match enabled {
            true => Some(Arc::from(
                bl_sources::connect(&cfg).map_err(|e| (400, format!("cannot connect: {e}")))?,
            )),
            false => None,
        };
        let rec = SourceRec {
            id: match &existing {
                Some(e) => e.id.clone(),
                None => {
                    crate::random::hex(8).map_err(|e| (500, format!("no random source: {e}")))?
                }
            },
            name: if name.trim().is_empty() {
                cfg.kind.clone()
            } else {
                name.trim().chars().take(64).collect()
            },
            enabled,
            cfg,
        };
        {
            let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
            match list.iter_mut().find(|s| s.id == rec.id) {
                Some(s) => *s = rec.clone(),
                None => {
                    if list.len() >= MAX_SOURCES {
                        return Err((409, "too many sources".into()));
                    }
                    list.push(rec.clone());
                }
            }
        }
        self.save().map_err(|e| (500, e))?;
        {
            let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
            match conn {
                Some(c) => conns.insert(rec.id.clone(), c),
                None => conns.remove(&rec.id),
            };
        }
        self.request(&rec.id);
        Ok(rec.id)
    }

    pub fn delete(&self, id: &str) -> bool {
        let removed = {
            let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
            let n = list.len();
            list.retain(|s| s.id != id);
            n != list.len()
        };
        if removed {
            let _ = self.save();
            self.tracks
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
            self.conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
            let _ = std::fs::remove_file(self.cache_dir.join(format!("{id}.json")));
        }
        removed
    }

    /// Lists a source again soon.
    pub fn request(&self, id: &str) {
        let mut w = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
        if !w.iter().any(|x| x == id) {
            w.push(id.to_string());
        }
        self.wake.notify_all();
    }

    /// Lists all enabled sources again (with each library rescan).
    pub fn request_all(&self) {
        let ids: Vec<String> = self
            .list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.request(&id);
        }
    }

    fn connection(&self, id: &str) -> Result<Arc<dyn MusicSource>, String> {
        if let Some(c) = self.conns.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            return Ok(c.clone());
        }
        let rec = self.get(id).ok_or("no such source")?;
        if !rec.enabled {
            return Err("the source is turned off".into());
        }
        let c: Arc<dyn MusicSource> = Arc::from(bl_sources::connect(&rec.cfg)?);
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), c.clone());
        Ok(c)
    }

    /// A song's file (reconnects once when the connection broke).
    pub fn open(&self, id: &str, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        match self.connection(id)?.open(key) {
            Ok(s) => Ok(s),
            Err(_) => {
                self.conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(id);
                self.connection(id)?.open(key)
            }
        }
    }

    /// A library entry's file: local files directly, songs of remote
    /// sources through their source.
    pub fn open_entry(&self, e: &Entry) -> Result<Box<dyn bl_media::Source>, (u16, String)> {
        let src: Box<dyn bl_media::Source> = match &e.remote {
            Some((id, key)) => self.open(id, key).map_err(|err| {
                crate::warn!("cannot open {}: {err}", e.track.path);
                (502, format!("cannot read the song from its source: {err}"))
            })?,
            None => Box::new(
                std::fs::File::open(&e.abs)
                    .map_err(|_| (404, "track file not found".to_string()))?,
            ),
        };
        Ok(Box::new(std::io::BufReader::with_capacity(256 * 1024, src)))
    }

    pub fn cover(&self, id: &str, key: &str, size: u32) -> Option<Vec<u8>> {
        self.connection(id).ok()?.cover(key, size)
    }

    /// Reports a play to the source (at most once a minute per song: the
    /// start of a stream is requested again when a player reconnects).
    pub fn played(&self, id: &str, key: &str) {
        {
            let mut recent = self.played.lock().unwrap_or_else(|e| e.into_inner());
            recent.retain(|_, t| t.elapsed() < Duration::from_secs(60));
            if recent
                .insert(format!("{id}/{key}"), Instant::now())
                .is_some()
            {
                return;
            }
        }
        if let Ok(c) = self.connection(id) {
            let key = key.to_string();
            std::thread::spawn(move || c.played(&key));
        }
    }

    /// Starts a Nextcloud login: returns its id and the page where the user
    /// allows access (the app password then comes from Nextcloud).
    pub fn nextcloud_start(
        &self,
        server: &str,
        name: &str,
        path: &str,
    ) -> Result<(String, String), (u16, String)> {
        let server = server.trim().trim_end_matches('/');
        if !(server.starts_with("https://") || server.starts_with("http://")) {
            return Err((
                400,
                "the address must start with https:// or http://".into(),
            ));
        }
        let login = bl_sources::webdav::nextcloud_start(server).map_err(|e| (400, e))?;
        if !(login.login_url.starts_with("https://") || login.login_url.starts_with("http://")) {
            return Err((502, "the server sent an invalid login page".into()));
        }
        let pid = crate::random::hex(16).map_err(|e| (500, format!("no random source: {e}")))?;
        let url = login.login_url.clone();
        let mut logins = self.logins.lock().unwrap_or_else(|e| e.into_inner());
        logins.retain(|_, p| p.started.elapsed() < LOGIN_TIME);
        if logins.len() >= 8 {
            return Err((429, "too many open logins".into()));
        }
        logins.insert(
            pid.clone(),
            NextcloudPending {
                login,
                started: Instant::now(),
                name: name.to_string(),
                path: path.to_string(),
            },
        );
        Ok((pid, url))
    }

    /// Asks Nextcloud whether the user allowed access; adds the source
    /// when so and returns its id.
    pub fn nextcloud_poll(&self, pid: &str) -> Result<Option<String>, (u16, String)> {
        let (login, name, path) = {
            let logins = self.logins.lock().unwrap_or_else(|e| e.into_inner());
            match logins.get(pid).filter(|p| p.started.elapsed() < LOGIN_TIME) {
                Some(p) => (
                    bl_sources::webdav::NextcloudLogin {
                        login_url: p.login.login_url.clone(),
                        poll_endpoint: p.login.poll_endpoint.clone(),
                        token: p.login.token.clone(),
                    },
                    p.name.clone(),
                    p.path.clone(),
                ),
                None => return Err((404, "the login ended; start it again".into())),
            }
        };
        let Some((server, user, password)) =
            bl_sources::webdav::nextcloud_poll(&login).map_err(|e| (502, e))?
        else {
            return Ok(None);
        };
        self.logins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(pid);
        let cfg = SourceConfig {
            kind: "webdav".into(),
            url: bl_sources::webdav::nextcloud_dav_url(&server, &user),
            user,
            secret: password,
            path,
            pin: String::new(),
        };
        let name = if name.trim().is_empty() {
            "Nextcloud"
        } else {
            &name
        };
        self.put(None, name, true, cfg).map(Some)
    }

    /// Media servers in the local network (for the admin's choice).
    pub fn discover() -> Value {
        Value::Array(
            bl_sources::dlna::discover(Duration::from_secs(2))
                .into_iter()
                .map(|f| Value::object().with("name", f.name).with("url", f.location))
                .collect(),
        )
    }

    /// Lists the wanted sources, one after the other; `changed` publishes
    /// the library after each.
    pub fn run(self: Arc<Self>, changed: impl Fn() + Send + 'static) {
        loop {
            let id = {
                let mut w = self.wanted.lock().unwrap_or_else(|e| e.into_inner());
                while w.is_empty() {
                    w = self
                        .wake
                        .wait_timeout(w, Duration::from_secs(3600))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                w.remove(0)
            };
            let Some(rec) = self.get(&id) else { continue };
            if !rec.enabled {
                changed();
                continue;
            }
            self.sync(&rec);
            changed();
        }
    }

    fn set_status(&self, id: &str, f: impl FnOnce(&mut Status)) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        f(s.entry(id.to_string()).or_default());
    }

    fn sync(&self, rec: &SourceRec) {
        let t0 = Instant::now();
        self.set_status(&rec.id, |s| {
            s.syncing = true;
            s.found = 0;
        });
        let result = self.connection(&rec.id).and_then(|c| {
            let old: HashMap<String, RemoteTrack> = self
                .tracks
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&rec.id)
                .into_iter()
                .flatten()
                .map(|t| (t.key.clone(), t.clone()))
                .collect();
            let known = |key: &str, size: u64, modified: u64| {
                old.get(key)
                    .filter(|t| t.size == size && t.modified == modified && modified > 0)
                    .cloned()
            };
            let id = rec.id.clone();
            c.list(&known, &mut |n| {
                self.set_status(&id, |s| s.found = n);
                true
            })
        });
        match result {
            Ok(list) => {
                let v = Value::Array(list.iter().map(track_json).collect());
                let _ = std::fs::create_dir_all(&self.cache_dir);
                let file = self.cache_dir.join(format!("{}.json", rec.id));
                if let Err(e) = util::write_atomic(&file, v.to_json().as_bytes()) {
                    crate::warn!("cannot write {}: {e}", file.display());
                }
                crate::info!(
                    "source {}: {} songs listed in {:.1} s",
                    rec.name,
                    list.len(),
                    t0.elapsed().as_secs_f32()
                );
                self.tracks
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(rec.id.clone(), list);
                self.set_status(&rec.id, |s| {
                    s.syncing = false;
                    s.error.clear();
                    s.synced = util::now_secs();
                });
            }
            Err(e) => {
                crate::warn!("source {}: {e}", rec.name);
                self.conns
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&rec.id);
                self.set_status(&rec.id, |s| {
                    s.syncing = false;
                    s.error = e;
                });
            }
        }
    }
}
