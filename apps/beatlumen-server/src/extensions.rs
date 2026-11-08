//! Source extensions: separate programs (any language and licence) that
//! the server reaches over HTTP and that find music elsewhere, e.g. on a
//! video platform. The server only talks to the extensions the admin
//! configured; the apps only talk to the server. Protocol and API:
//! `docs/EXTENSIONS.md`.
//!
//! A song the user picks is imported by the server: it reads the audio
//! from the extension, leaves out the parts the extension marks (WebM/Opus
//! is remuxed into Ogg/Opus without re-encoding), writes tags and cover,
//! and stores it in the library ("keep") or for a while only (the app then
//! keeps it in the user's encrypted files). The file can be played while it
//! is still being written.
//!
//! An extension of protocol 2 can also offer rows of suggestions, collections
//! (albums, playlists) and a radio for a song; what it offers is in its
//! manifest. Songs it brought into the library are deleted when they were not
//! played for a while (the admin decides how long); the library's own songs
//! are never touched.

use crate::util;
use crate::State;
use bl_json::Value;
use bl_net::client::{self, Options};
use bl_net::server::{Body, Request, Response};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// Imports running at the same time.
const MAX_JOBS: usize = 3;
/// Temporary imports (for encrypted files) are kept this long.
const TEMP_KEEP: Duration = Duration::from_secs(2 * 3600);
/// Finished jobs are forgotten after this long.
const JOB_KEEP: Duration = Duration::from_secs(3600);
const MAX_JSON: usize = 1 << 20;
const MAX_IMAGE: usize = 4 << 20;
const MAX_TEXT: usize = 300;
/// Suggestions and search results change slowly, and every request here is
/// one more request to the outside world.
const SEARCH_KEEP: Duration = Duration::from_secs(10 * 60);
const BROWSE_KEEP: Duration = Duration::from_secs(15 * 60);
/// Albums and playlists change seldom: one opened again is shown from the disk
/// at once and asked for anew in the background when it is older than this.
const COLLECTION_REFRESH: Duration = Duration::from_secs(3600);
/// Answers and pictures kept on disk go when nobody looked at them this long.
const STORED_KEEP: Duration = Duration::from_secs(30 * 24 * 3600);
const MAX_CACHED: usize = 256;
const MAX_SHELVES: usize = 20;
/// A paging position is the extension's, but it travels through URLs.
const MAX_CURSOR: usize = 2048;

struct Ext {
    id: String,
    url: String,
    /// Last manifest (or failure) and when it was fetched.
    manifest: Mutex<Option<(Instant, Option<Manifest>)>>,
}

#[derive(Clone, Debug, PartialEq)]
struct Manifest {
    name: String,
    search: bool,
    shelves: bool,
    collections: bool,
    radio: bool,
}

/// What an extension says about itself; other protocol versions are refused.
fn parse_manifest(v: &Value, id: &str) -> Option<Manifest> {
    let protocol = v.get("protocol").as_u64()?;
    if !(1..=2).contains(&protocol) {
        return None;
    }
    let can = |what: &str| {
        protocol >= 2
            && v.get("capabilities")
                .as_array()
                .is_some_and(|a| a.iter().any(|c| c.as_str() == Some(what)))
    };
    Some(Manifest {
        name: Some(text(v, "name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string()),
        search: v.get("search").as_bool().unwrap_or(false) || can("search"),
        shelves: can("shelves"),
        collections: can("collections"),
        radio: can("radio"),
    })
}

/// A song the extension brought into the library.
#[derive(Clone, Debug)]
struct Imported {
    path: PathBuf,
    /// Seconds since 1970.
    added: u64,
    played: u64,
    /// Whose library it is in: who brought it in.
    holders: Vec<String>,
    /// Those of them who added it on purpose (not just by playing it): it stays
    /// however long nobody plays it.
    pinned: Vec<String>,
    /// When someone last took it out of their library.
    released: u64,
}

/// A song as the extension describes it.
#[derive(Clone, Debug, Default)]
struct Item {
    id: String,
    title: String,
    artist: String,
    album: String,
    year: String,
    /// Seconds, after the cuts.
    duration: f64,
    format: String,
    cuts: Vec<(f64, f64)>,
    image: bool,
    source: String,
}

impl Item {
    fn ext(&self) -> &'static str {
        match self.format.as_str() {
            "webm-opus" | "ogg-opus" => "opus",
            "mp3" => "mp3",
            "m4a" => "m4a",
            "flac" => "flac",
            _ => "bin",
        }
    }
}

#[derive(Default)]
struct JobState {
    written: u64,
    done: bool,
    error: Option<String>,
    /// Library track id once the scan found it.
    track: Option<String>,
    path: PathBuf,
    finished: Option<Instant>,
}

struct Job {
    id: String,
    key: String,
    keep: bool,
    /// Whose library it goes into once it is there, and whether on purpose.
    holders: Mutex<Vec<(String, bool)>>,
    item: Item,
    st: Mutex<JobState>,
    cv: Condvar,
}

impl Job {
    fn lock(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn json(&self) -> Value {
        let st = self.lock();
        Value::object()
            .with("job", self.id.as_str())
            .with(
                "state",
                if st.error.is_some() {
                    "failed"
                } else if st.done {
                    "done"
                } else {
                    "running"
                },
            )
            .with("bytes", st.written)
            .with("track", st.track.clone())
            .with("error", st.error.clone())
            .with("audio", format!("/api/extensions/jobs/{}/audio", self.id))
            .with("keep", self.keep)
            .with("title", self.item.title.as_str())
            .with("artist", self.item.artist.as_str())
            .with("album", self.item.album.as_str())
            .with("duration", self.item.duration)
            .with("mime", util::audio_mime(self.item.ext()))
    }
}

pub struct Extensions {
    exts: Vec<Ext>,
    token: Option<String>,
    /// Library folder of kept imports (one more music root).
    pub imports: PathBuf,
    temp: PathBuf,
    images: PathBuf,
    /// Suggestions, albums and playlists as last answered, by request.
    stored: PathBuf,
    /// Stored answers being asked for anew.
    refreshing: Mutex<HashSet<String>>,
    imported_file: PathBuf,
    /// "ext/item" -> the kept import.
    imported: Mutex<HashMap<String, Imported>>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    /// What extensions answered lately, by extension and request.
    answers: Mutex<HashMap<String, (Instant, Value)>>,
    state: OnceLock<Weak<State>>,
}

/// A stored answer and when it was given, when the file holds the one for
/// `key` (two requests could share a file name).
fn read_stored(file: &Path, key: &str) -> Option<(u64, Value)> {
    let len = std::fs::metadata(file).ok()?.len();
    if len > 2 * MAX_JSON as u64 {
        return None;
    }
    let v = bl_json::parse_bytes(&std::fs::read(file).ok()?).ok()?;
    if v.str_or("key", "") != key {
        return None;
    }
    Some((v.get("fetched").as_u64()?, v.get("answer").clone()))
}

/// Marks a kept file as looked at, at most once a day, so it is not pruned.
fn touch(path: &Path) {
    let day = Duration::from_secs(24 * 3600);
    let stale = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > day);
    if stale {
        if let Ok(f) = File::options().write(true).open(path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    }
}

fn opts(io_secs: u64) -> Options {
    Options {
        connect_timeout: Duration::from_secs(5),
        io_timeout: Duration::from_secs(io_secs),
        max_body: MAX_JSON,
        max_redirects: 0,
    }
}

fn pct(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn text(v: &Value, key: &str) -> String {
    v.str_or(key, "")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Item ids are opaque to the server but must be safe in URLs and paths.
fn valid_item_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        && !id.contains("..")
}

fn valid_cursor(c: &str) -> bool {
    !c.is_empty() && c.len() <= MAX_CURSOR && c.bytes().all(|b| b.is_ascii_graphic())
}

fn limit_of(req: &Request) -> usize {
    req.query("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 50)
}

fn parse_item(v: &Value) -> Option<Item> {
    let id = v.str_or("id", "").to_string();
    if !valid_item_id(&id) {
        return None;
    }
    let cuts = v
        .get("cuts")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let c = c.as_array()?;
                    let (s, e) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
                    (s.is_finite() && e.is_finite() && e > s && s >= 0.0).then_some((s, e))
                })
                .take(64)
                .collect()
        })
        .unwrap_or_default();
    let duration = v
        .get("duration")
        .as_f64()
        .filter(|d| d.is_finite() && *d >= 0.0);
    Some(Item {
        id,
        title: text(v, "title"),
        artist: text(v, "artist"),
        album: text(v, "album"),
        year: text(v, "year"),
        duration: duration.unwrap_or(0.0).min(24.0 * 3600.0),
        format: text(v, "format"),
        cuts,
        image: v.get("image").as_bool().unwrap_or(false),
        source: text(v, "source"),
    })
}

