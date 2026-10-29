//! Static web client: files from a directory, files embedded at build time
//! (`BEATLUMEN_EMBED_WEB`), or a built-in placeholder page.
//!
//! Pre-compressed copies (`x.gz` next to `x`) are sent to clients that
//! accept gzip. Embedded builds keep only the compressed copy of such
//! files; clients without gzip get it decompressed.
//!
//! Integrity: when the web client comes with `manifest.json` (SHA-256 of
//! every file) and, in official builds, `manifest.sig` (Ed25519 signature
//! with the release key), the server checks every file against it at start
//! and again whenever a file in the directory changes, and refuses to serve
//! files that were modified or added (see `docs/ENCRYPTION.md`).

use crate::util;
use bl_net::server::{Request, Response};
use bl_vault::manifest::{self, Manifest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/embedded_web.rs"));

pub enum WebSource {
    Dir(PathBuf),
    /// Embedded files: path -> (contents, etag).
    Embedded(HashMap<&'static str, (&'static [u8], String)>),
    Placeholder,
}

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Beatlumen Server</title>
<style>
body { font-family: system-ui, sans-serif; background: #111318; color: #e8e8ee; margin: 0; display: grid; place-items: center; min-height: 100vh; }
main { max-width: 34rem; padding: 2rem; }
h1 { font-weight: 600; margin-top: 0; }
code { background: #22252d; padding: 0.1em 0.35em; border-radius: 4px; }
a { color: #8fb4ff; }
</style>
</head>
<body>
<main>
<h1>Beatlumen Server is running</h1>
<p>The web client is not installed on this server. Connect with an Beatlumen app,
or start the server with <code>--web-dir</code> pointing to the built web client.</p>
<p>Server information: <a href="/api/info">/api/info</a></p>
</main>
</body>
</html>
"#;

/// Whether the client accepts gzip-encoded responses.
fn accepts_gzip(req: &Request) -> bool {
    accepts_gzip_header(req.header("accept-encoding"))
}

fn accepts_gzip_header(header: Option<&str>) -> bool {
    header.is_some_and(|h| {
        h.split(',').any(|item| {
            let mut parts = item.split(';');
            let name = parts.next().unwrap_or("").trim();
            let q_zero = parts.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q <= 0.0))
            });
            name.eq_ignore_ascii_case("gzip") && !q_zero
        })
    })
}

/// [`gunzip`] for the release tools.
pub fn gunzip_for_check(data: &[u8]) -> Option<Vec<u8>> {
    gunzip(data)
}

/// Decompresses a gzip file (RFC 1952) for clients without gzip support.
fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return None;
    }
    let flags = data[3];
    let mut pos = 10;
    if flags & 4 != 0 {
        let xlen = u16::from_le_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2 + xlen;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            pos += data.get(pos..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    let size = u32::from_le_bytes(data[data.len() - 4..].try_into().ok()?) as usize;
    let out =
        bl_image::inflate::inflate_with_limit(data.get(pos..data.len() - 8)?, 512 << 20).ok()?;
    (out.len() as u32 == size as u32).then_some(out)
}

/// The web client and the result of its integrity check.
pub struct WebClient {
    pub source: WebSource,
    pub integrity: Integrity,
}

impl WebClient {
    pub fn resolve(explicit: Option<&Path>) -> WebClient {
        let source = WebSource::resolve(explicit);
        let integrity = Integrity::check(&source);
        WebClient { source, integrity }
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.source.describe(), self.integrity.describe())
    }

    pub fn serve(&self, req: &Request, segs: &[&str]) -> Response {
        self.source.serve(req, segs, &self.integrity)
    }
}

pub struct Integrity {
    manifest: Option<Manifest>,
    /// The manifest's signature is valid (or this build has no release key).
    trusted: bool,
    pub status: &'static str,
    /// Check results: file -> (stamp of the file(s) on disk, matches).
    seen: Mutex<HashMap<String, (u64, bool)>>,
}

fn modified_response() -> Response {
    Response::error(
        503,
        "this file of the web client does not match its integrity manifest and is not served",
    )
}

