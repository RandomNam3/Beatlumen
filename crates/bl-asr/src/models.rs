//! Speech recognition models to pick from (the server's admin or the
//! app): a built-in list of the whisper.cpp models (file name, size,
//! SHA-256), downloaded in the background into a models folder when
//! picked.
//!
//! Downloads resume after an interruption (`<file>.part` and a range
//! request), are checked against the SHA-256 in the list and only then
//! renamed to their final name; the caller then switches to the model. Only the address of the list's source is contacted,
//! and only when the admin picks a model.

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
/// from the smallest to the best); models not in it rank lowest.
pub fn rank(id: &str) -> usize {
    CATALOG.iter().position(|m| m.id == id).map_or(0, |i| i + 1)
}

/// The catalog id of a model file (by its file name), or the file's name.
pub fn id_of(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    CATALOG.iter().find(|m| m.file == name).map_or_else(
        || name.trim_end_matches(".bin").to_string(),
        |m| m.id.to_string(),
    )
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

    /// The list with what is downloaded, downloading or failed.
    pub fn list(&self, current: Option<&Path>) -> Value {
        let downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
        Value::Array(
            CATALOG
                .iter()
                // Retired models only while one is on the disk.
                .filter(|m| !m.retired || self.path(m).is_file() || downloads.contains_key(m.id))
                .map(|m| {
                    let path = self.path(m);
                    let mut v = Value::object()
                        .with("id", m.id)
                        .with("file", m.file)
                        .with("size", m.size)
                        .with("memoryMb", m.memory_mb)
                        .with("note", m.note)
                        .with("downloaded", path.is_file())
                        .with("current", current.is_some_and(|c| c == path));
                    match downloads.get(m.id) {
                        Some(Download::Running { done, total }) => {
                            v = v
                                .with("state", "downloading")
                                .with("done", *done)
                                .with("total", *total)
                        }
                        Some(Download::Checking) => v = v.with("state", "checking"),
                        Some(Download::Failed(e)) => {
                            v = v.with("state", "failed").with("error", e.as_str())
                        }
                        None => {}
                    }
                    v
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
                match me.fetch(m) {
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

    fn fetch(&self, m: &ModelInfo) -> Result<PathBuf, String> {
        let spec = FileSpec {
            name: m.file.to_string(),
            url: format!("{}{}", self.source, m.file),
            size: m.size,
            sha256: m.sha256.to_string(),
        };
        self.fetch_file(m.id, &self.dir, &spec, 0, m.size)
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
