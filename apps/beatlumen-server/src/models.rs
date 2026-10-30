//! Speech recognition models the admin can pick: a built-in list of the
//! whisper.cpp models (file name, size, SHA-256), downloaded in the
//! background into `<data>/models/` when picked.
//!
//! Downloads resume after an interruption (`<file>.part` and a range
//! request), are checked against the SHA-256 in the list and only then
//! renamed to their final name; the server then switches to the model
//! without a restart. Only the address of the list's source is contacted,
//! and only when the admin picks a model.

use crate::sha256::Sha256;
use crate::util;
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
}

/// The models offered (multilingual; quantised ones need less memory).
pub const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "tiny",
        file: "ggml-tiny.bin",
        size: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        memory_mb: 150,
        note: "fastest, rough",
    },
    ModelInfo {
        id: "base-q5_1",
        file: "ggml-base-q5_1.bin",
        size: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
        memory_mb: 200,
        note: "fast, small",
    },
    ModelInfo {
        id: "base",
        file: "ggml-base.bin",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        memory_mb: 300,
        note: "fast",
    },
    ModelInfo {
        id: "small-q5_1",
        file: "ggml-small-q5_1.bin",
        size: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        memory_mb: 450,
        note: "good for songs, recommended",
    },
    ModelInfo {
        id: "small",
        file: "ggml-small.bin",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        memory_mb: 800,
        note: "good for songs",
    },
    ModelInfo {
        id: "medium-q5_0",
        file: "ggml-medium-q5_0.bin",
        size: 539_212_467,
        sha256: "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
        memory_mb: 1_000,
        note: "better, slow",
    },
    ModelInfo {
        id: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size: 574_041_195,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        memory_mb: 1_100,
        note: "best, slow",
    },
];

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == id)
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
        }
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
                crate::info!("downloading the speech model {}", m.file);
                match me.fetch(m) {
                    Ok(path) => {
                        me.downloads
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(m.id);
                        crate::info!("speech model {} downloaded", m.file);
                        done(path);
                    }
                    Err(e) => {
                        crate::warn!("download of {} failed: {e}", m.file);
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
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create {}: {e}", self.dir.display()))?;
        let part = self.dir.join(format!("{}.part", m.file));
        let mut have = std::fs::metadata(&part).map(|md| md.len()).unwrap_or(0);
        if have > m.size {
            let _ = std::fs::remove_file(&part);
            have = 0;
        }
        let (status, mut body) = self.open(&format!("{}{}", self.source, m.file), have)?;
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
                    m.id,
                    Download::Running {
                        done,
                        total: m.size,
                    },
                );
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        if done != m.size {
            return Err(format!("download ended after {done} of {} bytes", m.size));
        }
        self.set(m.id, Download::Checking);
        let mut h = Sha256::default();
        let mut f = std::fs::File::open(&part).map_err(|e| e.to_string())?;
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        if util::hex(&h.finish()) != m.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err("the file does not match its checksum".into());
        }
        let path = self.path(m);
        std::fs::rename(&part, &path)
            .map_err(|e| format!("cannot rename {}: {e}", part.display()))?;
        Ok(path)
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