/// Writes the import and tells readers about every new byte.
struct JobWriter {
    file: File,
    job: Arc<Job>,
}

impl Write for JobWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.file.write(buf)?;
        self.job.lock().written += n as u64;
        self.job.cv.notify_all();
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Reads an import while it is written: waits for more until it is done.
struct Growing {
    job: Arc<Job>,
    file: File,
    pos: u64,
}

impl Read for Growing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut idle = Instant::now();
        loop {
            let st = self.job.lock();
            if self.pos < st.written {
                let avail = (st.written - self.pos).min(buf.len() as u64) as usize;
                drop(st);
                self.file.seek(SeekFrom::Start(self.pos))?;
                let n = self.file.read(&mut buf[..avail])?;
                self.pos += n as u64;
                return Ok(n);
            }
            if let Some(e) = &st.error {
                return Err(io::Error::other(e.clone()));
            }
            if st.done {
                return Ok(0);
            }
            let written = st.written;
            let (st, _) = self
                .job
                .cv
                .wait_timeout(st, Duration::from_secs(2))
                .unwrap_or_else(|e| e.into_inner());
            if st.written != written {
                idle = Instant::now();
            } else if idle.elapsed() > Duration::from_secs(120) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "import stalled"));
            }
        }
    }
}

impl Imported {
    /// In this account's library? Added without an account, a song is in the
    /// library everyone shares.
    fn seen_by(&self, account: &str) -> bool {
        self.holders.iter().any(|h| h == account || h.is_empty())
            || (self.holders.is_empty() && self.released == 0)
    }
}

impl Extensions {
    pub fn new(config: &crate::Config, data_root: &Path) -> Result<Extensions, String> {
        let token = match &config.extension_token_file {
            Some(p) => {
                let t = std::fs::read_to_string(p)
                    .map_err(|e| format!("cannot read extension token {}: {e}", p.display()))?;
                let t = t.trim().to_string();
                if t.len() < 16 || t.contains(['\r', '\n']) {
                    return Err(
                        "the extension token must be one line of at least 16 characters".into(),
                    );
                }
                Some(t)
            }
            None => None,
        };
        let ext_root = data_root.join("extensions");
        let e = Extensions {
            exts: config
                .extensions
                .iter()
                .map(|(id, url)| Ext {
                    id: id.clone(),
                    url: url.clone(),
                    manifest: Mutex::new(None),
                })
                .collect(),
            token,
            imports: ext_root.join("library"),
            temp: ext_root.join("temporary"),
            images: data_root.join("cache").join("extension-images"),
            stored: data_root.join("cache").join("extension-answers"),
            refreshing: Mutex::new(HashSet::new()),
            imported_file: ext_root.join("imported.json"),
            imported: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            answers: Mutex::new(HashMap::new()),
            state: OnceLock::new(),
        };
        if e.enabled() {
            for d in [&e.imports, &e.temp, &e.images, &e.stored] {
                std::fs::create_dir_all(d)
                    .map_err(|err| format!("cannot create {}: {err}", d.display()))?;
            }
            e.load_imported();
            e.prune_temp();
        }
        Ok(e)
    }

