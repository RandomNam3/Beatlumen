//! Speech recognition models to pick from (the server's admin or the
//! app): a built-in list of the whisper.cpp models (file name, size,
//! SHA-256), downloaded in the background into a models folder when
//! picked.
//!
//! Downloads resume after an interruption (`<file>.part` and a range
//! request), are checked against the SHA-256 in the list and only then
//! renamed to their final name; the caller then switches to the model. Only the address of the list's source is contacted,
//! and only when the admin picks a model.
//!
//! Own models: a `ggml-<id>.bin` in the folder that is not in the list (one
//! trained on singing, say), with the timing head made for it
//! (`ggml-<id>.head`) and an optional description (`ggml-<id>.json`:
//! `{"note": "...", "rank": 1, "memoryMb": 800}`). The server offers them
//! to its apps, which keep them the same way.

use crate::sha256::Sha256;
use bl_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Where the files of the list come from.
pub const SOURCE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

pub struct ModelInfo {
    pub id: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// Memory the weights take while recognising (about), MB.
    pub memory_mb: u64,
    /// Quality and speed in a few words (English; the admin page
    /// translates by id).
    pub note: &'static str,
    /// No longer offered for download (it did worse than the others in the
    /// comparison with real songs); a downloaded copy keeps working.
    pub retired: bool,
}

/// The models (multilingual; quantised ones need less memory), from the
/// smallest to the best. Compared on real songs (writing the singing down,
/// timing a known text): tiny, small-q5_1 and medium-q5_0 did worse than
/// the others of their size or speed and are retired.
pub const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "tiny",
        file: "ggml-tiny.bin",
        size: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        memory_mb: 150,
        note: "not recommended any more",
        retired: true,
    },
    ModelInfo {
        id: "base-q5_1",
        file: "ggml-base-q5_1.bin",
        size: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
        memory_mb: 200,
        note: "fast, small",
        retired: false,
    },
    ModelInfo {
        id: "base",
        file: "ggml-base.bin",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        memory_mb: 300,
        note: "fast",
        retired: false,
    },
    ModelInfo {
        id: "small-q5_1",
        file: "ggml-small-q5_1.bin",
        size: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        memory_mb: 450,
        note: "not recommended any more",
        retired: true,
    },
    ModelInfo {
        id: "small",
        file: "ggml-small.bin",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        memory_mb: 800,
        note: "good for songs, recommended",
        retired: false,
    },
    ModelInfo {
        id: "medium-q5_0",
        file: "ggml-medium-q5_0.bin",
        size: 539_212_467,
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        memory_mb: 1_000,
        note: "not recommended any more",
        retired: true,
    },
    ModelInfo {
        id: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        memory_mb: 1_100,
        note: "best, slow",
        retired: false,
    },
];

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Receives progress messages (`true`: a problem).
pub type Log = Box<dyn Fn(&str, bool) + Send + Sync>;

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == id)
}

/// How good a model is compared to the others (the catalog is ordered
/// from the smallest to the best); models not in it rank lowest (see
/// [`Models::rank_of`] for own models).
pub fn rank(id: &str) -> usize {
    CATALOG.iter().position(|m| m.id == id).map_or(0, |i| i + 1)
}

/// The catalog id of a model file (by its file name), or the file's name
/// without `ggml-` and `.bin` (an own model's id).
pub fn id_of(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    CATALOG.iter().find(|m| m.file == name).map_or_else(
        || {
            let stem = name.strip_suffix(".bin").unwrap_or(name);
            stem.strip_prefix("ggml-").unwrap_or(stem).to_string()
        },
        |m| m.id.to_string(),
    )
}

/// The file name of a model: the catalog's, or `ggml-<id>.bin` for an own one.
pub fn file_of(id: &str) -> String {
    find(id).map_or_else(|| format!("ggml-{id}.bin"), |m| m.file.to_string())
}

/// Whether `id` can name an own model's file (no path, no catalog id).
pub fn valid_own_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && find(id).is_none()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.')
}

