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
pub fn handle(state: &std::sync::Arc<State>, req: Request) -> Response {
    let t0 = Instant::now();
    let is_api = req.path == "/api" || req.path.starts_with("/api/");
    let mut resp = match check_origin(state, &req) {
        Err(refused) => refused,
        Ok(()) => util::catch("request handler", || route(state, &req))
            .unwrap_or_else(|_| Response::error(500, "internal server error")),
    };
    finish_headers(&mut resp, is_api);
    if crate::log::verbose() || resp.status >= 500 {
        let remote = req.remote.map(|a| a.to_string()).unwrap_or_default();
        // The query string is not logged (it may contain a token).
        crate::info!(
            "{remote} {} {} -> {} ({} ms)",
            req.method,
            req.path,
            resp.status,
            t0.elapsed().as_millis()
        );
    }
    resp
}

/// Whether `host` (without port) is a name the server answers to.
fn host_allowed(state: &State, host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') {
        return true; // IP literal: no DNS involved
    }
    h == "localhost"
        || [".localhost", ".local", ".lan", ".home.arpa", ".internal"]
            .iter()
            .any(|s| h.ends_with(s))
        || state.config().allowed_hosts.contains(&h)
        || state.config().public_url.as_deref().is_some_and(|u| {
            let rest = u.split_once("://").map_or(u, |(_, r)| r);
            let authority = rest.split('/').next().unwrap_or("");
            host_name(authority).eq_ignore_ascii_case(&h)
        })
}

/// Strips the port from a `Host` header value (`[v6]:port`, `name:port`).
fn host_name(v: &str) -> &str {
    if let Some(rest) = v.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    v.rsplit_once(':').map_or(v, |(h, p)| {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            v
        }
    })
}

/// Rejects requests from other web sites: cross-site writes (CSRF) always,
/// unknown host names (DNS rebinding) when no password protects the server
/// and always for the admin interface.
fn check_origin(state: &State, req: &Request) -> Result<(), Response> {
    let host = req.header("host").unwrap_or("");
    let admin = req.path.starts_with("/admin") || req.path.starts_with("/api/admin");
    if (admin || !state.auth.required()) && !host_allowed(state, host_name(host)) {
        crate::warn!("refused request for unknown host name {host:?} (allow it with --allow-host)");
        return Err(Response::error(
            421,
            "unknown host name; start the server with --allow-host for it",
        ));
    }
    let writes = !matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS");
    if writes {
        let cross_site = req
            .header("sec-fetch-site")
            .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"));
        let foreign_origin = req.header("origin").is_some_and(|o| {
            let o = o.split_once("://").map_or(o, |(_, rest)| rest);
            o == "null" || !o.eq_ignore_ascii_case(host)
        });
        if cross_site || foreign_origin {
            return Err(Response::error(403, "cross-site request refused"));
        }
    }
    Ok(())
}

fn finish_headers(resp: &mut Response, is_api: bool) {
    let h = &mut resp.headers;
    h.set("X-Content-Type-Options", "nosniff");
    h.set("Referrer-Policy", "no-referrer");
    h.set("Cross-Origin-Resource-Policy", "same-origin");
    if h.get("content-type")
        .is_some_and(|t| t.starts_with("text/html"))
    {
        // Pages with a stricter policy of their own keep it.
        if h.get("content-security-policy").is_none() {
            h.set("Content-Security-Policy", CSP);
        }
        h.set("Cross-Origin-Opener-Policy", "same-origin");
        h.set("Cross-Origin-Embedder-Policy", "require-corp");
        h.set("X-Frame-Options", "DENY");
    }
    if is_api && h.get("cache-control").is_none() {
        h.set("Cache-Control", "no-store");
    }
}

