//! The admin interface: its login and the `/api/admin/*` endpoints (the
//! pages themselves are in [`crate::admin_web`]).
//!
//! * The admin password is set through a one-time setup link: logged at the
//!   first start (valid for a day) or created with `beatlumen-server
//!   admin-link` (valid for an hour; also resets a forgotten password).
//!   `BEATLUMEN_ADMIN_PASSWORD_FILE` sets it without a browser.
//! * Only an Argon2id verifier of the password is kept (`admin.json`), and
//!   only digests of setup tokens (`admin-setup.json`) and sessions.
//! * Sessions are cookies (`HttpOnly`, `SameSite=Strict`, `Secure` behind
//!   HTTPS) that end after 12 idle hours or 7 days; every request must
//!   carry the `X-Beatlumen-Admin` header, which other sites cannot send
//!   (no CORS), on top of the server's cross-site check.
//! * Failed logins are limited per address and slowed down.

use crate::config::{self, Config};
use crate::sha256::sha256;
use crate::util::{self, ct_eq};
use crate::State;
use bl_json::Value;
use bl_net::server::{Request, Response};
use bl_vault::keys::Verifier;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COOKIE: &str = "bl_admin";
const SESSION_IDLE: u64 = 12 * 3600;
const SESSION_MAX: u64 = 7 * 24 * 3600;
const MAX_SESSIONS: usize = 50;
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(900);
const FAILURE_DELAY: Duration = Duration::from_millis(500);
pub const MIN_PASSWORD: usize = 12;
/// Validity of the setup link logged at the first start.
pub const FIRST_SETUP_SECS: u64 = 24 * 3600;
/// Validity of a setup link from `admin-link`.
pub const LINK_SETUP_SECS: u64 = 3600;

#[derive(Clone, Copy)]
struct Session {
    created: u64,
    seen: u64,
}

#[derive(Default)]
struct Inner {
    verifier: Option<Verifier>,
    server_password: Option<Verifier>,
    sessions: HashMap<[u8; 32], Session>,
}

pub struct Admin {
    path: PathBuf,
    setup_path: PathBuf,
    inner: Mutex<Inner>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    check_lock: Mutex<()>,
    /// The password comes from a file (not changeable here).
    pub password_from_file: bool,
}

fn verifier_json(v: &Verifier) -> Value {
    Value::object()
        .with("salt", util::hex(&v.salt))
        .with("hash", util::hex(&v.hash))
        .with("m", v.params.m_kib as u64)
        .with("t", v.params.t as u64)
        .with("p", v.params.p as u64)
}

fn verifier_from(v: &Value) -> Option<Verifier> {
    Some(Verifier {
        salt: util::unhex(v.get("salt").as_str()?)?.try_into().ok()?,
        hash: util::unhex(v.get("hash").as_str()?)?.try_into().ok()?,
        params: bl_vault::keys::KdfParams {
            m_kib: v.get("m").as_u64()? as u32,
            t: v.get("t").as_u64()? as u32,
            p: v.get("p").as_u64()? as u32,
        },
    })
}

fn digest_from(s: &str) -> Option<[u8; 32]> {
    util::unhex(s)?.try_into().ok()
}

fn setup_path(data: &Path) -> PathBuf {
    data.join("admin-setup.json")
}