/// A model that is not in the catalog, in the models folder.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnModel {
    pub id: String,
    /// The model file first, then the timing head made for it if there is
    /// one (`url` is empty for files in the folder).
    pub files: Vec<FileSpec>,
    /// About, MB: from the description, else guessed from the file's size.
    pub memory_mb: u64,
    pub note: String,
    /// The description's rank: own models rank above the catalog, and
    /// among themselves by this.
    pub rank: usize,
}

impl OwnModel {
    /// The JSON the server offers to its apps (and an app keeps as the
    /// description of a downloaded one).
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("note", self.note.as_str())
            .with("memoryMb", self.memory_mb)
            .with("rank", self.rank as u64)
            .with(
                "files",
                Value::Array(
                    self.files
                        .iter()
                        .map(|f| {
                            Value::object()
                                .with("name", f.name.as_str())
                                .with("size", f.size)
                                .with("sha256", f.sha256.as_str())
                        })
                        .collect(),
                ),
            )
    }

    /// An offered model from its JSON, its files fetched through `url`
    /// (file name to address); `None` when the files are not plain names
    /// of this model or its head.
    pub fn from_json(v: &Value, url: impl Fn(&str) -> String) -> Option<OwnModel> {
        let id = v.str_or("id", "");
        if !valid_own_id(id) {
            return None;
        }
        let stem = format!("ggml-{id}");
        let files: Vec<FileSpec> = v
            .get("files")
            .as_array()?
            .iter()
            .map(|f| FileSpec {
                name: f.str_or("name", "").to_string(),
                url: url(f.str_or("name", "")),
                size: f.get("size").as_u64().unwrap_or(0),
                sha256: f.str_or("sha256", "").to_lowercase(),
            })
            .collect();
        let head_ok = |n: &str| {
            n.strip_suffix(".head").is_some_and(|s| {
                s == stem || stem.strip_prefix(s).is_some_and(|q| q.starts_with("-q"))
            })
        };
        let ok = files
            .first()
            .is_some_and(|f| f.name == format!("{stem}.bin"))
            && files.iter().skip(1).all(|f| head_ok(&f.name))
            && files.iter().all(|f| f.size > 0 && f.sha256.len() == 64);
        ok.then(|| OwnModel {
            id: id.to_string(),
            files,
            memory_mb: v.get("memoryMb").as_u64().unwrap_or(0),
            note: v.str_or("note", "").to_string(),
            rank: v.get("rank").as_u64().unwrap_or(0) as usize,
        })
    }
}

/// The timing head next to a model file stem (`ggml-x-q5_1` also takes
/// `ggml-x.head`, as `bl_align::Head::for_model` does).
fn head_name(dir: &Path, stem: &str) -> Option<String> {
    let plain = stem.rsplit_once("-q").map_or(stem, |(s, _)| s);
    [stem, plain]
        .iter()
        .map(|s| format!("{s}.head"))
        .find(|n| dir.join(n).is_file())
}

/// About the memory a model of `size` bytes takes (as the catalog's).
fn memory_guess(size: u64) -> u64 {
    100 + size / 1_000_000 * 3 / 2
}

/// A list entry with the state of its download.
pub fn with_state(v: Value, d: &Download) -> Value {
    match d {
        Download::Running { done, total } => v
            .with("state", "downloading")
            .with("done", *done)
            .with("total", *total),
        Download::Checking => v.with("state", "checking"),
        Download::Failed(e) => v.with("state", "failed").with("error", e.as_str()),
    }
}

/// A file to download: its name in the folder, where it comes from, its
/// size and SHA-256 (hex).
#[derive(Clone, Debug, PartialEq)]
pub struct FileSpec {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Download {
    Running { done: u64, total: u64 },
    Checking,
    Failed(String),
}

/// Downloads and the models folder.
pub struct Models {
    pub dir: PathBuf,
    source: String,
    downloads: Mutex<HashMap<String, Download>>,
    /// SHA-256 of own models' files by name, with the size and time of
    /// the file they were computed for.
    hashes: Mutex<HashMap<String, (u64, std::time::SystemTime, String)>>,
    log: Log,
}

impl Models {
    pub fn new(dir: PathBuf) -> Models {
        Models::with_source(dir, SOURCE.to_string())
    }