    pub fn enabled(&self) -> bool {
        !self.exts.is_empty()
    }

    pub fn attach(&self, state: &Arc<State>) {
        let _ = self.state.set(Arc::downgrade(state));
    }

    fn load_imported(&self) {
        let Ok(data) = std::fs::read(&self.imported_file) else {
            return;
        };
        let Ok(v) = bl_json::parse_bytes(&data) else {
            return;
        };
        let now = util::now_secs();
        let mut older = false;
        {
            let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(o) = v.as_object() {
                for (k, entry) in o.iter() {
                    // Before times were kept: the file only; it gets the full
                    // time from now on.
                    let names = |key: &str| -> Vec<String> {
                        entry
                            .get(key)
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|n| n.as_str().map(str::to_string))
                            .collect()
                    };
                    let (rel, added, played) = match entry.as_str() {
                        Some(rel) => {
                            older = true;
                            (rel, now, now)
                        }
                        None => (
                            entry.str_or("path", ""),
                            entry.get("added").as_u64().unwrap_or(now),
                            entry.get("played").as_u64().unwrap_or(now),
                        ),
                    };
                    if let Some(path) = util::join_rel(&self.imports, rel) {
                        m.insert(
                            k.to_string(),
                            Imported {
                                path,
                                added,
                                played,
                                holders: names("holders"),
                                pinned: names("pinned"),
                                released: entry.get("released").as_u64().unwrap_or(0),
                            },
                        );
                    }
                }
            }
        }
        if older {
            self.save_imported();
        }
    }

    fn save_imported(&self) {
        let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let mut o = Value::object();
        for (k, imp) in m.iter() {
            if let Some(rel) = imp
                .path
                .strip_prefix(&self.imports)
                .ok()
                .and_then(util::rel_string)
            {
                o = o.with(
                    k,
                    Value::object()
                        .with("path", rel)
                        .with("added", imp.added)
                        .with("played", imp.played)
                        .with("holders", imp.holders.clone())
                        .with("pinned", imp.pinned.clone())
                        .with("released", imp.released),
                );
            }
        }
        if let Err(e) = util::write_atomic(&self.imported_file, o.to_json().as_bytes()) {
            crate::warn!("cannot write {}: {e}", self.imported_file.display());
        }
    }

    /// A song is in this account's library, on purpose or by playing it.
    fn hold(&self, key: &str, account: &str, pin: bool) {
        let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = m.get_mut(key) else { return };
        let mut changed = false;
        if !i.holders.iter().any(|h| h == account) {
            i.holders.push(account.to_string());
            changed = true;
        }
        if pin && !i.pinned.iter().any(|h| h == account) {
            i.pinned.push(account.to_string());
            changed = true;
        }
        drop(m);
        if changed {
            self.save_imported();
        }
    }

    /// Songs of extensions in other accounts' libraries, not in this one's: what
    /// someone else keeps is nobody's business. Songs nobody is known to hold
    /// (imported before this was kept) are everyone's.
    pub fn foreign_tracks(&self, state: &State, account: &str) -> Vec<String> {
        let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let idx = state.library();
        m.values()
            .filter(|i| !i.seen_by(account))
            .filter_map(|i| {
                idx.entries
                    .iter()
                    .find(|e| e.abs == i.path)
                    .map(|e| e.track.id.clone())
            })
            .collect()
    }

    /// The library track of an import, for an account that has it.
    fn held_track(&self, state: &State, ext: &str, item: &str, account: &str) -> Option<String> {
        let held = {
            let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
            m.get(&format!("{ext}/{item}"))?.seen_by(account)
        };
        if held {
            self.imported_track(state, ext, item)
        } else {
            None
        }
    }

    /// A song of an extension, and not from one of the admin's folders?
    pub fn is_import(&self, path: &Path) -> bool {
        path.starts_with(&self.imports)
    }

    /// An account took this file out of its library (see `hidden.rs`).
    pub fn released(&self, path: &Path, account: &str, now: u64) {
        let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = m.values_mut().find(|i| i.path == path) else {
            return;
        };
        i.holders.retain(|h| h != account);
        i.pinned.retain(|h| h != account);
        i.released = now;
        drop(m);
        self.save_imported();
    }

    /// An account got this file back from its trash: it wanted it, so it stays.
    pub fn restored(&self, path: &Path, account: &str) {
        let key = {
            let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
            m.iter()
                .find(|(_, i)| i.path == path)
                .map(|(k, _)| k.clone())
        };
        if let Some(key) = key {
            self.hold(&key, account, true);
        }
    }

    /// A song is being played: an import stays for longer. Written about
    /// once an hour per song.
    pub fn note_played(&self, path: &Path) {
        if !path.starts_with(&self.imports) {
            return;
        }
        let now = util::now_secs();
        let changed = {
            let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
            m.values_mut()
                .find(|i| i.path == path && now.saturating_sub(i.played) >= 3600)
                .map(|i| i.played = now)
                .is_some()
        };
        if changed {
            self.save_imported();
        }
    }

    /// Deletes imports not played for as long as the admin set. Only what an
    /// extension brought in, by the list kept here and only inside its
    /// folder: the library's own songs are not on that list.
    pub fn cleanup(&self, state: &State, now: u64) -> usize {
        let cfg = state.config();
        if !self.enabled() {
            return 0;
        }
        let cutoff = now.saturating_sub(u64::from(cfg.extension_cleanup_days) * 24 * 3600);
        let keep_days = crate::vault::KEEP_DAYS * 24 * 3600;
        let listed = if cfg.extension_cleanup_keep_listed {
            state.listed_tracks()
        } else {
            HashSet::new()
        };
        let idx = state.library();
        let track_of: HashMap<&Path, &str> = idx
            .entries
            .iter()
            .filter(|e| e.abs.starts_with(&self.imports))
            .map(|e| (e.abs.as_path(), e.track.id.as_str()))
            .collect();
        let mut removed = Vec::new();
        let mut forgotten = false;
        self.imported
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, imp| {
                if !imp.path.starts_with(&self.imports) {
                    return true;
                }
                // Whoever took it out can get it back from the trash, whatever
                // else happens: nothing goes before that time is over.
                if imp.released > 0 && now.saturating_sub(imp.released) < keep_days {
                    return true;
                }
                // In nobody's library any more, and nobody played it since.
                let abandoned =
                    imp.released > 0 && imp.holders.is_empty() && imp.played < imp.released;
                // Or not played for as long as the admin set.
                let unplayed = cfg.extension_cleanup_days > 0
                    && imp.played.max(imp.added) < cutoff
                    && !(cfg.extension_cleanup_keep_listed && !imp.pinned.is_empty());
                if !abandoned && !unplayed {
                    return true;
                }
                if track_of
                    .get(imp.path.as_path())
                    .is_some_and(|id| listed.contains(*id))
                {
                    return true;
                }
                // A link in there could lead anywhere: only plain files.
                match std::fs::symlink_metadata(&imp.path) {
                    Ok(m) if !m.file_type().is_file() => return true,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        forgotten = true;
                        return false;
                    }
                    Err(_) => return true,
                    Ok(_) => {}
                }
                match std::fs::remove_file(&imp.path) {
                    Ok(()) => {
                        crate::info!("extension song not played for a while, deleted: {key}");
                        removed.push(format!("{key}/true"));
                        false
                    }
                    // In use (Windows) and the like: next time.
                    Err(e) => {
                        crate::warn!("cannot delete {}: {e}", imp.path.display());
                        true
                    }
                }
            });
        if !removed.is_empty() || forgotten {
            self.save_imported();
        }
        if !removed.is_empty() {
            // The finished imports must not answer for a file that is gone.
            self.jobs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, j| !removed.contains(&j.key));
            state.request_scan();
        }
        removed.len()
    }

    /// Makes every import look `age` seconds older (for tests).
    #[doc(hidden)]
    pub fn backdate_for_tests(&self, age: u64) {
        let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        for i in m.values_mut() {
            i.added = i.added.saturating_sub(age);
            i.played = i.played.saturating_sub(age);
        }
    }

    /// Songs and bytes the extensions keep in the library (admin overview).
    pub fn storage(&self) -> (usize, u64) {
        let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = m
            .values()
            .filter_map(|i| std::fs::metadata(&i.path).ok())
            .map(|m| m.len())
            .sum();
        (m.len(), bytes)
    }

    /// Removes old temporary imports, and pictures and answers nobody looked
    /// at for a long time.
    pub fn prune_temp(&self) {
        let old = |p: &Path, age: Duration| {
            std::fs::metadata(p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|a| a > age)
        };
        for (dir, age) in [
            (&self.temp, TEMP_KEEP),
            (&self.images, STORED_KEEP),
            (&self.stored, STORED_KEEP),
        ] {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if old(&e.path(), age) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.retain(|_, j| !j.lock().finished.is_some_and(|t| t.elapsed() > JOB_KEEP));
    }

    fn ext(&self, id: &str) -> Option<&Ext> {
        self.exts.iter().find(|e| e.id == id)
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut h = vec![("Accept".to_string(), "application/json".to_string())];
        if let Some(t) = &self.token {
            h.push(("Authorization".into(), format!("Bearer {t}")));
        }
        h
    }

    fn get_json(&self, ext: &Ext, path: &str, io_secs: u64) -> Result<Value, String> {
        let r = client::request(
            "GET",
            &format!("{}{path}", ext.url),
            &self.headers(),
            &[],
            &opts(io_secs),
        )
        .map_err(|e| format!("extension {}: {e}", ext.id))?;
        if r.status != 200 {
            let msg = bl_json::parse_bytes(&r.body)
                .ok()
                .map(|v| v.str_or("error", "").to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| format!("HTTP {}", r.status));
            return Err(format!("extension {}: {msg}", ext.id));
        }
        bl_json::parse_bytes(&r.body).map_err(|e| format!("extension {}: bad JSON: {e}", ext.id))
    }

    fn manifest(&self, ext: &Ext) -> Option<Manifest> {
        let mut m = ext.manifest.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, cached)) = m.as_ref() {
            let fresh = if cached.is_some() { 300 } else { 30 };
            if at.elapsed() < Duration::from_secs(fresh) {
                return cached.clone();
            }
        }
        let fetched = match self.get_json(ext, "/manifest", 10) {
            Ok(v) => {
                let parsed = parse_manifest(&v, &ext.id);
                if parsed.is_none() {
                    crate::warn!("extension {}: unsupported protocol version", ext.id);
                }
                parsed
            }
            Err(e) => {
                crate::warn!("{e}");
                None
            }
        };
        *m = Some((Instant::now(), fetched.clone()));
        fetched
    }

    /// The library track of a kept import, if it is (still) there.
    fn imported_track(&self, state: &State, ext: &str, item: &str) -> Option<String> {
        let path = self
            .imported
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&format!("{ext}/{item}"))?
            .path
            .clone();
        let idx = state.library();
        idx.entries
            .iter()
            .find(|e| e.abs == path)
            .map(|e| e.track.id.clone())
    }

    // ---- HTTP API ------------------------------------------------------------

    pub fn handle(&self, state: &State, req: &Request, segs: &[&str]) -> Response {
        let m = req.method.as_str();
        let get = m == "GET" || m == "HEAD";
        match segs {
            [] if get => {
                let list: Vec<Value> = self
                    .exts
                    .iter()
                    .filter_map(|e| {
                        let man = self.manifest(e)?;
                        Some(
                            Value::object()
                                .with("id", e.id.as_str())
                                .with("name", man.name)
                                .with("search", man.search)
                                .with("shelves", man.shelves)
                                .with("collections", man.collections)
                                .with("radio", man.radio),
                        )
                    })
                    .collect();
                Response::json(200, &Value::object().with("extensions", Value::Array(list)))
            }
            ["jobs", job] if get => match self.job(job) {
                Some(j) => Response::json(200, &j.json()),
                None => Response::error(404, "job not found"),
            },
            ["jobs", job, "audio"] if get => match self.job(job) {
                Some(j) => job_audio(req, &j),
                None => Response::error(404, "job not found"),
            },
            [ext, "search"] if get => {
                let Some(e) = self.ext(ext) else {
                    return Response::error(404, "extension not found");
                };
                self.search(state, e, req)
            }
            [ext, "image", item] if get => {
                let Some(e) = self.ext(ext) else {
                    return Response::error(404, "extension not found");
                };
                self.image(e, item, req)
            }
            [ext, "shelves"] if get => {
                let Some(e) = self.ext(ext).filter(|e| self.can(e, |m| m.shelves)) else {
                    return Response::error(404, "not offered by this extension");
                };
                self.shelves(state, e, req)
            }
            [ext, "collections", item] if get => {
                let Some(e) = self.ext(ext).filter(|e| self.can(e, |m| m.collections)) else {
                    return Response::error(404, "not offered by this extension");
                };
                self.collection(state, e, item, req)
            }
            [ext, "radio", item] if get => {
                let Some(e) = self.ext(ext).filter(|e| self.can(e, |m| m.radio)) else {
                    return Response::error(404, "not offered by this extension");
                };
                self.radio(state, e, item, req)
            }
            [ext, "import"] if m == "POST" => {
                let Some(e) = self.ext(ext) else {
                    return Response::error(404, "extension not found");
                };
                let Ok(body) = bl_json::parse_bytes(&req.body) else {
                    return Response::error(400, "expected JSON");
                };
                let item = body.str_or("id", "");
                if !valid_item_id(item) {
                    return Response::error(400, "invalid id");
                }
                let keep = body.get("keep").as_bool().unwrap_or(true);
                let pin = keep && body.get("pin").as_bool().unwrap_or(false);
                let account = state.account_of(req);
                match self.import(state, e, item, keep, pin, &account) {
                    Ok(v) => Response::json(200, &v),
                    Err((code, msg)) => Response::error(code, &msg),
                }
            }
            _ => Response::error(404, "not found"),
        }
    }

    fn job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    fn search(&self, state: &State, e: &Ext, req: &Request) -> Response {
        let q: String = req
            .query("q")
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect();
        if q.is_empty() {
            return Response::json(
                200,
                &Value::object().with("items", Value::Array(Vec::new())),
            );
        }
        let limit = req
            .query("limit")
            .and_then(|l| l.parse::<u32>().ok())
            .unwrap_or(20)
            .clamp(1, 50);
        let path = format!("/search?q={}&limit={limit}", pct(&q));
        let v = match self.get_cached(e, &path, 30, SEARCH_KEEP) {
            Ok(v) => v,
            Err(msg) => {
                crate::warn!("{msg}");
                return Response::error(502, &msg);
            }
        };
        let account = state.account_of(req);
        let items = self.entries(state, e, v.get("items"), limit as usize, &account);
        Response::json(200, &Value::object().with("items", Value::Array(items)))
    }

    fn can(&self, e: &Ext, what: impl Fn(&Manifest) -> bool) -> bool {
        self.manifest(e).is_some_and(|m| what(&m))
    }

    /// An answer given a moment ago, or a new one.
    fn get_cached(
        &self,
        e: &Ext,
        path: &str,
        io_secs: u64,
        keep: Duration,
    ) -> Result<Value, String> {
        let key = format!("{}{path}", e.id);
        {
            let answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, v)) = answers.get(&key) {
                if at.elapsed() < keep {
                    return Ok(v.clone());
                }
            }
        }
        let v = self.get_json(e, path, io_secs)?;
        let mut answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        if answers.len() >= MAX_CACHED {
            answers.retain(|_, (at, _)| at.elapsed() < keep);
            if answers.len() >= MAX_CACHED {
                answers.clear();
            }
        }
        answers.insert(key, (Instant::now(), v.clone()));
        Ok(v)
    }

    /// An answer kept on disk: shown at once, also after a restart, and asked
    /// for anew in the background once it is older than `refresh`. Only the
    /// first time is it waited for.
    fn get_stored(
        &self,
        e: &Ext,
        path: &str,
        io_secs: u64,
        refresh: Duration,
    ) -> Result<Value, String> {
        let key = format!("{}{path}", e.id);
        let file = self.stored_file(&key);
        if let Some((fetched, v)) = read_stored(&file, &key) {
            touch(&file);
            if util::now_secs().saturating_sub(fetched) >= refresh.as_secs() {
                self.refresh_stored(e, path, io_secs);
            }
            return Ok(v);
        }
        let v = self.get_json(e, path, io_secs)?;
        self.store(&key, &v);
        Ok(v)
    }

    fn stored_file(&self, key: &str) -> PathBuf {
        self.stored
            .join(format!("{:016x}.json", util::fnv1a64(key.as_bytes())))
    }

    fn store(&self, key: &str, v: &Value) {
        let json = Value::object()
            .with("key", key)
            .with("fetched", util::now_secs())
            .with("answer", v.clone())
            .to_json();
        if let Err(err) = util::write_atomic(&self.stored_file(key), json.as_bytes()) {
            crate::warn!("cannot keep an extension's answer: {err}");
        }
    }

    /// Asks for a stored answer anew, on a thread of its own. The new answer
    /// replaces it; a failure leaves it as it was.
    fn refresh_stored(&self, e: &Ext, path: &str, io_secs: u64) {
        let key = format!("{}{path}", e.id);
        let mut refreshing = self.refreshing.lock().unwrap_or_else(|e| e.into_inner());
        if refreshing.contains(&key) {
            return;
        }
        let Some(state) = self.state.get().cloned() else {
            return;
        };
        refreshing.insert(key.clone());
        drop(refreshing);
        let (id, path, k) = (e.id.clone(), path.to_string(), key.clone());
        let spawned = std::thread::Builder::new()
            .name("ext-refresh".into())
            .spawn(move || {
                let Some(state) = state.upgrade() else {
                    return;
                };
                let x = &state.extensions;
                if let Some(e) = x.ext(&id) {
                    match x.get_json(e, &path, io_secs) {
                        Ok(v) => x.store(&k, &v),
                        Err(msg) => crate::debug!("{msg}"),
                    }
                }
                x.refreshing
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&k);
            });
        if spawned.is_err() {
            self.refreshing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
    }

    /// Songs and collections as the apps see them; anything else is left out.
    fn entries(
        &self,
        state: &State,
        e: &Ext,
        list: &Value,
        limit: usize,
        account: &str,
    ) -> Vec<Value> {
        let collections = self.can(e, |m| m.collections);
        list.as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|raw| match raw.str_or("kind", "song") {
                "song" => self.song_json(state, e, raw, account),
                "collection" if collections => self.collection_json(e, raw),
                _ => None,
            })
            .take(limit)
            .collect()
    }

    fn image_path(&self, e: &Ext, item: &str, has_image: bool) -> Option<String> {
        has_image.then(|| format!("/api/extensions/{}/image/{item}", e.id))
    }

    fn song_json(&self, state: &State, e: &Ext, raw: &Value, account: &str) -> Option<Value> {
        if raw.str_or("kind", "song") != "song" {
            return None;
        }
        let it = parse_item(raw)?;
        let detail = text(raw, "detail");
        Some(
            Value::object()
                .with("kind", "song")
                .with("id", it.id.as_str())
                .with("title", it.title.as_str())
                .with("artist", it.artist.as_str())
                .with("album", it.album.as_str())
                .with("duration", it.duration)
                .with("detail", detail)
                .with("image", self.image_path(e, &it.id, it.image))
                .with("track", self.held_track(state, &e.id, &it.id, account)),
        )
    }

    fn collection_json(&self, e: &Ext, raw: &Value) -> Option<Value> {
        let id = raw.str_or("id", "");
        if !valid_item_id(id) {
            return None;
        }
        Some(
            Value::object()
                .with("kind", "collection")
                .with("id", id)
                .with("title", text(raw, "title"))
                .with("artist", text(raw, "artist"))
                .with("detail", text(raw, "detail"))
                .with("count", raw.get("count").as_u64())
                .with(
                    "image",
                    self.image_path(e, id, raw.get("image").as_bool().unwrap_or(false)),
                ),
        )
    }

    /// `GET .../shelves`: rows of suggestions.
    fn shelves(&self, state: &State, e: &Ext, req: &Request) -> Response {
        let limit = limit_of(req);
        let account = state.account_of(req);
        let v = match self.get_stored(e, &format!("/shelves?limit={limit}"), 30, BROWSE_KEEP) {
            Ok(v) => v,
            Err(msg) => {
                crate::warn!("{msg}");
                return Response::error(502, &msg);
            }
        };
        let shelves: Vec<Value> = v
            .get("shelves")
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|shelf| {
                let items = self.entries(state, e, shelf.get("items"), limit, &account);
                (!items.is_empty()).then(|| {
                    Value::object()
                        .with("title", text(shelf, "title"))
                        .with("items", Value::Array(items))
                })
            })
            .take(MAX_SHELVES)
            .collect();
        Response::json(200, &Value::object().with("shelves", Value::Array(shelves)))
    }

    /// `GET .../collections/<id>`: the songs of an album or playlist, a page
    /// at a time.
    fn collection(&self, state: &State, e: &Ext, item: &str, req: &Request) -> Response {
        if !valid_item_id(item) {
            return Response::error(400, "invalid id");
        }
        let limit = limit_of(req);
        let account = state.account_of(req);
        let mut path = format!("/collection/{item}?limit={limit}");
        if let Some(after) = req.query("after").filter(|a| !a.is_empty()) {
            if !valid_cursor(after) {
                return Response::error(400, "invalid position");
            }
            path.push_str(&format!("&after={}", pct(after)));
        }
        let v = match self.get_stored(e, &path, 30, COLLECTION_REFRESH) {
            Ok(v) => v,
            Err(msg) => {
                crate::warn!("{msg}");
                return Response::error(502, &msg);
            }
        };
        // Only songs in a collection.
        let items: Vec<Value> = v
            .get("items")
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|raw| self.song_json(state, e, raw, &account))
            .take(limit)
            .collect();
        Response::json(
            200,
            &Value::object()
                .with("id", item)
                .with("title", text(&v, "title"))
                .with("artist", text(&v, "artist"))
                .with("detail", text(&v, "detail"))
                .with(
                    "image",
                    self.image_path(e, item, v.get("image").as_bool().unwrap_or(false)),
                )
                .with("items", Value::Array(items))
                .with(
                    "next",
                    v.get("next")
                        .as_str()
                        .filter(|n| valid_cursor(n))
                        .map(str::to_string),
                ),
        )
    }

    /// `GET .../radio/<id>`: songs to follow a song.
    fn radio(&self, state: &State, e: &Ext, item: &str, req: &Request) -> Response {
        if !valid_item_id(item) {
            return Response::error(400, "invalid id");
        }
        let limit = limit_of(req);
        let account = state.account_of(req);
        let v = match self.get_json(e, &format!("/radio/{item}?limit={limit}"), 30) {
            Ok(v) => v,
            Err(msg) => {
                crate::warn!("{msg}");
                return Response::error(502, &msg);
            }
        };
        let items: Vec<Value> = v
            .get("items")
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|raw| self.song_json(state, e, raw, &account))
            .take(limit)
            .collect();
        Response::json(200, &Value::object().with("items", Value::Array(items)))
    }

    /// The item's picture as a JPEG thumbnail (re-encoded here, so only
    /// clean JPEG data reaches the apps).
    fn image(&self, e: &Ext, item: &str, req: &Request) -> Response {
        if !valid_item_id(item) {
            return Response::error(400, "invalid id");
        }
        let size = req
            .query("size")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(300)
            .clamp(32, 1024);
        let key = util::fnv1a64(format!("{}/{item}", e.id).as_bytes());
        let cache = self.images.join(format!("{key:016x}_{size}.jpg"));
        if cache.is_file() {
            touch(&cache);
            return Response::file(&cache, "image/jpeg", None)
                .with_header("Cache-Control", "private, max-age=86400");
        }
        let data = match self.fetch_image(e, item) {
            Ok(d) => d,
            Err(msg) => return Response::error(502, &msg),
        };
        match crate::covers::make_thumbnail(&data, size) {
            Ok(jpeg) => {
                let _ = util::write_atomic(&cache, &jpeg);
                Response::bytes(200, "image/jpeg", jpeg)
                    .with_header("Cache-Control", "private, max-age=86400")
            }
            Err(msg) => Response::error(502, &format!("cannot decode the picture: {msg}")),
        }
    }

    fn fetch_image(&self, e: &Ext, item: &str) -> Result<Vec<u8>, String> {
        let mut o = opts(30);
        o.max_body = MAX_IMAGE;
        let r = client::request(
            "GET",
            &format!("{}/image/{item}", e.url),
            &self.headers(),
            &[],
            &o,
        )
        .map_err(|err| format!("extension {}: {err}", e.id))?;
        if r.status != 200 {
            return Err(format!("extension {}: picture: HTTP {}", e.id, r.status));
        }
        Ok(r.body)
    }

    fn import(
        &self,
        state: &State,
        e: &Ext,
        item_id: &str,
        keep: bool,
        pin: bool,
        account: &str,
    ) -> Result<Value, (u16, String)> {
        if keep {
            if let Some(track) = self.imported_track(state, &e.id, item_id) {
                self.hold(&format!("{}/{item_id}", e.id), account, pin);
                // Whoever asks for it again wants it back in their library.
                state.hidden_of(account, |h| {
                    h.remove(&track);
                });
                return Ok(Value::object()
                    .with("state", "done")
                    .with("track", track)
                    .with("keep", true));
            }
        }
        let key = format!("{}/{item_id}/{keep}", e.id);
        {
            let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(j) = jobs
                .values()
                .find(|j| j.key == key && j.lock().error.is_none())
            {
                if keep {
                    j.holders
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((account.to_string(), pin));
                    self.hold(&format!("{}/{item_id}", e.id), account, pin);
                }
                return Ok(j.json());
            }
            let running = jobs.values().filter(|j| !j.lock().done).count();
            if running >= MAX_JOBS {
                return Err((503, "too many imports at once, try again shortly".into()));
            }
        }
        let v = self
            .get_json(e, &format!("/item/{item_id}"), 60)
            .map_err(|m| (502, m))?;
        let item = parse_item(&v)
            .filter(|i| i.id == item_id)
            .ok_or((502, format!("extension {}: invalid item", e.id)))?;
        if !matches!(
            item.format.as_str(),
            "webm-opus" | "ogg-opus" | "mp3" | "m4a" | "flac"
        ) {
            return Err((
                502,
                format!("extension {}: unsupported format {:?}", e.id, item.format),
            ));
        }
        let id = crate::random::hex(12).map_err(|err| (500, err.to_string()))?;
        let dir = if keep {
            self.imports.join(&e.id)
        } else {
            self.temp.clone()
        };
        std::fs::create_dir_all(&dir).map_err(|err| (500, err.to_string()))?;
        let name = if item.artist.is_empty() {
            item.title.clone()
        } else {
            format!("{} - {}", item.artist, item.title)
        };
        let stem = util::sanitize_stem(&format!("{name}.x"));
        let mut final_path = dir.join(format!("{stem}.{}", item.ext()));
        if final_path.exists() || !keep {
            final_path = dir.join(format!(
                "{stem} [{}].{}",
                item_id.replace(':', "_"),
                item.ext()
            ));
        }
        let part = final_path.with_extension(format!("{}.part", item.ext()));
        let file = File::create(&part)
            .map_err(|err| (500, format!("cannot write {}: {err}", part.display())))?;
        let job = Arc::new(Job {
            id: id.clone(),
            key,
            keep,
            holders: Mutex::new(vec![(account.to_string(), pin)]),
            item,
            st: Mutex::new(JobState {
                path: part.clone(),
                ..Default::default()
            }),
            cv: Condvar::new(),
        });
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, job.clone());
        let ext_id = e.id.clone();
        let ext_url = e.url.clone();
        let headers = self.headers();
        let weak = self.state.get().cloned();
        let j = job.clone();
        std::thread::Builder::new()
            .name("extension-import".into())
            .spawn(move || {
                let result = run_import(&ext_id, &ext_url, &headers, &j, file, &part, &final_path);
                let state = weak.and_then(|w| w.upgrade());
                finish_import(state, &ext_id, &j, result, &final_path);
            })
            .map_err(|err| (500, err.to_string()))?;
        crate::info!("extension {}: importing {:?}", e.id, job.item.title);
        Ok(job.json())
    }
}