/// Creates a setup token valid for `secs` and returns it (the link is
/// `/admin/#setup=<token>`). Kept as a digest next to at most four others.
pub fn create_setup_token(data: &Path, secs: u64) -> Result<String, String> {
    std::fs::create_dir_all(data).map_err(|e| format!("cannot create {}: {e}", data.display()))?;
    let token = crate::random::hex(16).map_err(|e| format!("no random source: {e}"))?;
    let path = setup_path(data);
    let now = util::now_secs();
    let mut list: Vec<Value> = std::fs::read(&path)
        .ok()
        .and_then(|d| bl_json::parse_bytes(&d).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.get("expires").as_u64().is_some_and(|e| e > now))
        .collect();
    while list.len() >= 5 {
        list.remove(0);
    }
    list.push(
        Value::object()
            .with("digest", util::hex(&sha256(token.as_bytes())))
            .with("expires", now + secs),
    );
    util::write_atomic(&path, Value::Array(list).to_json().as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    util::make_private(&path);
    Ok(token)
}

impl Admin {
    pub fn open(data: &Path, password_file: Option<&Path>) -> Result<Admin, String> {
        let path = data.join("admin.json");
        let stored = std::fs::read(&path)
            .ok()
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .unwrap_or_default();
        let now = util::now_secs();
        let mut inner = Inner {
            verifier: verifier_from(stored.get("verifier")),
            server_password: verifier_from(stored.get("serverPassword")),
            sessions: HashMap::new(),
        };
        for s in stored.get("sessions").as_array().into_iter().flatten() {
            let (Some(d), Some(created), Some(seen)) = (
                s.get("digest").as_str().and_then(digest_from),
                s.get("created").as_u64(),
                s.get("seen").as_u64(),
            ) else {
                continue;
            };
            if now.saturating_sub(created) < SESSION_MAX && now.saturating_sub(seen) < SESSION_IDLE
            {
                inner.sessions.insert(d, Session { created, seen });
            }
        }
        let admin = Admin {
            path,
            setup_path: setup_path(data),
            inner: Mutex::new(Inner::default()),
            failures: Mutex::new(HashMap::new()),
            check_lock: Mutex::new(()),
            password_from_file: password_file.is_some(),
        };
        if let Some(f) = password_file {
            let text = std::fs::read_to_string(f)
                .map_err(|e| format!("admin password file {}: {e}", f.display()))?;
            let pw = text.trim_end_matches(['\r', '\n']);
            if pw.chars().count() < MIN_PASSWORD {
                return Err(format!(
                    "admin password file {}: use at least {MIN_PASSWORD} characters",
                    f.display()
                ));
            }
            if !inner.verifier.as_ref().is_some_and(|v| v.matches(pw)) {
                inner.verifier = Some(Verifier::new(pw).map_err(|e| e.to_string())?);
                inner.sessions.clear();
            }
        }
        *admin.inner.lock().unwrap_or_else(|e| e.into_inner()) = inner;
        admin.save();
        Ok(admin)
    }

    fn save(&self) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.save_locked(&inner);
    }

    fn save_locked(&self, inner: &Inner) {
        let mut v = Value::object();
        if let Some(ver) = &inner.verifier {
            v = v.with("verifier", verifier_json(ver));
        }
        if let Some(ver) = &inner.server_password {
            v = v.with("serverPassword", verifier_json(ver));
        }
        v = v.with(
            "sessions",
            Value::Array(
                inner
                    .sessions
                    .iter()
                    .map(|(d, s)| {
                        Value::object()
                            .with("digest", util::hex(d))
                            .with("created", s.created)
                            .with("seen", s.seen)
                    })
                    .collect(),
            ),
        );
        match util::write_atomic(&self.path, v.to_json_pretty().as_bytes()) {
            Ok(()) => util::make_private(&self.path),
            Err(e) => crate::warn!("cannot write {}: {e}", self.path.display()),
        }
    }

    /// No admin password yet.
    pub fn needs_setup(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .verifier
            .is_none()
    }

    /// The server password set here (None: none, or one from the
    /// configuration).
    pub fn server_password(&self) -> Option<Verifier> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .server_password
            .clone()
    }

    pub fn set_server_password(&self, v: Option<Verifier>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.server_password = v;
        self.save_locked(&inner);
    }

    /// Uses up a setup token; false when unknown or expired.
    fn take_setup_token(&self, token: &str) -> bool {
        let now = util::now_secs();
        let d = util::hex(&sha256(token.as_bytes()));
        let Some(list) = std::fs::read(&self.setup_path)
            .ok()
            .and_then(|b| bl_json::parse_bytes(&b).ok())
            .and_then(|v| v.as_array().cloned())
        else {
            return false;
        };
        let mut found = false;
        let rest: Vec<Value> = list
            .into_iter()
            .filter(|t| {
                let live = t.get("expires").as_u64().is_some_and(|e| e > now);
                let hit = ct_eq(t.str_or("digest", "").as_bytes(), d.as_bytes());
                found |= live && hit;
                live && !hit
            })
            .collect();
        let _ = util::write_atomic(&self.setup_path, Value::Array(rest).to_json().as_bytes());
        found
    }

    /// Whether a setup token is waiting (for the page's hint).
    fn setup_pending(&self) -> bool {
        let now = util::now_secs();
        std::fs::read(&self.setup_path)
            .ok()
            .and_then(|b| bl_json::parse_bytes(&b).ok())
            .and_then(|v| v.as_array().cloned())
            .is_some_and(|l| {
                l.iter()
                    .any(|t| t.get("expires").as_u64().is_some_and(|e| e > now))
            })
    }

    fn new_session(&self, inner: &mut Inner) -> Result<String, String> {
        let token = crate::random::hex(32).map_err(|e| format!("no random source: {e}"))?;
        let now = util::now_secs();
        inner.sessions.retain(|_, s| {
            now.saturating_sub(s.created) < SESSION_MAX && now.saturating_sub(s.seen) < SESSION_IDLE
        });
        while inner.sessions.len() >= MAX_SESSIONS {
            let oldest = *inner
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.seen)
                .map(|(d, _)| d)
                .expect("not empty");
            inner.sessions.remove(&oldest);
        }
        inner.sessions.insert(
            sha256(token.as_bytes()),
            Session {
                created: now,
                seen: now,
            },
        );
        self.save_locked(inner);
        Ok(token)
    }

    fn cookie_token(req: &Request) -> Option<String> {
        req.header("cookie")?.split(';').find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == COOKIE).then(|| v.trim().to_string())
        })
    }

    /// Whether the request comes from a logged-in admin (refreshes the
    /// session).
    pub fn session(&self, req: &Request) -> bool {
        let Some(t) = Self::cookie_token(req) else {
            return false;
        };
        let d = sha256(t.as_bytes());
        let now = util::now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = inner.sessions.get_mut(&d) else {
            return false;
        };
        if now.saturating_sub(s.created) >= SESSION_MAX
            || now.saturating_sub(s.seen) >= SESSION_IDLE
        {
            inner.sessions.remove(&d);
            self.save_locked(&inner);
            return false;
        }
        // Saved at most every ten minutes.
        if now.saturating_sub(s.seen) > 600 {
            s.seen = now;
            self.save_locked(&inner);
        } else {
            s.seen = now;
        }
        true
    }

    fn end_session(&self, req: &Request) {
        if let Some(t) = Self::cookie_token(req) {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.sessions.remove(&sha256(t.as_bytes())).is_some() {
                self.save_locked(&inner);
            }
        }
    }

    /// Limits failed attempts per address; Err while blocked.
    fn guard(&self, ip: IpAddr) -> Result<(), (u16, String)> {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        if f.get(&ip).is_some_and(|(n, _)| *n >= MAX_FAILURES) {
            return Err((429, "too many failed attempts; try again later".into()));
        }
        Ok(())
    }

    fn failed(&self, ip: IpAddr) {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        f.entry(ip).or_insert((0, Instant::now())).0 += 1;
        drop(f);
        crate::warn!("failed admin login from {ip}");
        std::thread::sleep(FAILURE_DELAY);
    }

    fn check_password(&self, pw: &str) -> bool {
        let v = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .verifier
            .clone();
        v.is_some_and(|v| v.matches(pw))
    }

    /// Logs in with the admin password; returns the session token.
    fn login(&self, pw: &str, ip: IpAddr) -> Result<String, (u16, String)> {
        self.guard(ip)?;
        let _serial = self.check_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.check_password(pw) {
            self.failed(ip);
            return Err((401, "wrong password".into()));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.new_session(&mut inner).map_err(|e| (500, e))
    }

    /// Sets the admin password with a setup token; ends other sessions.
    fn setup(&self, token: &str, pw: &str, ip: IpAddr) -> Result<String, (u16, String)> {
        self.guard(ip)?;
        if self.password_from_file {
            return Err((
                409,
                "the admin password is set by a file on the server".into(),
            ));
        }
        check_new_password(pw)?;
        if !self.take_setup_token(token) {
            self.failed(ip);
            return Err((403, "this setup link is invalid or expired; create a new one with `beatlumen-server admin-link`".into()));
        }
        let v = Verifier::new(pw).map_err(|e| (500, e.to_string()))?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.verifier = Some(v);
        inner.sessions.clear();
        crate::info!("admin password set");
        self.new_session(&mut inner).map_err(|e| (500, e))
    }

    /// Changes the admin password (the current one confirms); other
    /// sessions end.
    fn change_password(
        &self,
        current: &str,
        pw: &str,
        ip: IpAddr,
    ) -> Result<String, (u16, String)> {
        self.guard(ip)?;
        if self.password_from_file {
            return Err((
                409,
                "the admin password is set by a file on the server".into(),
            ));
        }
        check_new_password(pw)?;
        let _serial = self.check_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.check_password(current) {
            self.failed(ip);
            return Err((403, "the current password is wrong".into()));
        }
        let v = Verifier::new(pw).map_err(|e| (500, e.to_string()))?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.verifier = Some(v);
        inner.sessions.clear();
        crate::info!("admin password changed");
        self.new_session(&mut inner).map_err(|e| (500, e))
    }

    /// Logs the setup link when no admin password exists yet.
    pub fn announce(&self, data: &Path, addr: std::net::SocketAddr) {
        if !self.needs_setup() {
            return;
        }
        match create_setup_token(data, FIRST_SETUP_SECS) {
            Ok(t) => {
                let host = if addr.ip().is_unspecified() {
                    format!("<server address>:{}", addr.port())
                } else {
                    addr.to_string()
                };
                crate::log::write_private(
                    "INFO",
                    &format!(
                        "set the admin password within 24 hours at http://{host}/admin/#setup={t}"
                    ),
                );
            }
            Err(e) => crate::warn!("cannot create the admin setup link: {e}"),
        }
    }
}

