//! Background images from the configured image folders plus uploads in
//! `data/uploads`.
//!
//! Ids are FNV-1a hashes like track ids: `<image folder index>/<relative
//! path>` for configured folders and `uploads/<file name>` for uploads (so
//! upload ids survive changes of the folder list). The listing is cached for
//! a few seconds and rebuilt after uploads and deletions.

use crate::util::{self, id_of};
use bl_json::Value;
use bl_net::api::ImageEntry;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Largest accepted upload.
pub const MAX_UPLOAD: usize = 50 * 1024 * 1024;
const INDEX_TTL: Duration = Duration::from_secs(15);
const MAX_DEPTH: usize = 16;
const MAX_IMAGES: usize = 200_000;

pub type Failure = (u16, String);

#[derive(Debug, Clone)]
pub struct ImageItem {
    pub entry: ImageEntry,
    pub path: PathBuf,
    pub upload: bool,
    pub mtime_ns: u64,
}

pub struct ImageIndex {
    built: Instant,
    pub items: Vec<ImageItem>,
    by_id: HashMap<String, usize>,
}

pub struct Images {
    dirs: Vec<PathBuf>,
    uploads: PathBuf,
    pub cache_dir: PathBuf,
    index: Mutex<Option<Arc<ImageIndex>>>,
    upload_lock: Mutex<()>,
}

fn is_image_name(name: &str) -> bool {
    let ext = util::ext_lower(Path::new(name));
    util::IMAGE_EXTENSIONS.contains(&ext.as_str())
}

