//! HTTP routing: the `/api/*` endpoints and the static web client.
//!
//! Every request runs under `catch_unwind` (a panic becomes a 500 response
//! instead of silently killing the connection thread), and security headers
//! are added to every response.

use crate::covers;
use crate::library::{Entry, Index};
use crate::util::{self, etag_matches};
use crate::State;
use bl_json::Value;
use bl_net::server::{Request, Response};
use std::time::Instant;

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
media-src 'self' blob:; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// Entry point for every request.
pub fn handle(state: &State, req: Request) -> Response {
    let t0 = Instant::now();
    let is_api = req.path == "/api" || req.path.starts_with("/api/");
    let mut resp = match util::catch("request handler", || route(state, &req)) {
        Ok(r) => r,
        Err(_) => Response::error(500, "internal server error"),
    };
    finish_headers(&mut resp, is_api);
    if crate::log::verbose() || resp.status >= 500 {
        let remote = req.remote.map(|a| a.to_string()).unwrap_or_default();
        // The query string is not logged (it may contain a token).
        crate::info!("{remote} {} {} -> {} ({} ms)", req.method, req.path, resp.status, t0.elapsed().as_millis());
    }
    resp
}

fn finish_headers(resp: &mut Response, is_api: bool) {
    let h = &mut resp.headers;
    h.set("X-Content-Type-Options", "nosniff");
    h.set("Referrer-Policy", "no-referrer");
    h.set("Cross-Origin-Resource-Policy", "same-origin");
    if h.get("content-type").is_some_and(|t| t.starts_with("text/html")) {
        h.set("Content-Security-Policy", CSP);
        h.set("Cross-Origin-Opener-Policy", "same-origin");
        h.set("Cross-Origin-Embedder-Policy", "require-corp");
        h.set("X-Frame-Options", "DENY");
    }
    if is_api && h.get("cache-control").is_none() {
        h.set("Cache-Control", "no-store");
    }
}

fn route(state: &State, req: &Request) -> Response {
    if !req.path.starts_with('/') || req.path.contains(['\0', '\\']) {
        return Response::error(400, "invalid path");
    }
    let segs = req.segments();
    match segs.first() {
        Some(&"api") => api(state, req, &segs[1..]),
        _ => state.web.serve(req, &segs),
    }
}

fn not_allowed(allow: &str) -> Response {
    Response::error(405, "method not allowed").with_header("Allow", allow)
}

fn fail((code, msg): (u16, String)) -> Response {
    Response::error(code, &msg)
}

fn json(status: u16, v: &Value) -> Response {
    Response::json(status, v)
}

/// Parses `?size=` with a default and clamping.
fn size_param(req: &Request, default: u32, min: u32, max: u32) -> u32 {
    req.query("size").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(default).clamp(min, max)
}