fn check_new_password(pw: &str) -> Result<(), (u16, String)> {
    if pw.chars().count() < MIN_PASSWORD {
        return Err((400, format!("use at least {MIN_PASSWORD} characters")));
    }
    if pw.len() > 1024 {
        return Err((400, "the password is too long".into()));
    }
    Ok(())
}

fn fail((code, msg): (u16, String)) -> Response {
    Response::error(code, &msg)
}

fn body(req: &Request) -> Result<Value, (u16, String)> {
    bl_json::parse_bytes(&req.body).map_err(|e| (400, format!("invalid JSON: {e}")))
}

fn ip_of(req: &Request) -> IpAddr {
    req.remote
        .map(|a| a.ip())
        .unwrap_or(IpAddr::from([0, 0, 0, 0]))
}

/// The session cookie; `Secure` when the request came over HTTPS.
fn cookie(state: &State, req: &Request, token: &str, max_age: u64) -> String {
    let https = req
        .header("x-forwarded-proto")
        .is_some_and(|p| p.eq_ignore_ascii_case("https"))
        || state
            .config()
            .public_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://"));
    format!(
        "{COOKIE}={token}; Path=/api/admin; Max-Age={max_age}; HttpOnly; SameSite=Strict{}",
        if https { "; Secure" } else { "" }
    )
}

