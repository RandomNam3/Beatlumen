//! Static web client: files from a directory, files embedded at build time
//! (`OPENCHROMA_EMBED_WEB`), or a built-in placeholder page.

use crate::util;
use oc_net::server::{Request, Response};
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
<title>OpenChroma Server</title>
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
<h1>OpenChroma Server is running</h1>
<p>The web client is not installed on this server. Connect with an OpenChroma app,
or start the server with <code>--web-dir</code> pointing to the built web client.</p>
<p>Server information: <a href="/api/info">/api/info</a></p>
</main>
</body>
</html>
"#;

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
        if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("web"))).filter(|d| d.is_dir()) {
            return WebSource::Dir(d);
        }
        if !EMBEDDED_WEB.is_empty() {
            return WebSource::Embedded(EMBEDDED_WEB.iter().map(|(p, d)| (*p, (*d, format!("{:016x}", util::fnv1a64(d))))).collect());
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
            if *s == "." || *s == ".." || s.starts_with('.') || s.contains(['\\', ':', '\0']) || s.chars().any(char::is_control) {
                return Response::error(400, "invalid path");
            }
        }
        let rel = segs.join("/");
        let has_ext = segs.last().is_some_and(|s| s.contains('.'));
        match self {
            WebSource::Placeholder => {
                if rel.is_empty() || rel == "index.html" || !has_ext {
                    Response::bytes(200, "text/html; charset=utf-8", PLACEHOLDER.as_bytes().to_vec()).with_header("Cache-Control", "no-cache")
                } else {
                    Response::error(404, "not found")
                }
            }
            WebSource::Embedded(files) => {
                let key = if rel.is_empty() { "index.html".to_string() } else { rel.clone() };
                let found = files.get(key.as_str()).map(|f| (key.clone(), f)).or_else(|| {
                    let idx = if rel.is_empty() { "index.html".to_string() } else { format!("{rel}/index.html") };
                    files.get(idx.as_str()).map(|f| (idx.clone(), f))
                });
                // Client side routes fall back to the main page.
                let found = found.or_else(|| (!has_ext).then(|| files.get("index.html").map(|f| ("index.html".to_string(), f))).flatten());
                match found {
                    Some((path, (data, etag))) => {
                        if util::etag_matches(req.header("if-none-match"), etag) {
                            return Response::new(304).with_header("ETag", format!("\"{etag}\""));
                        }
                        let ext = util::ext_lower(Path::new(&path));
                        Response::bytes(200, util::static_mime(&ext), data.to_vec()).with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "no-cache")
                    }
                    None => Response::error(404, "not found"),
                }
            }
            WebSource::Dir(root) => {
                let Some(mut path) = (if rel.is_empty() { Some(root.clone()) } else { util::join_rel(root, &rel) }) else {
                    return Response::error(400, "invalid path");
                };
                if path.is_dir() {
                    path = path.join("index.html");
                }
                if !path.is_file() && !has_ext {
                    path = root.join("index.html");
                }
                let Ok(meta) = std::fs::metadata(&path) else { return Response::error(404, "not found") };
                if !meta.is_file() {
                    return Response::error(404, "not found");
                }
                let etag = format!("{:016x}", util::fnv1a64(format!("{}:{}", util::mtime_ns(&meta), meta.len()).as_bytes()));
                if util::etag_matches(req.header("if-none-match"), &etag) {
                    return Response::new(304).with_header("ETag", format!("\"{etag}\""));
                }
                let ext = util::ext_lower(&path);
                Response::file(&path, util::static_mime(&ext), req.header("range")).with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "no-cache")
            }
        }
    }
}