fn route(state: &std::sync::Arc<State>, req: &Request) -> Response {
    if !req.path.starts_with('/') || req.path.contains(['\0', '\\']) {
        return Response::error(400, "invalid path");
    }
    let segs = req.segments();
    // Android lets the app use passkeys of this domain only when the domain
    // names the app (Digital Asset Links).
    if segs == [".well-known", "assetlinks.json"] && matches!(req.method.as_str(), "GET" | "HEAD") {
        return assetlinks(state);
    }
    match segs.as_slice() {
        ["api", "admin", rest @ ..] => crate::admin::handle(state, req, rest),
        ["api", rest @ ..] => api(state, req, rest),
        ["admin", rest @ ..] => crate::admin_web::serve(req, rest),
        // Public links (/share/<id>#<secret>) open the share page.
        ["share", id] if !id.contains('.') => state.web.serve(req, &["share", "index.html"]),
        _ => state.web.serve(req, &segs),
    }
}

/// Largest settings document of an account (encrypted, bytes).
const MAX_ACCOUNT_SETTINGS: usize = 256 * 1024;

/// `GET/PUT /api/vault/settings`: an account's settings as the apps
/// encrypted them (the server cannot read them). A change is announced to
/// the account's other devices.
fn account_settings(state: &State, req: &Request) -> Response {
    let Some(user) = state.vault.session_user(req) else {
        return Response::error(401, "authentication required");
    };
    // In the database, with the account's other data.
    let key = format!("{}settings", crate::store::scope(&user));
    match req.method.as_str() {
        "GET" | "HEAD" => match state.store.get(&key) {
            Some(d) => Response::bytes(200, "application/octet-stream", d),
            None => Response::error(404, "no settings yet"),
        },
        "PUT" => {
            if req.body.is_empty() || req.body.len() > MAX_ACCOUNT_SETTINGS {
                return Response::error(413, "settings too large");
            }
            if let Err(e) = state.store.put(&key, &req.body) {
                crate::warn!("cannot store settings of {user}: {e}");
                return Response::error(500, "cannot store");
            }
            state.devices.notify(
                &format!("user:{user}"),
                Value::object()
                    .with("type", "settings")
                    .with("from", req.query("device").unwrap_or("")),
            );
            Response::new(204)
        }
        _ => not_allowed("GET, HEAD, PUT"),
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
    req.query("size")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn api(state: &State, req: &Request, segs: &[&str]) -> Response {
    let m = req.method.as_str();
    let get = m == "GET" || m == "HEAD";
    match segs {
        ["info"] => {
            return if get {
                json(200, &state.info().to_json())
            } else {
                not_allowed("GET, HEAD")
            }
        }
        // For container health checks and monitoring: no login needed, no
        // details of the setup.
        ["health"] => {
            return if get {
                let problems = state.health_problems();
                let status = if problems.is_empty() { 200 } else { 503 };
                let v = Value::object()
                    .with("status", if problems.is_empty() { "ok" } else { "failing" })
                    .with("problems", problems);
                json(status, &v).with_header("Cache-Control", "no-store")
            } else {
                not_allowed("GET, HEAD")
            }
        }
        ["login"] => {
            return if m == "POST" {
                login(state, req)
            } else {
                not_allowed("POST")
            }
        }
        // Settings that follow an account (encrypted by the apps).
        ["vault", "settings"] => return account_settings(state, req),
        ["vault", rest @ ..] => return state.vault.handle(req, rest, client_ip(req)),
        // Public links to encrypted files: no account needed.
        ["share", rest @ ..] => return state.vault.public_link(req, rest, client_ip(req)),
        // A room's code, or the token it gave.
        ["rooms", "join"] => {
            return if m == "POST" {
                state.rooms.join(state, req, client_ip(req))
            } else {
                not_allowed("POST")
            }
        }
        ["rooms", id, rest @ ..] => return state.rooms.member_request(state, req, id, rest),
        _ => {}
    }
    // Users with an account may use the library as well; their playlists
    // and presets are their own.
    let user = state.vault.session_user(req);
    // A guest of a room: that room's songs and nothing else.
    let guest = state.rooms.guest(req);
    if let Some(g) = &guest {
        if !state.rooms.guest_may(g, m, segs) {
            return Response::error(403, "not for a guest of a room");
        }
    } else if !state.auth.authorized(req) && user.is_none() {
        return Response::error(401, "authentication required")
            .with_header("WWW-Authenticate", "Bearer realm=\"Beatlumen\"");
    }
    let store = user.as_deref().map(|u| state.user_data.get(u));
    let playlists = store.as_ref().map_or(&state.playlists, |s| &s.playlists);
    let presets = store.as_ref().map_or(&state.presets, |s| &s.presets);
    let favorites = store.as_ref().map_or(&state.favorites, |s| &s.favorites);
    let history = store.as_ref().map_or(&state.history, |s| &s.history);
    let song_labels = store.as_ref().map_or(&state.labels, |s| &s.labels);
    let login = crate::recommend::Login {
        favorites,
        history,
        playlists,
        labels: song_labels,
    };
    // Devices see the devices of the same login: an account, or everyone
    // with the server password.
    if matches!(segs, ["devices", ..] | ["events"] | ["time"]) {
        let scope = match &guest {
            Some(g) => format!("room:{}", g.room),
            None => user
                .as_deref()
                .map_or_else(|| "shared".to_string(), |u| format!("user:{u}")),
        };
        let rooms = state
            .rooms
            .scopes_of(req.query("device").unwrap_or(""), &scope);
        return state.devices.handle(req, segs, &scope, &rooms);
    }
    if segs == ["rooms"] {
        if m != "POST" {
            return not_allowed("POST");
        }
        let scope = user
            .as_deref()
            .map_or_else(|| "shared".to_string(), |u| format!("user:{u}"));
        return state.rooms.create(state, req, &scope);
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
        ["library", "hidden"] if get => hidden_list(state, req),
        ["library", "remove"] if req.method == "POST" => hide_tracks(state, req, true),
        ["library", "restore"] if req.method == "POST" => hide_tracks(state, req, false),
        ["extensions", rest @ ..] => state.extensions.handle(state, req, rest),
        ["library"] => not_allowed("GET, HEAD"),
        ["rescan"] if m == "POST" => {
            state.request_scan();
            json(202, &Value::object().with("scanning", true))
        }
        ["rescan"] => not_allowed("POST"),
        ["tracks", id, "lyrics", "sync"] => {
            let idx = state.library();
            let Some(e) = util::is_id(id).then(|| idx.track(id)).flatten() else {
                return Response::error(404, "track not found");
            };
            if !state.lyrics_sync.available() {
                return Response::error(501, "speech recognition is not set up on this server");
            }
            match m {
                "GET" | "HEAD" => json(200, &state.lyrics_sync.status_json(&e.track.id)),
                "POST" => {
                    state.lyrics_sync.request(&e.track.id);
                    json(202, &state.lyrics_sync.status_json(&e.track.id))
                }
                _ => not_allowed("GET, HEAD, POST"),
            }
        }
        ["tracks", id, "lyrics"] => {
            let idx = state.library();
            let Some(e) = util::is_id(id).then(|| idx.track(id)).flatten() else {
                return Response::error(404, "track not found");
            };
            match m {
                "GET" | "HEAD" => match state.lyrics.get(e) {
                    Some(v) => json(200, &v).with_header("Cache-Control", "private, no-cache"),
                    None => Response::error(404, "no song text"),
                },
                "PUT" => match state.lyrics.put(e, &req.body) {
                    Ok(()) => Response::new(204),
                    Err(err) => fail(err),
                },
                "DELETE" => {
                    state.lyrics.delete(e);
                    Response::new(204)
                }
                _ => not_allowed("GET, HEAD, PUT, DELETE"),
            }
        }
        ["tracks", id, "labels"] => {
            let idx = state.library();
            if idx.track(id).is_none() {
                return Response::error(404, "track not found");
            }
            match m {
                "GET" | "HEAD" => json(200, &Value::from(song_labels.of(id))),
                "PUT" => match song_labels.change(id, &req.body) {
                    Ok(names) => json(200, &Value::from(names)),
                    Err(e) => fail(e),
                },
                _ => not_allowed("GET, HEAD, PUT"),
            }
        }
        ["tracks", id, what] => {
            if !get {
                return not_allowed("GET, HEAD");
            }
            let idx = state.library();
            let Some(e) = util::is_id(id).then(|| idx.track(id)).flatten() else {
                return Response::error(404, "track not found");
            };
            match *what {
                "stream" => stream(state, req, e),
                "cover" => cover(state, req, &idx, e),
                "analysis" => analysis(state, req, e),
                "loudness" => match state.analyzer.loudness(e) {
                    Ok(body) => Response::bytes(200, "application/json", body.into_bytes())
                        .with_header("Cache-Control", "private, max-age=3600"),
                    Err(err) => fail(err),
                },
                _ => Response::error(404, "not found"),
            }
        }
        ["albums", id, "cover"] => {
            if !get {
                return not_allowed("GET, HEAD");
            }
            let idx = state.library();
            let Some(album) = util::is_id(id).then(|| idx.album(id)).flatten() else {
                return Response::error(404, "album not found");
            };
            match album.cover_track.as_deref().and_then(|t| idx.track(t)) {
                Some(e) => cover(state, req, &idx, e),
                None => Response::error(404, "album has no cover"),
            }
        }
        ["playlists"] => match m {
            "GET" | "HEAD" => {
                let mut list = playlists.list_json();
                if let Value::Array(a) = &mut list {
                    let folder = state
                        .folder_playlists
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    a.extend(folder.iter().map(|p| p.to_json().with("folder", true)));
                }
                json(200, &list)
            }
            "POST" => match playlists.create(&req.body) {
                Ok(p) => json(201, &p.to_json()),
                Err(e) => fail(e),
            },
            _ => not_allowed("GET, HEAD, POST"),
        },
        ["playlists", id] => {
            if !util::is_id(id) {
                return Response::error(404, "playlist not found");
            }
            let folder = state
                .folder_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .find(|p| p.id == *id)
                .cloned();
            if let Some(p) = folder {
                return match m {
                    "GET" | "HEAD" => json(200, &p.to_json().with("folder", true)),
                    _ => Response::error(
                        403,
                        "this playlist is a file in a music folder; change it there",
                    ),
                };
            }
            match m {
                "GET" | "HEAD" => match playlists.get(id) {
                    Some(p) => json(200, &p.to_json()),
                    None => Response::error(404, "playlist not found"),
                },
                "PUT" => match playlists.update(id, &req.body) {
                    Ok(p) => json(200, &p.to_json()),
                    Err(e) => fail(e),
                },
                "DELETE" => match playlists.delete(id) {
                    Ok(()) => Response::new(204),
                    Err(e) => fail(e),
                },
                _ => not_allowed("GET, HEAD, PUT, DELETE"),
            }
        }
        ["artist-info"] if get => {
            let name = req.query("name").unwrap_or("");
            let language = req.query("lang").unwrap_or("en");
            let lib = state.library();
            // The artist's id, when the tags of one of their songs name it.
            let mbid = lib
                .tracks()
                .iter()
                .map(|e| &e.track)
                .find(|t| {
                    t.artists.len() == 1
                        && !t.meta.mb_artist.is_empty()
                        && t.artists[0].eq_ignore_ascii_case(name)
                })
                .map(|t| t.meta.mb_artist.clone());
            json(200, &state.artists.info(name, mbid, language))
        }
        ["artist-info"] => not_allowed("GET, HEAD"),
        ["labels"] if get => json(200, &crate::recommend::labels(state, &login)),
        ["labels"] => not_allowed("GET, HEAD"),
        ["recommendations"] if get => json(200, &crate::recommend::recommendations(state, &login)),
        ["recommendations"] => not_allowed("GET, HEAD"),
        ["stats"] if get => {
            let range = req.query("range").unwrap_or("all");
            let offset = req
                .query("utcOffset")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            json(200, &crate::recommend::stats(state, &login, range, offset))
        }
        ["stats"] => not_allowed("GET, HEAD"),
        // Speech models for the apps (when the admin offers them).
        ["models"] if get => json(200, &offered_models(state)),
        ["models"] => not_allowed("GET, HEAD"),
        ["models", id, file] if get => model_file(state, req, id, file),
        ["models", _, _] => not_allowed("GET, HEAD"),
        ["radio"] if m == "POST" => match crate::recommend::radio(state, &login, &req.body) {
            Ok(v) => json(200, &v),
            Err(e) => fail(e),
        },
        ["radio"] => not_allowed("POST"),
        ["favorites"] if get => json(200, &favorites.list_json()),
        ["favorites"] => not_allowed("GET, HEAD"),
        ["favorites", id] => match m {
            "PUT" => match favorites.add(id, &req.body) {
                Ok(()) => Response::new(204),
                Err(e) => fail(e),
            },
            "DELETE" => match favorites.remove(id) {
                Ok(()) => Response::new(204),
                Err(e) => fail(e),
            },
            _ => not_allowed("PUT, DELETE"),
        },
        ["history"] => match m {
            "GET" | "HEAD" => {
                let since = req.query("since").and_then(|s| s.parse().ok()).unwrap_or(0);
                let limit = size_param(req, 1000, 1, 10_000) as usize;
                json(200, &history.list_json(since, limit))
            }
            "POST" => match history.add(&req.body) {
                Ok(n) => json(200, &Value::object().with("added", n)),
                Err(e) => fail(e),
            },
            "DELETE" => match history.clear() {
                Ok(()) => Response::new(204),
                Err(e) => fail(e),
            },
            _ => not_allowed("GET, HEAD, POST, DELETE"),
        },
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
        ["presets"] if get => json(200, &presets.list()),
        ["presets"] => not_allowed("GET, HEAD"),
        ["presets", ..] => {
            // The name is everything after the prefix (already decoded); a
            // decoded '/' makes it invalid instead of changing the route.
            let name = req
                .path
                .trim_end_matches('/')
                .strip_prefix("/api/presets/")
                .unwrap_or("");
            match m {
                "GET" | "HEAD" => match presets.get(name) {
                    Ok(data) => Response::bytes(200, "application/json", data),
                    Err(e) => fail(e),
                },
                "PUT" => match presets.put(name, &req.body) {
                    Ok(created) => json(
                        if created { 201 } else { 200 },
                        &Value::object().with("name", name),
                    ),
                    Err(e) => fail(e),
                },
                "DELETE" => match presets.delete(name) {
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
pub(crate) fn client_ip(req: &Request) -> Option<std::net::IpAddr> {
    let peer = req.remote.map(|a| a.ip())?;
    if peer.is_loopback() {
        let forwarded = req
            .header("x-forwarded-for")
            .and_then(|h| h.rsplit(',').next())
            .or_else(|| req.header("x-real-ip"));
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
        Err(LoginError::TooMany) => {
            Response::error(429, "too many failed login attempts; try again later")
        }
        Err(LoginError::Internal(msg)) => Response::error(500, &msg),
    }
}

fn library(state: &State, req: &Request) -> Response {
    let idx = state.library();
    let etag = idx.revision.to_string();
    if etag_matches(req.header("if-none-match"), &etag) {
        return Response::new(304)
            .with_header("ETag", format!("\"{etag}\""))
            .with_header("Cache-Control", "no-cache");
    }
    Response::bytes(200, "application/json", idx.json().to_vec())
        .with_header("ETag", format!("\"{etag}\""))
        .with_header("Cache-Control", "no-cache")
}

/// What this account took out of its library: everything that stays hidden, and
/// what can still come back (the trash).
fn hidden_list(state: &State, req: &Request) -> Response {
    Response::json(
        200,
        &trash_list(state, &state.account_of(req), util::now_secs()),
    )
}

/// How long is left to take a removal back. Over for a song of an extension: for
/// this account it is gone from then on, and neither this list nor a restore may
/// tell whether it is still in somebody else's library. The admin's folders stay
/// (`lasting`): such a song can always come back.
pub fn trash_list(state: &State, account: &str, now: u64) -> Value {
    let idx = state.library();
    let items: Vec<Value> = state
        .hidden_of(account, |h| h.all())
        .into_iter()
        .filter_map(|(id, at)| {
            let e = idx.track(&id)?;
            let left = trash_left(at, now);
            let lasting = !state.extensions.is_import(&e.abs);
            (left > 0 || lasting).then(|| {
                Value::object()
                    .with("id", id.as_str())
                    .with("removed", at)
                    .with("secondsLeft", left)
                    .with("lasting", lasting)
                    .with("title", e.track.title.as_str())
                    .with("artist", e.track.artists.join(", "))
                    .with("album", e.track.album.as_str())
                    .with("duration", e.track.duration_ms as f64 / 1000.0)
            })
        })
        .collect();
    Value::object()
        .with("items", Value::Array(items))
        .with("foreign", state.extensions.foreign_tracks(state, account))
}

fn trash_left(removed: u64, now: u64) -> u64 {
    (crate::vault::TRASH_DAYS * 24 * 3600).saturating_sub(now.saturating_sub(removed))
}

/// `POST /api/library/remove` or `/restore` `{"ids": [...]}`: for this account
/// only. The file stays for everyone else; a song of an extension goes when it
/// is in nobody's library any more (`extensions.rs`), the admin's folders never.
fn hide_tracks(state: &State, req: &Request, hide: bool) -> Response {
    let Ok(v) = bl_json::parse_bytes(&req.body) else {
        return Response::error(400, "expected JSON");
    };
    let account = state.account_of(req);
    let ids: Vec<&str> = v
        .get("ids")
        .as_array()
        .into_iter()
        .flatten()
        .take(5000)
        .filter_map(|id| id.as_str().filter(|i| util::is_id(i)))
        .collect();
    let now = util::now_secs();
    if hide {
        remove_tracks(state, &account, &ids, now);
    } else {
        restore_tracks(state, &account, &ids, now);
    }
    Response::new(204)
}

pub fn remove_tracks(state: &State, account: &str, ids: &[&str], now: u64) {
    let idx = state.library();
    for id in ids {
        // Only what exists here.
        if let Some(e) = idx.track(id) {
            state.hidden_of(account, |h| h.add(id, now));
            state.extensions.released(&e.abs, account, now);
        }
    }
}

/// See `trash_list` for what is too late.
pub fn restore_tracks(state: &State, account: &str, ids: &[&str], now: u64) {
    let idx = state.library();
    for id in ids {
        let Some(at) = state.hidden_of(account, |h| h.removed_at(id)) else {
            continue;
        };
        let import = idx.track(id).filter(|e| state.extensions.is_import(&e.abs));
        if import.is_some() && trash_left(at, now) == 0 {
            continue;
        }
        state.hidden_of(account, |h| h.remove(id));
        if let Some(e) = import {
            state.extensions.restored(&e.abs, account);
        }
    }
}

/// `GET /api/models`: the speech models in the models folder, with their
/// files (name, size, SHA-256) for the apps to download through
/// `/api/models/<id>/<file>`; none when the admin does not offer them.
fn offered_models(state: &State) -> Value {
    if !state.config().offer_models {
        return Value::object().with("models", Value::Array(Vec::new()));
    }
    let models = &state.models;
    let file = |name: &str, size: u64, sha256: &str| {
        Value::object()
            .with("name", name)
            .with("size", size)
            .with("sha256", sha256)
    };
    let mut list: Vec<Value> = crate::models::CATALOG
        .iter()
        .filter(|m| models.path(m).is_file())
        .map(|m| {
            Value::object()
                .with("id", m.id)
                .with("note", m.note)
                .with("memoryMb", m.memory_mb)
                .with("files", Value::Array(vec![file(m.file, m.size, m.sha256)]))
        })
        .collect();
    list.extend(
        models
            .own_models(true)
            .iter()
            .map(|m| m.to_json().with("own", true)),
    );
    Value::object().with("models", Value::Array(list))
}

/// `GET /api/models/<id>/<file>`: one file of an offered model.
fn model_file(state: &State, req: &Request, id: &str, file: &str) -> Response {
    let models = &state.models;
    let known = state.config().offer_models
        && match crate::models::find(id) {
            Some(m) => m.file == file,
            None => models
                .own_models(false)
                .iter()
                .any(|m| m.id == id && m.files.iter().any(|f| f.name == file)),
        };
    if !known {
        return Response::error(404, "no such model file");
    }
    Response::file(
        &models.dir.join(file),
        "application/octet-stream",
        req.header("range"),
    )
    .with_header("Cache-Control", "private, max-age=86400")
}

fn stream(state: &State, req: &Request, e: &Entry) -> Response {
    // From the start: a song being played (not a client seeking around).
    if req
        .header("range")
        .map_or(true, |r| r.trim().starts_with("bytes=0-"))
        && req.query("start").is_none_or(|s| s == "0")
    {
        state.extensions.note_played(&e.abs);
    }
    if req.query("format") == Some("opus") {
        return transcoded(state, req, e);
    }
    if let Some((src, key)) = &e.remote {
        let range = req.header("range");
        return match state.sources.open(src, key) {
            Ok(file) => {
                if range.map_or("bytes=0-", str::trim).starts_with("bytes=0-") {
                    state.sources.played(src, key);
                }
                Response::seekable(file, util::audio_mime(&e.ext()), range)
                    .with_header("Cache-Control", "private, max-age=3600")
            }
            Err(err) => {
                crate::warn!("cannot open {}: {err}", e.track.path);
                Response::error(502, "cannot read the song from its source")
            }
        };
    }
    let resp = Response::file(&e.abs, util::audio_mime(&e.ext()), req.header("range"));
    if resp.status == 404 {
        crate::warn!("track file vanished: {}", e.abs.display());
        state.file_missing();
        return Response::error(404, "track file not found");
    }
    resp.with_header("Cache-Control", "private, max-age=3600")
}

/// The song converted to Ogg Opus while it is sent (smaller streams for
/// mobile data): `bitrate` in kbit/s, `start` in milliseconds (seeking
/// opens a new stream there). The length is not known up front, so byte
/// ranges are ignored.
fn transcoded(state: &State, req: &Request, e: &Entry) -> Response {
    use std::sync::atomic::Ordering;
    let bitrate = req
        .query("bitrate")
        .and_then(|b| b.parse::<u32>().ok())
        .unwrap_or(128)
        .clamp(24, 256);
    let start_ms = req
        .query("start")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let running = state.transcodes.fetch_add(1, Ordering::SeqCst);
    let slot = TranscodeSlot(state.transcodes.clone());
    if running >= crate::MAX_TRANSCODES {
        return Response::error(503, "too many songs are converted right now")
            .with_header("Retry-After", "5");
    }
    let src: Box<dyn bl_media::Source> = match &e.remote {
        Some((src, key)) => match state.sources.open(src, key) {
            Ok(s) => {
                if start_ms == 0 {
                    state.sources.played(src, key);
                }
                s
            }
            Err(err) => {
                crate::warn!("cannot open {}: {err}", e.track.path);
                return Response::error(502, "cannot read the song from its source");
            }
        },
        None => match std::fs::File::open(&e.abs) {
            Ok(f) => Box::new(std::io::BufReader::with_capacity(64 * 1024, f)),
            Err(_) => {
                crate::warn!("track file vanished: {}", e.abs.display());
                state.file_missing();
                return Response::error(404, "track file not found");
            }
        },
    };
    let mut decoder = match bl_media::open(src, Some(&e.ext())) {
        Ok(d) => d,
        Err(err) => {
            crate::debug!("cannot decode {}: {err}", e.rel);
            return Response::error(415, "cannot decode the song");
        }
    };
    if start_ms > 0 {
        let frame = start_ms * decoder.spec().sample_rate as u64 / 1000;
        if let Err(err) = decoder.seek(frame) {
            crate::debug!("cannot seek in {}: {err}", e.rel);
            return Response::error(416, "cannot start the song there");
        }
    }
    let mut opts = bl_opus_enc::TranscodeOptions::new(bitrate * 1000);
    // The same bytes when the client reconnects (it reads the start again
    // while finding out the format).
    opts.serial = Some(e.track.id.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    }));
    match bl_opus_enc::Transcoder::with_options(decoder, &opts) {
        Ok(t) => {
            let mut r = Response::new(200);
            r.body = bl_net::server::Body::Stream(Box::new(Slotted(t, slot)));
            r.with_header("Content-Type", "audio/ogg")
                .with_header("Cache-Control", "no-store")
                .with_header("X-Beatlumen-Transcoded", "opus")
        }
        Err(err) => {
            crate::debug!("cannot convert {}: {err}", e.rel);
            Response::error(415, "cannot convert the song")
        }
    }
}

/// Counts a running conversion until the response is sent (or dropped).
struct TranscodeSlot(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for TranscodeSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct Slotted<R>(R, #[allow(dead_code)] TranscodeSlot);

impl<R: std::io::Read> std::io::Read for Slotted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

/// Serves a cover thumbnail of a track (cached per size and source stamp).
fn cover(state: &State, req: &Request, idx: &Index, e: &Entry) -> Response {
    let size = size_param(req, 300, 32, 1024);
    let stamp = covers::cover_stamp(idx, e);
    let etag = format!("{stamp:016x}-{size}");
    let finish = |r: Response| {
        r.with_header("ETag", format!("\"{etag}\""))
            .with_header("Cache-Control", "private, max-age=3600")
    };
    if etag_matches(req.header("if-none-match"), &etag) {
        return finish(Response::new(304));
    }
    let cache = state
        .data
        .covers
        .join(format!("{}_{size}_{stamp:016x}.jpg", e.track.id));
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let _permit = state.thumbs.acquire();
    if cache.is_file() {
        return finish(Response::file(&cache, "image/jpeg", None));
    }
    let Some(src) = covers::cover_source(&state.sources, idx, e) else {
        return Response::error(404, "no cover");
    };
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
        Ok(blob) => Response::bytes(200, "application/octet-stream", blob)
            .with_header("Cache-Control", "private, max-age=3600"),
        Err(err) => fail(err),
    }
}

fn image(state: &State, req: &Request, id: &str) -> Response {
    let Some(item) = state.images.find(id) else {
        return Response::error(404, "image not found");
    };
    let wants_scaled = req
        .query("size")
        .is_some_and(|s| !s.trim().is_empty() && s.trim() != "0");
    // Backgrounds ask with anim=1: animations are sent as they are (the
    // client decodes their frames at screen size), still images scaled.
    let keep_animation = req.query("anim").is_some_and(|v| v.trim() == "1")
        && matches!(
            util::ext_lower(&item.path).as_str(),
            "gif" | "png" | "apng" | "webp"
        )
        && covers::read_background_file(&item.path).is_some_and(|d| bl_image::is_animated(&d));
    if !wants_scaled || keep_animation {
        let resp = Response::file(
            &item.path,
            util::image_mime(&util::ext_lower(&item.path)),
            req.header("range"),
        );
        if resp.status == 404 {
            state.images.invalidate();
            return Response::error(404, "image not found");
        }
        return resp.with_header("Cache-Control", "private, max-age=3600");
    }
    let size = size_param(req, 1920, 64, 8192);
    let cache = state.images.scaled_path(&item, size);
    let etag = cache
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let finish = |r: Response| {
        r.with_header("ETag", format!("\"{etag}\""))
            .with_header("Cache-Control", "private, max-age=3600")
    };
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
    let Some(src) = covers::read_background_file(&item.path) else {
        return Response::error(404, "image not found");
    };
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

fn assetlinks(state: &State) -> Response {
    let certs = state.config().all_android_certs();
    if certs.is_empty() || state.vault.rp.is_none() {
        return Response::error(404, "not found");
    }
    let fingerprints: Vec<Value> = certs
        .iter()
        .map(|c| {
            let pairs: Vec<String> = (0..c.len() / 2)
                .map(|i| c[i * 2..i * 2 + 2].to_ascii_uppercase())
                .collect();
            Value::from(pairs.join(":"))
        })
        .collect();
    let v = Value::Array(vec![Value::object()
        .with(
            "relation",
            Value::Array(vec![
                Value::from("delegate_permission/common.handle_all_urls"),
                Value::from("delegate_permission/common.get_login_creds"),
            ]),
        )
        .with(
            "target",
            Value::object()
                .with("namespace", "android_app")
                .with("package_name", "io.github.randomnam3.beatlumen")
                .with("sha256_cert_fingerprints", Value::Array(fingerprints)),
        )]);
    Response::json(200, &v).with_header("Cache-Control", "public, max-age=3600")
}