fn with_session(state: &State, req: &Request, token: Result<String, (u16, String)>) -> Response {
    match token {
        Ok(t) => Response::json(200, &Value::object().with("ok", true))
            .with_header("Set-Cookie", cookie(state, req, &t, SESSION_MAX)),
        Err(e) => fail(e),
    }
}

fn not_allowed(allow: &str) -> Response {
    Response::error(405, "method not allowed").with_header("Allow", allow)
}

/// `/api/admin/...`
pub fn handle(state: &Arc<State>, req: &Request, segs: &[&str]) -> Response {
    if req.header("x-beatlumen-admin").is_none() {
        return Response::error(403, "missing X-Beatlumen-Admin header");
    }
    let m = req.method.as_str();
    let admin = &state.admin;
    let ip = ip_of(req);
    match segs {
        ["state"] => {
            return Response::json(
                200,
                &Value::object()
                    .with("setup", admin.needs_setup())
                    .with("setupLink", admin.setup_pending())
                    .with("passwordFromFile", admin.password_from_file)
                    .with("loggedIn", admin.session(req))
                    .with("name", state.config().name.as_str())
                    .with("version", env!("CARGO_PKG_VERSION")),
            )
        }
        ["setup"] if m == "POST" => {
            let v = match body(req) {
                Ok(v) => v,
                Err(e) => return fail(e),
            };
            return with_session(
                state,
                req,
                admin.setup(v.str_or("token", ""), v.str_or("password", ""), ip),
            );
        }
        ["login"] if m == "POST" => {
            let v = match body(req) {
                Ok(v) => v,
                Err(e) => return fail(e),
            };
            return with_session(state, req, admin.login(v.str_or("password", ""), ip));
        }
        ["logout"] if m == "POST" => {
            admin.end_session(req);
            return Response::new(204).with_header("Set-Cookie", cookie(state, req, "", 0));
        }
        _ => {}
    }
    if !admin.session(req) {
        return Response::error(401, "admin login required");
    }
    match (m, segs) {
        ("PUT", ["password"]) => match body(req) {
            Ok(v) => with_session(
                state,
                req,
                admin.change_password(v.str_or("current", ""), v.str_or("password", ""), ip),
            ),
            Err(e) => fail(e),
        },
        ("GET", ["overview"]) => Response::json(200, &overview(state)),
        ("GET", ["settings"]) => Response::json(200, &settings(state)),
        ("PUT", ["settings"]) => match body(req).and_then(|v| save_settings(state, &v)) {
            Ok(v) => Response::json(200, &v),
            Err(e) => fail(e),
        },
        ("POST", ["restart"]) => {
            if state.restart() {
                Response::json(202, &Value::object().with("restarting", true))
            } else {
                Response::error(503, "this server cannot restart itself; restart it by hand")
            }
        }
        ("POST", ["rescan"]) => {
            state.request_scan();
            Response::new(202)
        }
        ("GET", ["log"]) => Response::json(
            200,
            &Value::Array(crate::log::recent().into_iter().map(Value::from).collect()),
        ),
        ("GET", ["users"]) => Response::json(200, &state.vault.admin_users()),
        ("DELETE", ["users", name]) => match state.vault.admin_delete_user(name) {
            Ok(()) => Response::new(204),
            Err(e) => fail(e),
        },
        ("GET", ["invites"]) => Response::json(200, &state.vault.admin_invites()),
        ("POST", ["invites"]) => {
            let days = body(req)
                .ok()
                .and_then(|v| v.get("days").as_u64())
                .unwrap_or(crate::vault::INVITE_DAYS)
                .clamp(1, 365);
            match state.vault.admin_create_invite(days) {
                Ok(code) => {
                    Response::json(201, &Value::object().with("code", code).with("days", days))
                }
                Err(e) => fail(e),
            }
        }
        ("DELETE", ["invites", id]) => match state.vault.admin_revoke_invite(id) {
            Ok(()) => Response::new(204),
            Err(e) => fail(e),
        },
        ("GET", ["links"]) => Response::json(200, &state.vault.admin_links()),
        ("DELETE", ["links", id]) => match state.vault.admin_delete_link(id) {
            Ok(()) => Response::new(204),
            Err(e) => fail(e),
        },
        ("PUT", ["server-password"]) => match body(req).and_then(|v| server_password(state, &v)) {
            Ok(()) => Response::new(204),
            Err(e) => fail(e),
        },
        ("POST", ["logins", "revoke"]) => {
            state.auth.revoke_all();
            crate::info!("admin ended all app logins");
            Response::new(204)
        }
        ("GET", ["speech"]) => Response::json(200, &speech(state)),
        ("POST", ["speech", "model"]) => match body(req).and_then(|v| pick_model(state, &v)) {
            Ok(v) => Response::json(202, &v),
            Err(e) => fail(e),
        },
        ("DELETE", ["speech", "models", id]) => match crate::models::find(id) {
            Some(mi) => {
                let path = state.models.path(mi);
                if state.lyrics_sync.model().as_deref() == Some(path.as_path()) {
                    return Response::error(409, "this model is in use; pick another one first");
                }
                state.models.delete(mi);
                Response::new(204)
            }
            None => Response::error(404, "no such model"),
        },
        ("GET", ["sources"]) => Response::json(200, &state.sources.admin_json()),
        ("GET", ["sources", "discover"]) => {
            Response::json(200, &crate::sources::Sources::discover())
        }
        ("POST", ["sources"]) => match body(req).and_then(|v| put_source(state, None, &v)) {
            Ok(id) => Response::json(201, &Value::object().with("id", id)),
            Err(e) => fail(e),
        },
        ("PUT", ["sources", id]) => match body(req).and_then(|v| put_source(state, Some(id), &v)) {
            Ok(id) => Response::json(200, &Value::object().with("id", id)),
            Err(e) => fail(e),
        },
        ("DELETE", ["sources", id]) => {
            if !state.sources.delete(id) {
                return Response::error(404, "no such source");
            }
            crate::info!("admin removed a music source");
            state.sources_changed();
            Response::new(204)
        }
        ("POST", ["sources", id, "sync"]) => {
            if state.sources.get(id).is_none() {
                return Response::error(404, "no such source");
            }
            state.sources.request(id);
            Response::new(202)
        }
        ("POST", ["sources", "nextcloud"]) => match body(req).and_then(|v| {
            state.sources.nextcloud_start(
                v.str_or("url", ""),
                v.str_or("name", ""),
                v.str_or("path", ""),
            )
        }) {
            Ok((id, url)) => {
                Response::json(200, &Value::object().with("id", id).with("loginUrl", url))
            }
            Err(e) => fail(e),
        },
        ("POST", ["sources", "nextcloud", id]) => match state.sources.nextcloud_poll(id) {
            Ok(done) => Response::json(
                200,
                &Value::object()
                    .with("done", done.is_some())
                    .with("id", done.unwrap_or_default()),
            ),
            Err(e) => fail(e),
        },
        ("POST", ["speech", "retry"]) => Response::json(
            200,
            &Value::object().with("retried", state.lyrics_sync.retry_failed() as u64),
        ),
        (_, ["password" | "settings" | "restart" | "rescan" | "server-password"]) => {
            not_allowed("GET, PUT, POST")
        }
        _ => Response::error(404, "not found"),
    }
}