/// Reads the audio from the extension into the file.
fn run_import(
    ext_id: &str,
    ext_url: &str,
    headers: &[(String, String)],
    job: &Arc<Job>,
    file: File,
    part: &Path,
    final_path: &Path,
) -> Result<(), String> {
    let item = &job.item;
    let mut tags = bl_media::RemuxTags {
        title: item.title.clone(),
        artist: item.artist.clone(),
        album: item.album.clone(),
        date: item.year.clone(),
        comments: Vec::new(),
        cover: None,
    };
    if !item.source.is_empty() {
        tags.comments.push(("SOURCE".into(), item.source.clone()));
    }
    if item.image {
        let mut o = opts(30);
        o.max_body = MAX_IMAGE;
        if let Ok(r) = client::request(
            "GET",
            &format!("{ext_url}/image/{}?full=1", item.id),
            headers,
            &[],
            &o,
        ) {
            // Re-encoded: only clean JPEG data goes into the file.
            if r.status == 200 {
                if let Ok(jpeg) = crate::covers::make_thumbnail(&r.body, 1024) {
                    tags.cover = Some(("image/jpeg".into(), jpeg));
                }
            }
        }
    }
    let mut o = opts(60);
    o.max_body = usize::MAX;
    let (status, _, body) = client::request_stream(
        "GET",
        &format!("{ext_url}/audio/{}", item.id),
        headers,
        &[],
        &o,
    )
    .map_err(|e| format!("extension {ext_id}: {e}"))?;
    if status != 200 {
        return Err(format!("extension {ext_id}: audio: HTTP {status}"));
    }
    let mut out = JobWriter {
        file,
        job: job.clone(),
    };
    if item.format == "webm-opus" {
        let r = bl_media::webm_opus_to_ogg(body, &mut out, &item.cuts, &tags)
            .map_err(|e| format!("cannot convert the audio: {e}"))?;
        crate::info!(
            "extension {ext_id}: {:?}: {:.1} s, {:.1} s left out",
            item.title,
            r.duration_ms as f64 / 1000.0,
            r.removed_ms as f64 / 1000.0
        );
    } else {
        // Other formats are kept as delivered.
        let mut body = body;
        io::copy(&mut body, &mut out).map_err(|e| format!("extension {ext_id}: audio: {e}"))?;
    }
    out.flush().map_err(|e| e.to_string())?;
    out.file.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(part, final_path)
        .map_err(|e| format!("cannot finish {}: {e}", final_path.display()))?;
    Ok(())
}