impl Integrity {
    fn check(source: &WebSource) -> Integrity {
        let read = |name: &str| -> Option<Vec<u8>> {
            match source {
                WebSource::Dir(d) => std::fs::read(d.join(name)).ok(),
                WebSource::Embedded(f) => f.get(name).map(|(d, _)| d.to_vec()),
                WebSource::Placeholder => None,
            }
        };
        let mut i = Integrity {
            manifest: None,
            trusted: true,
            status: if matches!(source, WebSource::Placeholder) {
                "none"
            } else {
                "unchecked"
            },
            seen: Mutex::new(HashMap::new()),
        };
        let Some(bytes) = read(manifest::MANIFEST) else {
            if !matches!(source, WebSource::Placeholder) {
                crate::warn!("web client has no integrity manifest: files are served unchecked");
            }
            return i;
        };
        let m = match Manifest::parse(&bytes) {
            Ok(m) => m,
            Err(e) => {
                crate::error!("web client manifest is invalid ({e}): no file is served");
                i.manifest = Some(Manifest::default());
                i.trusted = false;
                i.status = "modified";
                return i;
            }
        };
        let signed = match manifest::release_key() {
            Some(key) => {
                let sig =
                    read(manifest::SIGNATURE).map(|s| String::from_utf8_lossy(&s).into_owned());
                let ok = sig.is_some_and(|s| manifest::verify(&key, &bytes, &s).is_ok());
                if !ok {
                    crate::error!(
                        "web client manifest signature is missing or invalid: no file is served"
                    );
                    i.trusted = false;
                }
                ok
            }
            None => false,
        };
        let names: Vec<String> = m.files.keys().cloned().collect();
        i.manifest = Some(m);
        let mut bad: Vec<String> = names
            .iter()
            .filter(|n| !i.allows(source, n))
            .cloned()
            .collect();
        // Files that are not in the manifest are never served; mention them.
        let listed = |n: &str| {
            i.manifest.as_ref().is_some_and(|m| {
                m.files.contains_key(n)
                    || n.strip_suffix(".gz")
                        .is_some_and(|b| m.files.contains_key(b))
            }) || n == manifest::MANIFEST
                || n == manifest::SIGNATURE
        };
        let present: Vec<String> = match source {
            WebSource::Dir(d) => {
                let mut v = Vec::new();
                list_files(d, "", 0, &mut v);
                v
            }
            WebSource::Embedded(f) => f.keys().map(|k| k.to_string()).collect(),
            WebSource::Placeholder => Vec::new(),
        };
        bad.extend(
            present
                .into_iter()
                .filter(|n| !listed(n))
                .map(|n| format!("{n} (not listed)")),
        );
        i.status = if !i.trusted || !bad.is_empty() {
            "modified"
        } else if signed {
            "verified"
        } else {
            "unsigned"
        };
        if bad.is_empty() {
            crate::info!(
                "web client integrity: {} files match the {}manifest",
                names.len(),
                if signed { "signed " } else { "" }
            );
        } else {
            crate::error!(
                "web client integrity: modified files are not served: {}",
                bad.join(", ")
            );
        }
        i
    }

    fn describe(&self) -> &'static str {
        match self.status {
            "verified" => "integrity verified with the release signature",
            "unsigned" => "files match an unsigned manifest",
            "modified" => "MODIFIED FILES ARE NOT SERVED",
            "unchecked" => "no integrity manifest",
            _ => "no web client",
        }
    }

    /// Whether the file `key` (path relative to the web root) may be served.
    fn allows(&self, source: &WebSource, key: &str) -> bool {
        let Some(m) = &self.manifest else {
            return true;
        };
        if key == manifest::MANIFEST || key == manifest::SIGNATURE {
            return true;
        }
        if !self.trusted || !m.files.contains_key(key) {
            return false;
        }
        let gz_key = format!("{key}.gz");
        let stamp = match source {
            WebSource::Dir(d) => {
                let st = |p: &Path| {
                    std::fs::metadata(p)
                        .map_or(0, |md| util::mtime_ns(&md) ^ md.len().rotate_left(32) ^ 1)
                };
                st(&d.join(key)) ^ st(&d.join(&gz_key)).rotate_left(7)
            }
            _ => 1,
        };
        if let Some((s, ok)) = self.seen.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
            if *s == stamp {
                return *ok;
            }
        }
        let (plain, gz) = match source {
            WebSource::Dir(d) => (
                std::fs::read(d.join(key)).ok(),
                std::fs::read(d.join(&gz_key)).ok(),
            ),
            WebSource::Embedded(f) => (
                f.get(key).map(|(d, _)| d.to_vec()),
                f.get(gz_key.as_str()).map(|(d, _)| d.to_vec()),
            ),
            WebSource::Placeholder => (None, None),
        };
        // Every copy that may be served must match.
        let ok = (plain.is_some() || gz.is_some())
            && plain.as_ref().is_none_or(|p| m.matches(key, p))
            && gz
                .as_ref()
                .is_none_or(|g| gunzip(g).is_some_and(|p| m.matches(key, &p)));
        if !ok {
            crate::error!("web client file {key} does not match the integrity manifest");
        }
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), (stamp, ok));
        ok
    }
}