/// Adds (`id` None) or changes a music source from the admin's form.
fn put_source(state: &State, id: Option<&str>, v: &Value) -> Result<String, (u16, String)> {
    let mut cfg = bl_sources::SourceConfig::from_json(v);
    cfg.kind = cfg.kind.trim().to_ascii_lowercase();
    let r = state
        .sources
        .put(id, v.str_or("name", ""), v.bool_or("enabled", true), cfg)?;
    crate::info!(
        "admin {} a music source",
        if id.is_some() { "changed" } else { "added" }
    );
    Ok(r)
}

/// Bytes in a folder (recursively), stopping after many entries.
fn folder_size(p: &Path, budget: &mut u32) -> u64 {
    let mut total = 0;
    for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        match e.metadata() {
            Ok(m) if m.is_dir() => total += folder_size(&e.path(), budget),
            Ok(m) => total += m.len(),
            Err(_) => {}
        }
    }
    total
}

fn overview(state: &State) -> Value {
    let cfg = state.config();
    let idx = state.library();
    let data = &state.data;
    let mut budget = 200_000;
    let mut disk = Value::object();
    for (k, p) in [
        ("covers", data.covers.clone()),
        ("analysis", data.analysis.clone()),
        ("images", data.image_cache.clone()),
        ("uploads", data.uploads.clone()),
        ("lyrics", data.lyrics.clone()),
        ("vault", data.root.join("vault")),
        ("models", state.models.dir.clone()),
        ("imports", data.root.join("imports")),
    ] {
        disk = disk.with(k, folder_size(&p, &mut budget));
    }
    let mut warnings: Vec<Value> = Vec::new();
    if !state.auth.required() {
        warnings.push("noPassword".into());
    }
    if cfg
        .password
        .as_ref()
        .is_some_and(|p| p.chars().count() < 12)
    {
        warnings.push("shortPassword".into());
    }
    if state.roots.is_empty() {
        warnings.push("noMusic".into());
    }
    if cfg.speech_model.as_ref().is_some_and(|p| !p.is_file())
        && state
            .models
            .active()
            .as_array()
            .is_some_and(|a| a.is_empty())
    {
        warnings.push("speechModelMissing".into());
    }
    if state.web.integrity.status.to_string().as_str() == "modified" {
        warnings.push("webModified".into());
    }
    Value::object()
        .with("name", cfg.name.as_str())
        .with("version", env!("CARGO_PKG_VERSION"))
        .with("uptime", state.started.elapsed().as_secs())
        .with("restartPending", state.restart_pending())
        .with("canRestart", state.can_restart())
        .with(
            "library",
            Value::object()
                .with("tracks", idx.entries.len() as u64)
                .with("albums", idx.albums.len() as u64)
                .with("revision", idx.revision)
                .with("scanning", state.scanning())
                .with("folders", state.roots.len() as u64),
        )
        .with("users", state.vault.user_count() as u64)
        .with("links", state.vault.link_count() as u64)
        .with("logins", state.auth.token_count() as u64)
        .with("devices", state.devices.count_online() as u64)
        .with("passwordRequired", state.auth.required())
        .with("speech", state.lyrics_sync.summary())
        .with("downloads", state.models.active())
        .with("disk", disk)
        .with("data", data.root.display().to_string())
        .with("bind", cfg.bind.as_str())
        .with("webClient", state.web.describe())
        .with("webIntegrity", state.web.integrity.status.to_string())
        .with("warnings", Value::Array(warnings))
}