fn finish_import(
    state: Option<Arc<State>>,
    ext_id: &str,
    job: &Arc<Job>,
    result: Result<(), String>,
    final_path: &Path,
) {
    match result {
        Err(e) => {
            crate::warn!(
                "extension {ext_id}: import of {:?} failed: {e}",
                job.item.title
            );
            let part = job.lock().path.clone();
            let _ = std::fs::remove_file(&part);
            let mut st = job.lock();
            st.error = Some(e);
            st.done = true;
            st.finished = Some(Instant::now());
        }
        Ok(()) => {
            {
                let mut st = job.lock();
                st.path = final_path.to_path_buf();
                st.done = true;
                st.finished = Some(Instant::now());
            }
            job.cv.notify_all();
            if job.keep {
                if let Some(state) = state {
                    let exts = &state.extensions;
                    let now = util::now_secs();
                    exts.imported
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            format!("{ext_id}/{}", job.item.id),
                            Imported {
                                path: final_path.to_path_buf(),
                                added: now,
                                played: now,
                                holders: Vec::new(),
                                pinned: Vec::new(),
                                released: 0,
                            },
                        );
                    let key = format!("{ext_id}/{}", job.item.id);
                    let holders = job
                        .holders
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    for (account, pin) in holders {
                        exts.hold(&key, &account, pin);
                    }
                    exts.save_imported();
                    state.request_scan();
                    // The scan adds it to the library.
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_secs(120) {
                        if let Some(t) = exts.imported_track(&state, ext_id, &job.item.id) {
                            job.lock().track = Some(t);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
    }
    job.cv.notify_all();
}

/// The import's audio: the finished file, or while it is written the part
/// written so far followed by the rest as it comes (`Content-Range` with an
/// unknown length, `*`).
fn job_audio(req: &Request, job: &Arc<Job>) -> Response {
    let (path, done, error) = {
        let st = job.lock();
        (st.path.clone(), st.done, st.error.clone())
    };
    let mime = util::audio_mime(job.item.ext());
    if let Some(e) = error {
        return Response::error(502, &e);
    }
    if done {
        return Response::file(&path, mime, req.header("range"))
            .with_header("Cache-Control", "private, max-age=3600");
    }
    let start = req
        .header("range")
        .and_then(|r| r.trim().strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|n| n.trim().parse::<u64>().ok());
    let file = match File::open(&path) {
        Ok(f) => f,
        // Renamed just now: finished.
        Err(_) => {
            let st = job.lock();
            return Response::file(&st.path, mime, req.header("range"));
        }
    };
    let mut r = Response::new(if start.is_some() { 206 } else { 200 });
    r.headers.set("Content-Type", mime);
    r.headers.set("Accept-Ranges", "bytes");
    r.headers.set("Cache-Control", "no-store");
    if let Some(s) = start {
        r.headers.set("Content-Range", format!("bytes {s}-*/*"));
    }
    r.body = Body::Stream(Box::new(Growing {
        job: job.clone(),
        file,
        pos: start.unwrap_or(0),
    }));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> Option<Manifest> {
        parse_manifest(&bl_json::parse(json).unwrap(), "ext")
    }

    #[test]
    fn protocol_1_can_only_search() {
        let m = manifest(r#"{"protocol":1,"name":"Old","search":true,"capabilities":["radio"]}"#);
        let m = m.unwrap();
        assert_eq!(m.name, "Old");
        assert!(m.search);
        assert!(!m.radio, "capabilities belong to protocol 2");
    }

    #[test]
    fn protocol_2_says_what_it_can() {
        let m = manifest(r#"{"protocol":2,"capabilities":["search","shelves","radio","future"]}"#)
            .unwrap();
        assert_eq!(m.name, "ext", "no name: the configured id");
        assert!(m.search && m.shelves && m.radio);
        assert!(!m.collections);
    }

    #[test]
    fn other_protocols_are_refused() {
        assert!(manifest(r#"{"protocol":3,"search":true}"#).is_none());
        assert!(manifest(r#"{"search":true}"#).is_none());
    }

    #[test]
    fn a_paging_position_has_limits() {
        assert!(valid_cursor("abc-DEF_123=="));
        assert!(!valid_cursor(""));
        assert!(!valid_cursor("a b"));
        assert!(!valid_cursor("a\nb"));
        assert!(!valid_cursor(&"x".repeat(MAX_CURSOR + 1)));
    }
}
