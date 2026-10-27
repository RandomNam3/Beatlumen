//! Small shared helpers: hashing/ids, time, atomic file writes, MIME types,
//! a counting semaphore and name sanitizing.

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 64-bit FNV-1a.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Stable 16 hex character id of a string key.
pub fn id_of(key: &str) -> String {
    format!("{:016x}", fnv1a64(key.as_bytes()))
}

/// True for ids as produced by [`id_of`] / random 16 hex ids.
pub fn is_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() & 1 == 1 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok())).collect()
}

/// Constant-time equality (for equally long secrets/digests).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    std::hint::black_box(d) == 0
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Modification time in nanoseconds since the Unix epoch (0 if unknown).
pub fn mtime_ns(meta: &std::fs::Metadata) -> u64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos().min(u64::MAX as u128) as u64).unwrap_or(0)
}

/// Writes a file atomically (temporary file in the same directory, fsync,
/// rename over the target).
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Restricts a file to the owner (Unix only; no-op elsewhere).
pub fn make_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Lower case extension of a file name / path ("" if none).
pub fn ext_lower(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Content type of an audio file by extension.
pub fn audio_mime(ext: &str) -> &'static str {
    match ext {
        "mp3" | "mp2" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" | "m4b" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" | "wave" => "audio/wav",
        "aif" | "aiff" | "aifc" => "audio/aiff",
        "mka" => "audio/x-matroska",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// Image extensions served as background images.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "bmp", "gif", "webp"];

pub fn image_mime(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

pub fn image_format_ext(f: bl_image::Format) -> &'static str {
    match f {
        bl_image::Format::Png => "png",
        bl_image::Format::Jpeg => "jpg",
        bl_image::Format::Bmp => "bmp",
        bl_image::Format::Gif => "gif",
        bl_image::Format::WebP => "webp",
    }
}

/// Content type for static web client files.
pub fn static_mime(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" | "hfx" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => image_mime(ext),
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" => audio_mime(ext),
        _ => "application/octet-stream",
    }
}

/// Joins the normal components of a relative path with '/'. Returns `None`
/// for paths that contain anything but normal components or that are not
/// valid UTF-8.
pub fn rel_string(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Resolves a '/'-separated relative path below `root`, rejecting anything
/// that could escape it.
pub fn join_rel(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut p = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains(['\\', '\0']) || (cfg!(windows) && seg.contains(':')) {
            return None;
        }
        p.push(seg);
    }
    Some(p)
}

const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9",
];

fn is_windows_reserved(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("").trim().to_ascii_lowercase();
    WINDOWS_RESERVED.contains(&base.as_str())
}

/// Validates a user supplied name used directly as a file name (presets):
/// no path separators, no `..`, no control or reserved characters, 1..=100
/// characters, no leading dot, no trailing dot/space.
pub fn validate_name(name: &str) -> Result<&str, &'static str> {
    if name.is_empty() {
        return Err("name must not be empty");
    }
    if name.chars().count() > 100 {
        return Err("name is longer than 100 characters");
    }
    if name.contains("..") {
        return Err("name must not contain '..'");
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err("name contains invalid characters");
    }
    if name.starts_with('.') || name.ends_with('.') || name.starts_with(' ') || name.ends_with(' ') {
        return Err("name must not start or end with '.' or a space");
    }
    if is_windows_reserved(name) {
        return Err("name is reserved");
    }
    Ok(name)
}

/// Turns an arbitrary client supplied file name into a safe file stem:
/// strips directories, control and reserved characters, leading/trailing
/// dots and spaces, and limits the length. Never returns an empty string.
pub fn sanitize_stem(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let mut s: String = stem.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).take(80).collect();
    while s.contains("..") {
        s = s.replace("..", ".");
    }
    let s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.is_empty() {
        "image".to_string()
    } else if is_windows_reserved(&s) {
        format!("_{s}")
    } else {
        s
    }
}

/// A counting semaphore.
pub struct Semaphore {
    available: Mutex<usize>,
    cv: Condvar,
}

pub struct Permit<'a>(&'a Semaphore);

impl Semaphore {
    pub fn new(n: usize) -> Semaphore {
        Semaphore { available: Mutex::new(n.max(1)), cv: Condvar::new() }
    }
    pub fn acquire(&self) -> Permit<'_> {
        let mut a = self.available.lock().unwrap_or_else(|e| e.into_inner());
        while *a == 0 {
            a = self.cv.wait(a).unwrap_or_else(|e| e.into_inner());
        }
        *a -= 1;
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut a = self.0.available.lock().unwrap_or_else(|e| e.into_inner());
        *a += 1;
        self.0.cv.notify_one();
    }
}

/// Matches an `If-None-Match` header against an entity tag (given without
/// quotes).
pub fn etag_matches(header: Option<&str>, tag: &str) -> bool {
    let Some(h) = header else { return false };
    h.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.trim_start_matches("W/").trim_matches('"') == tag
    })
}

/// Runs `f`, converting a panic into an error message (so that stubs or bugs
/// in decoder crates never take down a connection thread silently).
pub fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown panic".into());
        crate::error!("{what} panicked: {msg}");
        format!("{what} failed (internal error)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_hex() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        let id = id_of("0/Artist/Album/01.flac");
        assert!(is_id(&id));
        assert!(!is_id("../../etc/passwd"));
        assert!(!is_id("ABCDEF0123456789"));
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])).unwrap(), vec![0, 1, 0xab, 0xff]);
        assert!(unhex("zz").is_none());
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn names() {
        assert!(validate_name("My Preset (2)").is_ok());
        for bad in ["", "../x", "a/b", "a\\b", "..", ".hidden", "x.", "con", "NUL.txt", "a\u{0}b", "a:b", &"x".repeat(101)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(sanitize_stem("../../etc/passwd.png"), "passwd");
        assert_eq!(sanitize_stem("C:\\Users\\x\\My Pic.jpeg"), "My Pic");
        assert_eq!(sanitize_stem("..."), "image");
        assert_eq!(sanitize_stem("a..b.png"), "a.b");
        assert_eq!(sanitize_stem("con.png"), "_con");
        assert_eq!(sanitize_stem("x<y>|z?.gif"), "xyz");
        assert_eq!(sanitize_stem(&"é".repeat(200)).chars().count(), 80);
    }

    #[test]
    fn rel_paths() {
        let root = Path::new("/music");
        assert_eq!(join_rel(root, "a/b.mp3").unwrap(), Path::new("/music/a/b.mp3"));
        assert!(join_rel(root, "a/../b").is_none());
        assert!(join_rel(root, "/abs").is_none());
        assert_eq!(rel_string(Path::new("a/b/c.mp3")).unwrap(), "a/b/c.mp3");
        assert!(rel_string(Path::new("../x")).is_none());
    }

    #[test]
    fn etags() {
        assert!(etag_matches(Some("\"5\""), "5"));
        assert!(etag_matches(Some("W/\"4\", \"5\""), "5"));
        assert!(etag_matches(Some("*"), "5"));
        assert!(!etag_matches(Some("\"6\""), "5"));
        assert!(!etag_matches(None, "5"));
    }
}