fn settings(state: &State) -> Value {
    let cfg = state.config();
    let list: Vec<Value> = config::ADMIN_SETTINGS
        .iter()
        .map(|s| {
            let origin = cfg
                .origins
                .iter()
                .find(|(k, _)| k == s.key)
                .map(|(_, o)| o.as_str());
            Value::object()
                .with("key", s.key)
                .with("value", config::setting_value(&cfg, s.key))
                .with("live", s.live)
                .with("lockedBy", origin.map_or(Value::Null, Value::from))
        })
        .collect();
    let password_origin = cfg
        .origins
        .iter()
        .find(|(k, _)| k == "password")
        .map(|(_, o)| o.clone())
        .or_else(|| {
            cfg.password
                .is_some()
                .then(|| "configuration file".to_string())
        });
    Value::object()
        .with("settings", Value::Array(list))
        .with("restartPending", state.restart_pending())
        .with(
            "serverPassword",
            Value::object()
                .with("set", state.auth.required())
                .with("lockedBy", password_origin.map_or(Value::Null, Value::from)),
        )
}

/// `PUT /api/admin/settings` `{"values": {key: value | null}}`: checks,
/// stores in `settings.json`, applies what can be applied live.
fn save_settings(state: &State, v: &Value) -> Result<Value, (u16, String)> {
    // One change at a time (read, merge and write settings.json).
    static SAVING: Mutex<()> = Mutex::new(());
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let values = v
        .get("values")
        .as_object()
        .ok_or((400, "expected {\"values\": {...}}".to_string()))?;
    let cur = state.config();
    let path = config::settings_path(&state.data.root);
    let mut stored = std::fs::read(&path)
        .ok()
        .and_then(|d| bl_json::parse_bytes(&d).ok())
        .filter(|v| v.as_object().is_some())
        .unwrap_or_else(Value::object);
    let mut next: Config = (*cur).clone();
    let mut restart = false;
    for (key, val) in values.iter() {
        let Some(info) = config::ADMIN_SETTINGS.iter().find(|s| s.key == key) else {
            return Err((400, format!("\"{key}\" cannot be set here")));
        };
        if let Some((_, origin)) = cur.origins.iter().find(|(k, _)| k == key) {
            return Err((409, format!("\"{key}\" is set by {origin}")));
        }
        let one = Value::object().with(key, val.clone());
        if val.is_null() {
            // Back to the default.
            apply_default(&mut next, &Config::default(), key);
            if let Some(o) = stored.as_object_mut() {
                o.remove(key);
            }
        } else {
            config::apply_json(&mut next, &one, &state.data.root).map_err(|e| (400, e))?;
            stored.set(key, val.clone());
        }
        if config::setting_value(&next, key) != config::setting_value(&cur, key) && !info.live {
            restart = true;
        }
    }
    if let Some(u) = &next.public_url {
        if !(u.starts_with("https://") || u.is_empty()) {
            return Err((400, "the public address must start with https://".into()));
        }
    }
    util::write_atomic(&path, stored.to_json_pretty().as_bytes())
        .map_err(|e| (500, format!("cannot write {}: {e}", path.display())))?;
    util::make_private(&path);
    state.apply_config(next, restart);
    crate::info!(
        "admin changed settings: {}",
        values.keys().collect::<Vec<_>>().join(", ")
    );
    Ok(Value::object().with("restartPending", state.restart_pending()))
}

