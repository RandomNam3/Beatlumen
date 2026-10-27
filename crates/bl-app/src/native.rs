//! Host services shared by the native platforms (Linux, Windows, Android):
//! file based storage, local file system access, HTTP via `bl-net` on
//! background threads and thread spawning.

use crate::host::{DirEntry, HttpRequest, HttpResponse};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct NativeServices {
    pub data_dir: PathBuf,
    start: Instant,
    next_id: u64,
    tx: Sender<(u64, Result<HttpResponse, String>)>,
    rx: Receiver<(u64, Result<HttpResponse, String>)>,
}

/// Maps a storage key ("settings.json", "presets/My Preset.hfx") to a path
/// below `dir`, rejecting traversal.
fn key_path(dir: &Path, key: &str) -> Option<PathBuf> {
    let mut p = dir.to_path_buf();
    for part in key.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') || part.contains(':') {
            return None;
        }
        p.push(part);
    }
    Some(p)
}

impl NativeServices {
    pub fn new(data_dir: PathBuf) -> NativeServices {
        let _ = std::fs::create_dir_all(&data_dir);
        let (tx, rx) = channel();
        NativeServices { data_dir, start: Instant::now(), next_id: 1, tx, rx }
    }

    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn unix_time(&self) -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    }

    pub fn random_seed(&self) -> u64 {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        let stack = &t as *const u64 as u64;
        t ^ stack.rotate_left(32) ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }

    pub fn storage_read(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(key_path(&self.data_dir, key)?).ok()
    }

    /// Writes atomically (temporary file + rename).
    pub fn storage_write(&self, key: &str, data: &[u8]) -> bool {
        let Some(path) = key_path(&self.data_dir, key) else { return false };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("tmp~");
        std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, &path).is_ok()
    }

    pub fn storage_delete(&self, key: &str) {
        if let Some(p) = key_path(&self.data_dir, key) {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn storage_list(&self, prefix: &str) -> Vec<String> {
        // Lists files in the directory part of the prefix.
        let (dir, file_prefix) = match prefix.rfind('/') {
            Some(i) => (&prefix[..i], &prefix[i + 1..]),
            None => ("", prefix),
        };
        let base = if dir.is_empty() { Some(self.data_dir.clone()) } else { key_path(&self.data_dir, dir) };
        let mut out = Vec::new();
        if let Some(base) = base {
            if let Ok(rd) = std::fs::read_dir(base) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with(file_prefix) && e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        out.push(if dir.is_empty() { name } else { format!("{dir}/{name}") });
                    }
                }
            }
        }
        out.sort();
        out
    }

    pub fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            let Ok(md) = std::fs::metadata(&p) else { continue };
            out.push(DirEntry {
                name,
                path: p.to_string_lossy().into_owned(),
                is_dir: md.is_dir(),
                size: md.len(),
                modified: md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
            });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    pub fn fs_metadata(&self, path: &str) -> Option<DirEntry> {
        let md = std::fs::metadata(path).ok()?;
        let p = Path::new(path);
        Some(DirEntry {
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string()),
            path: path.to_string(),
            is_dir: md.is_dir(),
            size: md.len(),
            modified: md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
        })
    }

    pub fn fs_open(&self, path: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        Ok(Box::new(std::io::BufReader::with_capacity(64 * 1024, f)))
    }

    pub fn fs_read(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }

    pub fn http_request(&mut self, req: HttpRequest) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let opts = bl_net::client::Options::default();
            let res = bl_net::client::request(&req.method, &req.url, &req.headers, &req.body, &opts)
                .map(|r| HttpResponse { status: r.status, headers: r.headers.0, body: r.body });
            let _ = tx.send((id, res));
        });
        id
    }

    pub fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)> {
        self.rx.try_iter().collect()
    }

    pub fn http_open_stream(&self, url: &str, headers: &[(String, String)]) -> Result<Box<dyn bl_media::Source>, String> {
        let s = bl_net::client::HttpStream::open(url, headers)?;
        Ok(Box::new(std::io::BufReader::with_capacity(64 * 1024, s)))
    }

    pub fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        let _ = std::thread::Builder::new().name("bl-worker".into()).spawn(job);
    }
}

/// Default per-user data directory for desktop platforms.
pub fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("BEATLUMEN_DATA") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Beatlumen");
        }
    }
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("beatlumen");
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".config").join("beatlumen");
    }
    PathBuf::from("beatlumen-data")
}

/// File browser roots for desktop platforms.
pub fn desktop_roots() -> Vec<(String, String)> {
    let mut roots = Vec::new();
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok();
    if let Some(h) = &home {
        roots.push(("Home".to_string(), h.clone()));
        for (label, sub) in [("Music", "Music"), ("Pictures", "Pictures"), ("Downloads", "Downloads")] {
            let p = Path::new(h).join(sub);
            if p.is_dir() {
                roots.push((label.to_string(), p.to_string_lossy().into_owned()));
            }
        }
    }
    #[cfg(windows)]
    {
        for letter in b'C'..=b'Z' {
            let d = format!("{}:\\", letter as char);
            if Path::new(&d).exists() {
                roots.push((d.clone(), d));
            }
        }
    }
    #[cfg(not(windows))]
    {
        roots.push(("/".to_string(), "/".to_string()));
        for m in ["/media", "/mnt"] {
            if Path::new(m).is_dir() {
                roots.push((m.to_string(), m.to_string()));
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_roundtrip_and_traversal() {
        let dir = std::env::temp_dir().join(format!("bl-native-test-{}", std::process::id()));
        let s = NativeServices::new(dir.clone());
        assert!(s.storage_write("a/b.json", b"x"));
        assert_eq!(s.storage_read("a/b.json").unwrap(), b"x");
        assert_eq!(s.storage_list("a/"), vec!["a/b.json".to_string()]);
        assert!(!s.storage_write("../evil", b"x"));
        assert!(s.storage_read("a/../a/b.json").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