fn api(state: &State, req: &Request, segs: &[&str]) -> Response {
    let m = req.method.as_str();
    let get = m == "GET" || m == "HEAD";
    match segs {
        ["info"] => return if get { json(200, &state.info().to_json()) } else { not_allowed("GET, HEAD") },
        ["login"] => return if m == "POST" { login(state, req) } else { not_allowed("POST") },
        _ => {}
    }
    if !state.auth.authorized(req) {
        return Response::error(401, "authentication required").with_header("WWW-Authenticate", "Bearer realm=\"Beatlumen\"");
    }
    match segs {
        ["logout"] => match m {
            "POST" => {
                state.auth.logout(req);
                Response::new(204)
            }
            _ => not_allowed("POST"),
        },
        ["library"] if get => library(state, req),
        ["library"] => not_allowed("GET, HEAD"),
        ["rescan"] if m == "POST" => {
            state.request_scan();
            json(202, &Value::object().with("scanning", true))
        }
        ["rescan"] => not_allowed("POST"),
        ["tracks", id, what] => {
            if !get {
                return not_allowed("GET, HEAD");
            }
            let idx = state.library();
            let Some(e) = util::is_id(id).then(|| idx.track(id)).flatten() else { return Response::error(404, "track not found") };
            match *what {
                "stream" => stream(state, req, e),
                "cover" => cover(state, req, &idx, e),
                "analysis" => analysis(state, req, e),
                _ => Response::error(404, "not found"),
            }
        }
        ["albums", id, "cover"] => {
            if !get {
                return not_allowed("GET, HEAD");
            }
            let idx = state.library();
            let Some(album) = util::is_id(id).then(|| idx.album(id)).flatten() else { return Response::error(404, "album not found") };
            match album.cover_track.as_deref().and_then(|t| idx.track(t)) {
                Some(e) => cover(state, req, &idx, e),
                None => Response::error(404, "album has no cover"),
            }
        }
        ["playlists"] => match m {
            "GET" | "HEAD" => json(200, &state.playlists.list_json()),
            "POST" => match state.playlists.create(&req.body) {
                Ok(p) => json(201, &p.to_json()),
                Err(e) => fail(e),
            },
            _ => not_allowed("GET, HEAD, POST"),
        },
        ["playlists", id] => {
            if !util::is_id(id) {
                return Response::error(404, "playlist not found");
            }
            match m {
                "GET" | "HEAD" => match state.playlists.get(id) {
                    Some(p) => json(200, &p.to_json()),
                    None => Response::error(404, "playlist not found"),
                },
                "PUT" => match state.playlists.update(id, &req.body) {
                    Ok(p) => json(200, &p.to_json()),
                    Err(e) => fail(e),
                },
                "DELETE" => match state.playlists.delete(id) {
                    Ok(()) => Response::new(204),
                    Err(e) => fail(e),
                },
                _ => not_allowed("GET, HEAD, PUT, DELETE"),
            }
        }
        ["images"] => match m {
            "GET" | "HEAD" => json(200, &state.images.list_json()),
            "POST" => match state.images.upload(req.header("x-filename"), &req.body) {
                Ok(e) => json(201, &e.to_json()),
                Err(e) => fail(e),
            },
            _ => not_allowed("GET, HEAD, POST"),
        },
        ["images", id] => {
            if !util::is_id(id) {
                return Response::error(404, "image not found");
            }
            match m {
                "GET" | "HEAD" => image(state, req, id),
                "DELETE" => match state.images.delete(id) {
                    Ok(()) => Response::new(204),
                    Err(e) => fail(e),
                },
                _ => not_allowed("GET, HEAD, DELETE"),
            }
        }
        ["presets"] if get => json(200, &state.presets.list()),
        ["presets"] => not_allowed("GET, HEAD"),
        ["presets", ..] => {
            // The name is everything after the prefix (already decoded); a
            // decoded '/' makes it invalid instead of changing the route.
            let name = req.path.trim_end_matches('/').strip_prefix("/api/presets/").unwrap_or("");
            match m {
                "GET" | "HEAD" => match state.presets.get(name) {
                    Ok(data) => Response::bytes(200, "application/json", data),
                    Err(e) => fail(e),
                },
                "PUT" => match state.presets.put(name, &req.body) {
                    Ok(created) => json(if created { 201 } else { 200 }, &Value::object().with("name", name)),
                    Err(e) => fail(e),
                },
                "DELETE" => match state.presets.delete(name) {
                    Ok(()) => Response::new(204),
                    Err(e) => fail(e),
                },
                _ => not_allowed("GET, HEAD, PUT, DELETE"),
            }
        }
        _ => Response::error(404, "not found"),
    }
}

/// The client address used for login rate limiting. Behind a reverse proxy
/// on the same machine (peer is loopback) the proxy's `X-Forwarded-For` /
/// `X-Real-IP` is used; remote peers cannot spoof it.
fn client_ip(req: &Request) -> Option<std::net::IpAddr> {
    let peer = req.remote.map(|a| a.ip())?;
    if peer.is_loopback() {
        let forwarded = req.header("x-forwarded-for").and_then(|h| h.rsplit(',').next()).or_else(|| req.header("x-real-ip"));
        if let Some(ip) = forwarded.and_then(|v| v.trim().parse().ok()) {
            return Some(ip);
        }
    }
    Some(peer)
}

fn login(state: &State, req: &Request) -> Response {
    use crate::auth::LoginError;
    match state.auth.login(&req.body, client_ip(req)) {
        Ok(token) => json(200, &Value::object().with("token", token)),
        Err(LoginError::BadRequest(msg)) => Response::error(400, &msg),
        Err(LoginError::Wrong) => Response::error(401, "wrong password"),
        Err(LoginError::TooMany) => Response::error(429, "too many failed login attempts; try again later"),
        Err(LoginError::Internal(msg)) => Response::error(500, &msg),
    }
}