/// Resets one setting of `cfg` to its value in `d`.
fn apply_default(cfg: &mut Config, d: &Config, key: &str) {
    match key {
        "name" => cfg.name = d.name.clone(),
        "music" => cfg.music_dirs = d.music_dirs.clone(),
        "images" => cfg.image_dirs = d.image_dirs.clone(),
        "rescanInterval" => cfg.rescan_interval = d.rescan_interval,
        "scanThreads" => cfg.scan_threads = d.scan_threads,
        "analysisMaxMinutes" => cfg.analysis_max_minutes = d.analysis_max_minutes,
        "registration" => cfg.registration = d.registration,
        "vaultQuotaMb" => cfg.vault_quota_mb = d.vault_quota_mb,
        "allowedHosts" => cfg.allowed_hosts = d.allowed_hosts.clone(),
        "publicUrl" => cfg.public_url = d.public_url.clone(),
        "androidCerts" => cfg.android_certs = d.android_certs.clone(),
        "pushAllowPrivate" => cfg.push_allow_private = d.push_allow_private,
        "sourceUrl" => cfg.source_url = d.source_url.clone(),
        "extensions" => cfg.extensions = d.extensions.clone(),
        "speechModel" => cfg.speech_model = d.speech_model.clone(),
        "autoSyncLyrics" => cfg.auto_sync_lyrics = d.auto_sync_lyrics,
        "verbose" => cfg.verbose = d.verbose,
        _ => {}
    }
}

