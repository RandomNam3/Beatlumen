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

use crate::util;
use crate::State;
use bl_json::Value;
use bl_net::client::{self, Options};
use bl_net::server::{Body, Request, Response};
use std::collections::HashMap;
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

struct Ext {
    id: String,
    url: String,
    /// Last manifest (or failure) and when it was fetched.
    manifest: Mutex<Option<(Instant, Option<Manifest>)>>,
}

#[derive(Clone)]
struct Manifest {
    name: String,
    search: bool,
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
    imported_file: PathBuf,
    /// "ext/item" -> file of a kept import.
    imported: Mutex<HashMap<String, PathBuf>>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    state: OnceLock<Weak<State>>,
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
            imported_file: ext_root.join("imported.json"),
            imported: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            state: OnceLock::new(),
        };
        if e.enabled() {
            for d in [&e.imports, &e.temp, &e.images] {
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
        let mut m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(o) = v.as_object() {
            for (k, p) in o.iter() {
                if let Some(rel) = p.as_str().and_then(|r| util::join_rel(&self.imports, r)) {
                    m.insert(k.to_string(), rel);
                }
            }
        }
    }

    fn save_imported(&self) {
        let m = self.imported.lock().unwrap_or_else(|e| e.into_inner());
        let mut o = Value::object();
        for (k, p) in m.iter() {
            if let Some(rel) = p
                .strip_prefix(&self.imports)
                .ok()
                .and_then(util::rel_string)
            {
                o = o.with(k, rel);
            }
        }
        if let Err(e) = util::write_atomic(&self.imported_file, o.to_json().as_bytes()) {
            crate::warn!("cannot write {}: {e}", self.imported_file.display());
        }
    }

    /// Removes old temporary imports and image thumbnails.
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
            (&self.images, Duration::from_secs(7 * 24 * 3600)),
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
            Ok(v) if v.get("protocol").as_u64() == Some(1) => Some(Manifest {
                name: Some(text(&v, "name"))
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| ext.id.clone()),
                search: v.get("search").as_bool().unwrap_or(false),
            }),
            Ok(_) => {
                crate::warn!("extension {}: unsupported protocol version", ext.id);
                None
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
            .get(&format!("{ext}/{item}"))
            .cloned()?;
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
                                .with("search", man.search),
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
                match self.import(state, e, item, keep) {
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
        let v = match self.get_json(e, &format!("/search?q={}&limit={limit}", pct(&q)), 30) {
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
            .take(limit as usize)
            .filter_map(|raw| {
                let it = parse_item(raw)?;
                let detail = text(raw, "detail");
                Some(
                    Value::object()
                        .with("id", it.id.as_str())
                        .with("title", it.title.as_str())
                        .with("artist", it.artist.as_str())
                        .with("album", it.album.as_str())
                        .with("duration", it.duration)
                        .with("detail", detail)
                        .with(
                            "image",
                            it.image
                                .then(|| format!("/api/extensions/{}/image/{}", e.id, it.id)),
                        )
                        .with("track", self.imported_track(state, &e.id, &it.id)),
                )
            })
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
    ) -> Result<Value, (u16, String)> {
        if keep {
            if let Some(track) = self.imported_track(state, &e.id, item_id) {
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
                    exts.imported
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            format!("{ext_id}/{}", job.item.id),
                            final_path.to_path_buf(),
                        );
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