fn library(state: &State, req: &Request) -> Response {
    let idx = state.library();
    let etag = idx.revision.to_string();
    if etag_matches(req.header("if-none-match"), &etag) {
        return Response::new(304).with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "no-cache");
    }
    Response::bytes(200, "application/json", idx.json().to_vec()).with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "no-cache")
}

fn stream(state: &State, req: &Request, e: &Entry) -> Response {
    let resp = Response::file(&e.abs, util::audio_mime(&e.ext()), req.header("range"));
    if resp.status == 404 {
        crate::warn!("track file vanished: {}", e.abs.display());
        state.file_missing();
        return Response::error(404, "track file not found");
    }
    resp.with_header("Cache-Control", "private, max-age=3600")
}

/// Serves a cover thumbnail of a track (cached per size and source stamp).
fn cover(state: &State, req: &Request, idx: &Index, e: &Entry) -> Response {
    let size = size_param(req, 300, 32, 1024);
    let stamp = covers::cover_stamp(idx, e);
    let etag = format!("{stamp:016x}-{size}");
    let finish = |r: Response| r.with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "private, max-age=3600");
    if etag_matches(req.header("if-none-match"), &etag) {
        return finish(Response::new(304));
    }
    let cache = state.data.covers.join(format!("{}_{size}_{stamp:016x}.jpg", e.track.id));
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let _permit = state.thumbs.acquire();
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let Some(src) = covers::cover_source(idx, e) else { return Response::error(404, "no cover") };
    match covers::make_thumbnail(&src, size) {
        Ok(jpeg) => {
            if let Err(err) = util::write_atomic(&cache, &jpeg) {
                crate::warn!("cannot write cover cache {}: {err}", cache.display());
            }
            finish(Response::bytes(200, "image/jpeg", jpeg))
        }
        Err(msg) => {
            crate::debug!("cannot make cover thumbnail for {}: {msg}", e.rel);
            Response::error(500, &format!("cannot decode cover: {msg}"))
        }
    }
}

fn analysis(state: &State, req: &Request, e: &Entry) -> Response {
    let cfg = match crate::analysis::config_from_query(req.query("cfg")) {
        Ok(c) => c,
        Err(msg) => return Response::error(400, &msg),
    };
    match state.analyzer.get(e, &cfg) {
        Ok(blob) => Response::bytes(200, "application/octet-stream", blob).with_header("Cache-Control", "private, max-age=3600"),
        Err(err) => fail(err),
    }
}

fn image(state: &State, req: &Request, id: &str) -> Response {
    let Some(item) = state.images.find(id) else { return Response::error(404, "image not found") };
    let wants_scaled = req.query("size").is_some_and(|s| !s.trim().is_empty() && s.trim() != "0");
    if !wants_scaled {
        let resp = Response::file(&item.path, util::image_mime(&util::ext_lower(&item.path)), req.header("range"));
        if resp.status == 404 {
            state.images.invalidate();
            return Response::error(404, "image not found");
        }
        return resp.with_header("Cache-Control", "private, max-age=3600");
    }
    let size = size_param(req, 1920, 64, 4096);
    let cache = state.images.scaled_path(&item, size);
    let etag = cache.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let finish = |r: Response| r.with_header("ETag", format!("\"{etag}\"")).with_header("Cache-Control", "private, max-age=3600");
    if etag_matches(req.header("if-none-match"), &etag) {
        return finish(Response::new(304));
    }
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let _permit = state.thumbs.acquire();
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let Some(src) = covers::read_image_file(&item.path) else { return Response::error(404, "image not found") };
    match covers::make_thumbnail(&src, size) {
        Ok(jpeg) => {
            if let Err(err) = util::write_atomic(&cache, &jpeg) {
                crate::warn!("cannot write image cache {}: {err}", cache.display());
            }
            finish(Response::bytes(200, "image/jpeg", jpeg))
        }
        Err(msg) => Response::error(500, &format!("cannot decode image: {msg}")),
    }
}