/// `PUT /api/admin/server-password` `{"password": "..." | null}`.
fn server_password(state: &State, v: &Value) -> Result<(), (u16, String)> {
    let cfg = state.config();
    if cfg.password.is_some() {
        return Err((
            409,
            "the server password is set in the server's configuration".into(),
        ));
    }
    match v.get("password") {
        Value::Null => {
            state.admin.set_server_password(None);
            state.auth.set_secret(None);
            crate::info!("admin removed the server password");
        }
        Value::String(pw) => {
            check_new_password(pw)?;
            let ver = Verifier::new(pw).map_err(|e| (500, e.to_string()))?;
            state.admin.set_server_password(Some(ver.clone()));
            state
                .auth
                .set_secret(Some(crate::auth::Secret::Verifier(ver)));
            crate::info!("admin set a new server password; all app logins ended");
        }
        _ => return Err((400, "expected {\"password\": string or null}".into())),
    }
    Ok(())
}

fn speech(state: &State) -> Value {
    let current = state.lyrics_sync.model();
    let cfg = state.config();
    let lock = |k: &str| {
        cfg.origins
            .iter()
            .find(|(o, _)| o == k)
            .map_or(Value::Null, |(_, o)| Value::from(o.as_str()))
    };
    Value::object()
        .with("models", state.models.list(current.as_deref()))
        .with("summary", state.lyrics_sync.summary())
        .with("modelLockedBy", lock("speechModel"))
        .with("autoLockedBy", lock("autoSyncLyrics"))
        .with("source", state.models.source())
}

/// `POST /api/admin/speech/model` `{"id": "small-q5_1"}`: downloads the
/// model if needed and switches to it; `{"id": null}` turns recognition off.
fn pick_model(state: &Arc<State>, v: &Value) -> Result<Value, (u16, String)> {
    if let Some((_, o)) = state
        .config()
        .origins
        .iter()
        .find(|(k, _)| k == "speechModel")
    {
        return Err((409, format!("the speech model is set by {o}")));
    }
    let set_path = |state: &State, p: Option<PathBuf>| -> Result<(), (u16, String)> {
        let value = p
            .as_ref()
            .map_or(Value::Null, |p| Value::from(p.display().to_string()));
        save_settings(
            state,
            &Value::object().with("values", Value::object().with("speechModel", value)),
        )
        .map(|_| ())
    };
    match v.get("id") {
        Value::Null => {
            set_path(state, None)?;
            Ok(Value::object().with("model", Value::Null))
        }
        Value::String(id) => {
            let m = crate::models::find(id).ok_or((404, "no such model".to_string()))?;
            let weak = Arc::downgrade(state);
            state.models.download(m, move |path| {
                if let Some(state) = weak.upgrade() {
                    if let Err((_, e)) = set_path(&state, Some(path)) {
                        crate::warn!("cannot switch to the new speech model: {e}");
                    }
                }
            });
            Ok(Value::object().with("model", m.id))
        }
        _ => Err((400, "expected {\"id\": string or null}".into())),
    }
}