    /// With another address for the files (tests).
    pub fn with_source(dir: PathBuf, source: String) -> Models {
        Models {
            dir,
            source,
            downloads: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            log: Box::new(|_, _| {}),
        }
    }

    /// Where download messages go (the server's log).
    pub fn with_log(mut self, log: Log) -> Models {
        self.log = log;
        self
    }

    /// Where the files come from.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn path(&self, m: &ModelInfo) -> PathBuf {
        self.dir.join(m.file)
    }

    /// The list with what is downloaded, downloading or failed: the
    /// catalog, then the own models in the folder (`"own": true`).
    pub fn list(&self, current: Option<&Path>) -> Value {
        let mut list = self.catalog_list(current);
        let own = self.own_models(false);
        let downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        if let Value::Array(items) = &mut list {
            for m in own {
                let path = self.dir.join(&m.files[0].name);
                let mut v = Value::object()
                    .with("id", m.id.as_str())
                    .with("file", m.files[0].name.as_str())
                    .with("size", m.files.iter().map(|f| f.size).sum::<u64>())
                    .with("memoryMb", m.memory_mb)
                    .with("note", m.note.as_str())
                    .with("own", true)
                    .with("downloaded", true)
                    .with("current", current.is_some_and(|c| c == path));
                if let Some(d) = downloads.get(&m.id) {
                    v = with_state(v, d);
                }
                items.push(v);
            }
        }
        list
    }

    fn catalog_list(&self, current: Option<&Path>) -> Value {
        let downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        Value::Array(
            CATALOG
                .iter()
                // Retired models only while one is on the disk.
                .filter(|m| !m.retired || self.path(m).is_file() || downloads.contains_key(m.id))
                .map(|m| {
                    let path = self.path(m);
                    let v = Value::object()
                        .with("id", m.id)
                        .with("file", m.file)
                        .with("size", m.size)
                        .with("memoryMb", m.memory_mb)
                        .with("note", m.note)
                        .with("downloaded", path.is_file())
                        .with("current", current.is_some_and(|c| c == path));
                    match downloads.get(m.id) {
                        Some(d) => with_state(v, d),
                        None => v,
                    }
                })
                .collect(),
        )
    }

    /// Downloads running now.
    pub fn active(&self) -> Value {
        let d = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        Value::Array(
            d.iter()
                .filter_map(|(id, s)| match s {
                    Download::Running { done, total } => Some(
                        Value::object()
                            .with("id", id.as_str())
                            .with("done", *done)
                            .with("total", *total),
                    ),
                    Download::Checking => Some(
                        Value::object()
                            .with("id", id.as_str())
                            .with("checking", true),
                    ),
                    Download::Failed(_) => None,
                })
                .collect(),
        )
    }

    /// Starts downloading `m` unless it is there or on its way; `done` runs
    /// with the file's path once it is complete and checked.
    pub fn download(
        self: &Arc<Self>,
        m: &'static ModelInfo,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let url = format!("{}{}", self.source, m.file);
        self.download_from(m, url, done)
    }

