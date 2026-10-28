//! Static web client: files from a directory, files embedded at build time
//! (`BEATLUMEN_EMBED_WEB`), or a built-in placeholder page.
//!
//! Pre-compressed copies (`x.gz` next to `x`) are sent to clients that
//! accept gzip. Embedded builds keep only the compressed copy of such
//! files; clients without gzip get it decompressed.

use crate::util;
use bl_net::server::{Request, Response};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    pub fn serve(&self, req: &Request, segs: &[&str]) -> Response {
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