fn list_files(dir: &Path, rel: &str, depth: usize, out: &mut Vec<String>) {
    if depth > 16 {
        return;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => list_files(&e.path(), &r, depth + 1, out),
            Ok(t) if t.is_file() => out.push(r),
            _ => {}
        }
    }
}

impl WebSource {
    /// Resolves the source: explicit directory, `<exe dir>/web`, embedded
    /// files, placeholder.
    pub fn resolve(explicit: Option<&Path>) -> WebSource {
        if let Some(d) = explicit {
            if !d.is_dir() {
                crate::warn!("web directory {} does not exist", d.display());
            }
            return WebSource::Dir(d.to_path_buf());
        }
        if let Some(d) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.join("web")))
            .filter(|d| d.is_dir())
        {
            return WebSource::Dir(d);
        }
        if !EMBEDDED_WEB.is_empty() {
            return WebSource::Embedded(
                EMBEDDED_WEB
                    .iter()
                    .map(|(p, d)| (*p, (*d, format!("{:016x}", util::fnv1a64(d)))))
                    .collect(),
            );
        }
        WebSource::Placeholder
    }

    pub fn describe(&self) -> String {
        match self {
            WebSource::Dir(d) => format!("directory {}", d.display()),
            WebSource::Embedded(m) => format!("{} embedded files", m.len()),
            WebSource::Placeholder => "built-in placeholder page (no web client installed)".into(),
        }
    }

    /// Serves a static file for the (already percent-decoded) path segments.
    pub fn serve(&self, req: &Request, segs: &[&str], guard: &Integrity) -> Response {
        if !matches!(req.method.as_str(), "GET" | "HEAD") {
            return Response::error(405, "method not allowed").with_header("Allow", "GET, HEAD");
        }
        for s in segs {
            if *s == "."
                || *s == ".."
                || s.starts_with('.')
                || s.contains(['\\', ':', '\0'])
                || s.chars().any(char::is_control)
            {
                return Response::error(400, "invalid path");
            }
        }
        let rel = segs.join("/");
        let has_ext = segs.last().is_some_and(|s| s.contains('.'));
        match self {
            WebSource::Placeholder => {
                if rel.is_empty() || rel == "index.html" || !has_ext {
                    Response::bytes(
                        200,
                        "text/html; charset=utf-8",
                        PLACEHOLDER.as_bytes().to_vec(),
                    )
                    .with_header("Cache-Control", "no-cache")
                } else {
                    Response::error(404, "not found")
                }
            }
            WebSource::Embedded(files) => {
                // A file may be embedded only gzip-compressed ("x.gz").
                let has = |k: &str| {
                    files.contains_key(k) || files.contains_key(format!("{k}.gz").as_str())
                };
                let key = if rel.is_empty() {
                    "index.html".to_string()
                } else {
                    rel.clone()
                };
                let found = if has(&key) {
                    Some(key)
                } else {
                    let idx = if rel.is_empty() {
                        "index.html".to_string()
                    } else {
                        format!("{rel}/index.html")
                    };
                    // Client side routes fall back to the main page.
                    if has(&idx) {
                        Some(idx)
                    } else if !has_ext && has("index.html") {
                        Some("index.html".to_string())
                    } else {
                        None
                    }
                };
                let Some(path) = found else {
                    return Response::error(404, "not found");
                };
                if !guard.allows(self, &path) {
                    return modified_response();
                }
                let mime = util::static_mime(&util::ext_lower(Path::new(&path)));
                let gz = files.get(format!("{path}.gz").as_str());
                if let Some((data, etag)) =
                    gz.filter(|_| accepts_gzip(req) || !files.contains_key(path.as_str()))
                {
                    let etag = format!("{etag}-gz");
                    if util::etag_matches(req.header("if-none-match"), &etag) {
                        return Response::new(304).with_header("ETag", format!("\"{etag}\""));
                    }
                    let resp = if accepts_gzip(req) {
                        Response::bytes(200, mime, data.to_vec())
                            .with_header("Content-Encoding", "gzip")
                    } else {
                        match gunzip(data) {
                            Some(plain) => Response::bytes(200, mime, plain),
                            None => return Response::error(500, "cannot decompress"),
                        }
                    };
                    return resp
                        .with_header("ETag", format!("\"{etag}\""))
                        .with_header("Vary", "Accept-Encoding")
                        .with_header("Cache-Control", "no-cache");
                }
                let Some((data, etag)) = files.get(path.as_str()) else {
                    return Response::error(404, "not found");
                };
                if util::etag_matches(req.header("if-none-match"), etag) {
                    return Response::new(304).with_header("ETag", format!("\"{etag}\""));
                }
                let mut resp = Response::bytes(200, mime, data.to_vec())
                    .with_header("ETag", format!("\"{etag}\""))
                    .with_header("Cache-Control", "no-cache");
                if gz.is_some() {
                    resp = resp.with_header("Vary", "Accept-Encoding");
                }
                resp
            }
            WebSource::Dir(root) => {
                let Some(mut path) = (if rel.is_empty() {
                    Some(root.clone())
                } else {
                    util::join_rel(root, &rel)
                }) else {
                    return Response::error(400, "invalid path");
                };
                if path.is_dir() {
                    path = path.join("index.html");
                }
                if !path.is_file() && !has_ext {
                    path = root.join("index.html");
                }
                let Ok(meta) = std::fs::metadata(&path) else {
                    return Response::error(404, "not found");
                };
                if !meta.is_file() {
                    return Response::error(404, "not found");
                }
                let key = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(util::rel_string)
                    .unwrap_or_default();
                if !guard.allows(self, &key) {
                    return modified_response();
                }
                let etag = format!(
                    "{:016x}",
                    util::fnv1a64(format!("{}:{}", util::mtime_ns(&meta), meta.len()).as_bytes())
                );
                if util::etag_matches(req.header("if-none-match"), &etag) {
                    return Response::new(304).with_header("ETag", format!("\"{etag}\""));
                }
                let ext = util::ext_lower(&path);
                let mut gz_name = path.as_os_str().to_os_string();
                gz_name.push(".gz");
                let gz = PathBuf::from(gz_name);
                if accepts_gzip(req) && gz.is_file() {
                    return Response::file(&gz, util::static_mime(&ext), None)
                        .with_header("Content-Encoding", "gzip")
                        .with_header("ETag", format!("\"{etag}-gz\""))
                        .with_header("Vary", "Accept-Encoding")
                        .with_header("Cache-Control", "no-cache");
                }
                Response::file(&path, util::static_mime(&ext), req.header("range"))
                    .with_header("ETag", format!("\"{etag}\""))
                    .with_header("Cache-Control", "no-cache")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_negotiation() {
        assert!(accepts_gzip_header(Some("gzip, deflate, br")));
        assert!(accepts_gzip_header(Some("br;q=1.0, GZIP;q=0.5")));
        assert!(!accepts_gzip_header(Some("gzip;q=0, deflate")));
        assert!(!accepts_gzip_header(Some("identity")));
        assert!(!accepts_gzip_header(None));
    }

    #[test]
    fn gunzip_roundtrip() {
        let plain: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        // Header with a file name (FNAME), deflate data, CRC (unchecked), size.
        let mut gz = vec![0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 0, 3];
        gz.extend_from_slice(b"beatlumen.wasm\0");
        gz.extend(bl_image::deflate::deflate(&plain, 6));
        gz.extend_from_slice(&[0; 4]);
        gz.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        assert_eq!(gunzip(&gz).as_deref(), Some(&plain[..]));
        // Wrong size or truncated data is rejected.
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n - 4] ^= 1;
        assert!(gunzip(&bad).is_none());
        assert!(gunzip(&gz[..20]).is_none());
        assert!(gunzip(b"not gzip at all, really").is_none());
    }
}