    /// [`download`](Self::download) from another address (a Beatlumen
    /// server that offers the model).
    pub fn download_from(
        self: &Arc<Self>,
        m: &'static ModelInfo,
        url: String,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let path = self.path(m);
        if path.is_file() {
            done(path);
            return;
        }
        if m.retired {
            self.downloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    m.id.to_string(),
                    Download::Failed("this model is not offered any more".into()),
                );
            return;
        }
        {
            let mut d = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                d.get(m.id),
                Some(Download::Running { .. } | Download::Checking)
            ) {
                return;
            }
            d.insert(
                m.id.to_string(),
                Download::Running {
                    done: 0,
                    total: m.size,
                },
            );
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name(format!("download {}", m.id))
            .spawn(move || {
                (me.log)(&format!("downloading the speech model {}", m.file), false);
                match me.fetch(m, url) {
                    Ok(path) => {
                        me.downloads
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(m.id);
                        (me.log)(&format!("speech model {} downloaded", m.file), false);
                        done(path);
                    }
                    Err(e) => {
                        (me.log)(&format!("download of {} failed: {e}", m.file), true);
                        me.set(m.id, Download::Failed(e));
                    }
                }
            });
    }

    fn set(&self, id: &str, s: Download) {
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), s);
    }

    /// Removes a downloaded model (and a partial download).
    pub fn delete(&self, m: &ModelInfo) -> bool {
        let part = self.dir.join(format!("{}.part", m.file));
        let _ = std::fs::remove_file(part);
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(m.id);
        std::fs::remove_file(self.path(m)).is_ok()
    }

    fn fetch(&self, m: &ModelInfo, url: String) -> Result<PathBuf, String> {
        let spec = FileSpec {
            name: m.file.to_string(),
            url,
            size: m.size,
            sha256: m.sha256.to_string(),
        };
        self.fetch_file(m.id, &self.dir, &spec, 0, m.size)
    }

    /// The own models in the folder (with their files' SHA-256 when `hash`;
    /// computed once per file and kept while it stays the same).
    pub fn own_models(&self, hash: bool) -> Vec<OwnModel> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
            .filter(|n| !CATALOG.iter().any(|m| m.file == n))
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|name| {
                let id = id_of(Path::new(&name));
                if !valid_own_id(&id) {
                    return None;
                }
                let stem = format!("ggml-{id}");
                let desc = std::fs::read(self.dir.join(format!("{stem}.json")))
                    .ok()
                    .and_then(|b| bl_json::parse_bytes(&b).ok())
                    .unwrap_or_else(Value::object);
                let files: Vec<FileSpec> = std::iter::once(name)
                    .chain(head_name(&self.dir, &stem))
                    .filter_map(|n| {
                        let md = std::fs::metadata(self.dir.join(&n)).ok()?;
                        let sha256 = if hash {
                            self.hash(&n, &md)?
                        } else {
                            String::new()
                        };
                        Some(FileSpec {
                            name: n,
                            url: String::new(),
                            size: md.len(),
                            sha256,
                        })
                    })
                    .collect();
                let first = files.first()?;
                let memory = desc.get("memoryMb").as_u64().filter(|&m| m > 0);
                Some(OwnModel {
                    memory_mb: memory.unwrap_or_else(|| memory_guess(first.size)),
                    note: desc.str_or("note", "").to_string(),
                    rank: desc.get("rank").as_u64().unwrap_or(0) as usize,
                    id,
                    files,
                })
            })
            .collect()
    }

    fn hash(&self, name: &str, md: &std::fs::Metadata) -> Option<String> {
        let modified = md.modified().ok()?;
        if let Some((size, time, sha)) = self
            .hashes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            if *size == md.len() && *time == modified {
                return Some(sha.clone());
            }
        }
        let mut h = Sha256::default();
        let mut f = std::fs::File::open(self.dir.join(name)).ok()?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        let sha = hex(&h.finish());
        self.hashes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), (md.len(), modified, sha.clone()));
        Some(sha)
    }

    /// How good the model `id` is: the catalog's [`rank`], above it the own
    /// models by their description's rank.
    pub fn rank_of(&self, id: &str) -> usize {
        match rank(id) {
            0 if valid_own_id(id) => {
                let desc = std::fs::read(self.dir.join(format!("ggml-{id}.json")))
                    .ok()
                    .and_then(|b| bl_json::parse_bytes(&b).ok());
                let own = desc.map_or(0, |d| d.get("rank").as_u64().unwrap_or(0) as usize);
                CATALOG.len() + 1 + own
            }
            r => r,
        }
    }

    /// Starts downloading an own model offered elsewhere (its files with
    /// their addresses) into the folder, keeping its description, unless it
    /// is there or on its way; `done` runs with the model file's path.
    pub fn download_own(
        self: &Arc<Self>,
        m: OwnModel,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let path = self.dir.join(&m.files[0].name);
        let total: u64 = m.files.iter().map(|f| f.size).sum();
        {
            let mut d = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                d.get(&m.id),
                Some(Download::Running { .. } | Download::Checking)
            ) {
                return;
            }
            d.insert(m.id.clone(), Download::Running { done: 0, total });
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name(format!("download {}", m.id))
            .spawn(move || {
                (me.log)(&format!("downloading the speech model {}", m.id), false);
                let mut before = 0;
                for f in &m.files {
                    if !me.dir.join(&f.name).is_file() {
                        if let Err(e) = me.fetch_file(&m.id, &me.dir, f, before, total) {
                            (me.log)(&format!("download of {} failed: {e}", m.id), true);
                            me.set(&m.id, Download::Failed(e));
                            return;
                        }
                    }
                    before += f.size;
                }
                let mut desc = m.to_json();
                if let Value::Object(o) = &mut desc {
                    o.remove("files");
                    o.remove("id");
                }
                let _ = std::fs::write(
                    me.dir.join(format!("ggml-{}.json", m.id)),
                    desc.to_json_pretty(),
                );
                me.downloads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&m.id);
                (me.log)(&format!("speech model {} downloaded", m.id), false);
                done(path);
            });
    }

    /// Removes an own model: its file, description and its head unless
    /// another model uses that too.
    pub fn delete_own(&self, id: &str) -> bool {
        if !valid_own_id(id) {
            return false;
        }
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let stem = format!("ggml-{id}");
        let head = head_name(&self.dir, &stem);
        let removed = std::fs::remove_file(self.dir.join(format!("{stem}.bin"))).is_ok();
        let _ = std::fs::remove_file(self.dir.join(format!("{stem}.bin.part")));
        let _ = std::fs::remove_file(self.dir.join(format!("{stem}.json")));
        if let Some(head) = head {
            let used = self
                .own_models(false)
                .iter()
                .any(|m| m.files.iter().any(|f| f.name == head));
            if !used {
                let _ = std::fs::remove_file(self.dir.join(head));
            }
        }
        removed
    }

    /// Downloads one checked file into `dir` (resuming a `.part`); progress
    /// is reported under `id`, counting from `before` of `total` bytes.
    fn fetch_file(
        &self,
        id: &str,
        dir: &Path,
        m: &FileSpec,
        before: u64,
        total: u64,
    ) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let part = dir.join(format!("{}.part", m.name));
        let mut have = std::fs::metadata(&part).map(|md| md.len()).unwrap_or(0);
        if have > m.size {
            let _ = std::fs::remove_file(&part);
            have = 0;
        }
        let (status, mut body) = self.open(&m.url, have)?;
        let resumed = status == 206 && have > 0;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut done = if resumed { have } else { 0 };
        let mut buf = vec![0u8; 256 * 1024];
        let mut last = std::time::Instant::now();
        loop {
            let n = body.read(&mut buf).map_err(|e| format!("download: {e}"))?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > m.size {
                let _ = std::fs::remove_file(&part);
                return Err("the file is larger than expected".into());
            }
            file.write_all(&buf[..n])
                .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            if last.elapsed().as_millis() > 250 {
                last = std::time::Instant::now();
                self.set(
                    id,
                    Download::Running {
                        done: before + done,
                        total,
                    },
                );
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        if done != m.size {
            return Err(format!("download ended after {done} of {} bytes", m.size));
        }
        self.set(id, Download::Checking);
        let mut h = Sha256::default();
        let mut f = std::fs::File::open(&part).map_err(|e| e.to_string())?;
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        if hex(&h.finish()) != m.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err("the file does not match its checksum".into());
        }
        let path = dir.join(&m.name);
        std::fs::rename(&part, &path)
            .map_err(|e| format!("cannot rename {}: {e}", part.display()))?;
        Ok(path)
    }

    /// The state of a download started with [`download_files`](Self::download_files).
    pub fn state(&self, id: &str) -> Option<Download> {
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Starts downloading a model made of several `files` into the folder
    /// `dir/id` unless they are there or on their way; `done` runs with the
    /// folder once all are complete and checked.
    pub fn download_files(
        self: &Arc<Self>,
        id: &str,
        files: Vec<FileSpec>,
        done: impl FnOnce(PathBuf) + Send + 'static,
    ) {
        let folder = self.dir.join(id);
        if files.iter().all(|f| folder.join(&f.name).is_file()) {
            done(folder);
            return;
        }
        let total: u64 = files.iter().map(|f| f.size).sum();
        {
            let mut d = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                d.get(id),
                Some(Download::Running { .. } | Download::Checking)
            ) {
                return;
            }
            d.insert(id.to_string(), Download::Running { done: 0, total });
        }
        let me = self.clone();
        let id = id.to_string();
        let _ = std::thread::Builder::new()
            .name(format!("download {id}"))
            .spawn(move || {
                (me.log)(&format!("downloading {id}"), false);
                let mut before = 0;
                for f in &files {
                    if !folder.join(&f.name).is_file() {
                        if let Err(e) = me.fetch_file(&id, &folder, f, before, total) {
                            (me.log)(&format!("download of {id} failed: {e}"), true);
                            me.set(&id, Download::Failed(e));
                            return;
                        }
                    }
                    before += f.size;
                }
                me.downloads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                (me.log)(&format!("{id} downloaded"), false);
                done(folder);
            });
    }

    /// Removes a model made of several files (and partial downloads).
    pub fn delete_files(&self, id: &str) -> bool {
        self.downloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        std::fs::remove_dir_all(self.dir.join(id)).is_ok()
    }

    /// GET with a range from `from`, following redirects (to the file
    /// host; no credentials are sent). Returns the status and the body.
    fn open(&self, url: &str, from: u64) -> Result<(u16, Box<dyn Read + Send>), String> {
        let opts = bl_net::client::Options::default();
        let mut url = url.to_string();
        for _ in 0..5 {
            let headers = if from > 0 {
                vec![("Range".to_string(), format!("bytes={from}-"))]
            } else {
                Vec::new()
            };
            let (status, h, body) =
                bl_net::client::request_stream("GET", &url, &headers, &[], &opts)?;
            match status {
                200 | 206 => return Ok((status, body)),
                301 | 302 | 303 | 307 | 308 => {
                    let loc = h.get("location").ok_or("redirect without a location")?;
                    let base = bl_net::client::Url::parse(&url)?;
                    let next = base.join(loc);
                    let n = bl_net::client::Url::parse(&next)?;
                    // Never from https to plain http.
                    if base.scheme == "https" && n.scheme != "https" {
                        return Err("insecure redirect".into());
                    }
                    url = next;
                }
                s => return Err(format!("HTTP status {s}")),
            }
        }
        Err("too many redirects".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bl-asr-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn own_model_ids_and_files() {
        assert_eq!(id_of(Path::new("/m/ggml-small.bin")), "small");
        assert_eq!(id_of(Path::new("/m/ggml-lyrics-v6.bin")), "lyrics-v6");
        assert_eq!(file_of("small"), "ggml-small.bin");
        assert_eq!(file_of("lyrics-v6"), "ggml-lyrics-v6.bin");
        assert!(valid_own_id("lyrics-base-q5_1"));
        for bad in ["", "small", "../x", "a/b", ".hidden", "a b"] {
            assert!(!valid_own_id(bad), "{bad}");
        }
    }

    #[test]
    fn own_models_are_found_with_head_description_and_rank() {
        let dir = folder("own");
        std::fs::write(dir.join("ggml-small.bin"), b"catalog").unwrap();
        std::fs::write(dir.join("ggml-mine-q5_1.bin"), b"weights").unwrap();
        std::fs::write(dir.join("ggml-mine.head"), b"head").unwrap();
        std::fs::write(dir.join("ggml-other.bin"), b"other").unwrap();
        std::fs::write(dir.join("ggml-other.bin.part"), b"o").unwrap();
        std::fs::write(
            dir.join("ggml-mine-q5_1.json"),
            r#"{"note": "trained on singing", "rank": 2, "memoryMb": 250}"#,
        )
        .unwrap();
        let models = Models::new(dir.clone());
        let own = models.own_models(true);
        assert_eq!(own.len(), 2);
        let mine = &own[0];
        assert_eq!(mine.id, "mine-q5_1");
        assert_eq!(mine.note, "trained on singing");
        assert_eq!(mine.memory_mb, 250);
        let names: Vec<&str> = mine.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["ggml-mine-q5_1.bin", "ggml-mine.head"]);
        let mut h = Sha256::default();
        h.update(b"weights");
        assert_eq!(mine.files[0].sha256, hex(&h.finish()));
        assert_eq!(own[1].id, "other");
        assert_eq!(own[1].files.len(), 1);
        assert_eq!(own[1].memory_mb, memory_guess(5));
        // Own models above the catalog, among themselves by their rank.
        assert_eq!(models.rank_of("small"), rank("small"));
        assert_eq!(models.rank_of("other"), CATALOG.len() + 1);
        assert_eq!(models.rank_of("mine-q5_1"), CATALOG.len() + 3);
        let list = models.list(Some(&dir.join("ggml-other.bin")));
        let items = list.as_array().unwrap();
        let other = items
            .iter()
            .find(|v| v.str_or("id", "") == "other")
            .unwrap();
        assert!(other.bool_or("own", false) && other.bool_or("current", false));
        assert_eq!(other.get("size").as_u64(), Some(5));
        let mine = items
            .iter()
            .find(|v| v.str_or("id", "") == "mine-q5_1")
            .unwrap();
        assert_eq!(mine.get("size").as_u64(), Some(11));

        // The head stays while another model uses it.
        std::fs::write(dir.join("ggml-mine.bin"), b"plain").unwrap();
        assert!(models.delete_own("mine-q5_1"));
        assert!(dir.join("ggml-mine.head").is_file());
        assert!(!dir.join("ggml-mine-q5_1.json").exists());
        assert!(models.delete_own("mine"));
        assert!(!dir.join("ggml-mine.head").exists());
        assert!(!models.delete_own("../ggml-small"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offered_models_only_name_their_own_files() {
        let sha = "a".repeat(64);
        let offer = |files: &[&str]| {
            Value::object().with("id", "mine-q5_1").with(
                "files",
                Value::Array(
                    files
                        .iter()
                        .map(|n| {
                            Value::object()
                                .with("name", *n)
                                .with("size", 3u64)
                                .with("sha256", sha.as_str())
                        })
                        .collect(),
                ),
            )
        };
        let url = |n: &str| format!("http://server/api/models/mine-q5_1/{n}");
        let m =
            OwnModel::from_json(&offer(&["ggml-mine-q5_1.bin", "ggml-mine.head"]), url).unwrap();
        assert_eq!(
            m.files[1].url,
            "http://server/api/models/mine-q5_1/ggml-mine.head"
        );
        assert!(
            OwnModel::from_json(&offer(&["ggml-mine-q5_1.bin", "ggml-mine-q5_1.head"]), url)
                .is_some()
        );
        for bad in [
            &["../ggml-mine-q5_1.bin"][..],
            &["ggml-other.bin"],
            &["ggml-mine-q5_1.bin", "../../.bashrc"],
            &["ggml-mine-q5_1.bin", "ggml-mine-q5_1.json"],
            &["ggml-mine-q5_1.bin", "ggml-min.head"],
        ] {
            assert!(OwnModel::from_json(&offer(bad), url).is_none(), "{bad:?}");
        }
        let mut catalog = offer(&["ggml-small.bin"]);
        catalog.set("id", "small");
        assert!(OwnModel::from_json(&catalog, url).is_none());
    }
}