/// Recursively lists image files below `root` as (relative path, path, metadata).
fn walk(root: &Path, recursive: bool, out: &mut Vec<(String, PathBuf, std::fs::Metadata)>) {
    let mut visited = HashSet::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if !visited.insert(std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone())) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<_> = rd
            .flatten()
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .collect();
        items.sort();
        for (name, path) in items.into_iter().rev() {
            if name.starts_with('.') || out.len() >= MAX_IMAGES {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if meta.is_dir() {
                if recursive && depth < MAX_DEPTH && name != "@eaDir" {
                    stack.push((path, child, depth + 1));
                }
            } else if meta.is_file() && is_image_name(&name) {
                out.push((child, path, meta));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
}

impl Images {
    pub fn new(dirs: Vec<PathBuf>, uploads: PathBuf, cache_dir: PathBuf) -> Images {
        Images {
            dirs,
            uploads,
            cache_dir,
            index: Mutex::new(None),
            upload_lock: Mutex::new(()),
        }
    }

    fn build(&self) -> ImageIndex {
        let mut items = Vec::new();
        let mut add = |key: String,
                       name: &str,
                       folder: String,
                       path: PathBuf,
                       meta: &std::fs::Metadata,
                       upload: bool| {
            let mtime_ns = util::mtime_ns(meta);
            items.push(ImageItem {
                entry: ImageEntry {
                    id: id_of(&key),
                    name: name.to_string(),
                    folder,
                    size: meta.len(),
                    modified: mtime_ns / 1_000_000_000,
                },
                path,
                upload,
                mtime_ns,
            });
        };
        for (i, root) in self.dirs.iter().enumerate() {
            let root_name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            let mut files = Vec::new();
            walk(root, true, &mut files);
            for (rel, path, meta) in files {
                let (parent, name) = rel.rsplit_once('/').unwrap_or(("", &rel));
                let folder = if parent.is_empty() {
                    root_name.clone()
                } else {
                    format!("{root_name}/{parent}")
                };
                add(
                    format!("{i}/{rel}"),
                    name,
                    folder,
                    path.clone(),
                    &meta,
                    false,
                );
            }
        }
        let mut files = Vec::new();
        walk(&self.uploads, false, &mut files);
        for (rel, path, meta) in files {
            add(
                format!("uploads/{rel}"),
                &rel.clone(),
                "uploads".into(),
                path.clone(),
                &meta,
                true,
            );
        }
        let mut by_id = HashMap::new();
        items.retain(|it| by_id.insert(it.entry.id.clone(), ()).is_none());
        let by_id = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.entry.id.clone(), i))
            .collect();
        ImageIndex {
            built: Instant::now(),
            items,
            by_id,
        }
    }

    /// The current listing (rebuilt when older than a few seconds).
    pub fn index(&self) -> Arc<ImageIndex> {
        self.index_max_age(INDEX_TTL)
    }

    fn index_max_age(&self, max_age: Duration) -> Arc<ImageIndex> {
        let mut g = self.index.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some(idx) if idx.built.elapsed() < max_age => idx.clone(),
            _ => {
                let idx = Arc::new(self.build());
                *g = Some(idx.clone());
                idx
            }
        }
    }

    pub fn invalidate(&self) {
        *self.index.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn list_json(&self) -> Value {
        Value::Array(
            self.index()
                .items
                .iter()
                .map(|i| i.entry.to_json())
                .collect(),
        )
    }

    /// Looks up an image; an unknown id triggers one refresh of a listing
    /// that is more than two seconds old.
    pub fn find(&self, id: &str) -> Option<ImageItem> {
        let idx = self.index();
        if let Some(&i) = idx.by_id.get(id) {
            return Some(idx.items[i].clone());
        }
        let idx = self.index_max_age(Duration::from_secs(2));
        idx.by_id.get(id).map(|&i| idx.items[i].clone())
    }

    /// Cache file of a scaled version.
    pub fn scaled_path(&self, item: &ImageItem, size: u32) -> PathBuf {
        let stamp = util::fnv1a64(format!("{}:{}", item.mtime_ns, item.entry.size).as_bytes());
        self.cache_dir
            .join(format!("{}_{size}_{stamp:016x}.jpg", item.entry.id))
    }

    /// Stores an uploaded image (raw body, client file name from `X-Filename`).
    pub fn upload(&self, filename: Option<&str>, body: &[u8]) -> Result<ImageEntry, Failure> {
        if body.is_empty() {
            return Err((400, "empty upload".into()));
        }
        if body.len() > MAX_UPLOAD {
            return Err((
                413,
                format!("image larger than {} MB", MAX_UPLOAD / (1024 * 1024)),
            ));
        }
        let format = bl_image::probe_format(body).ok_or((
            415,
            "not a supported image format (JPEG, PNG, BMP, GIF, WebP)".to_string(),
        ))?;
        let raw = filename
            .map(|f| bl_net::http::percent_decode(f, false))
            .unwrap_or_default();
        let stem = util::sanitize_stem(&raw);
        let ext = util::image_format_ext(format);
        std::fs::create_dir_all(&self.uploads)
            .map_err(|e| (500, format!("cannot create upload folder: {e}")))?;
        let _g = self.upload_lock.lock().unwrap_or_else(|e| e.into_inner());
        for n in 1..=1000 {
            let name = if n == 1 {
                format!("{stem}.{ext}")
            } else {
                format!("{stem} ({n}).{ext}")
            };
            let path = self.uploads.join(&name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(body).and_then(|_| f.sync_all()) {
                        drop(f);
                        let _ = std::fs::remove_file(&path);
                        return Err((500, format!("cannot store upload: {e}")));
                    }
                    drop(f);
                    self.invalidate();
                    let meta = std::fs::metadata(&path).ok();
                    crate::info!("stored uploaded image {name} ({} bytes)", body.len());
                    return Ok(ImageEntry {
                        id: id_of(&format!("uploads/{name}")),
                        name,
                        folder: "uploads".into(),
                        size: body.len() as u64,
                        modified: meta
                            .map(|m| util::mtime_ns(&m) / 1_000_000_000)
                            .unwrap_or_else(util::now_secs),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err((500, format!("cannot store upload: {e}"))),
            }
        }
        Err((409, "too many files with this name".into()))
    }

    /// Deletes an uploaded image (configured folders are read-only).
    pub fn delete(&self, id: &str) -> Result<(), Failure> {
        let item = self.find(id).ok_or((404, "image not found".to_string()))?;
        if !item.upload {
            return Err((403, "only uploaded images can be deleted".into()));
        }
        std::fs::remove_file(&item.path).map_err(|e| (500, format!("cannot delete image: {e}")))?;
        self.invalidate();
        if let Ok(rd) = std::fs::read_dir(&self.cache_dir) {
            let prefix = format!("{id}_");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(())
    }
}
