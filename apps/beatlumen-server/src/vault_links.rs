//! Public links to encrypted files (`/share/<id>#<secret>`, see
//! `bl_vault::link` and docs/ENCRYPTION.md). The server keeps the wrapped
//! file key and, for password links, the hash of a password proof; it
//! cannot decrypt anything. Anyone with the link id gets the file's
//! public information; password links hand out the wrapped key and the
//! file only after the right proof.

use super::{
    b64, body, bytes, fail, params_from, params_json, read_json, save_json, Failure, FileRec, Vault,
};
use crate::util::{self, ct_eq};
use bl_json::Value;
use bl_net::server::{Request, Response};
use bl_vault::keys::{KdfParams, SALT_LEN};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

/// Longest a link may live (0 = no expiry).
const MAX_LIFETIME: u64 = 366 * 24 * 3600;
const MAX_LINKS_PER_USER: usize = 1000;
/// Wrong passwords per link (and per address) within the window.
const MAX_LINK_FAILURES: u32 = 10;
const LINK_FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// How long a download permission from a password proof lasts.
const TOKEN_LIFETIME: Duration = Duration::from_secs(30 * 60);

pub(super) struct Link {
    id: String,
    file: String,
    owner: String,
    created: u64,
    /// Unix time, 0 = never.
    expires: u64,
    wrapped: Vec<u8>,
    password: Option<([u8; SALT_LEN], KdfParams, [u8; 32])>,
}

impl Link {
    fn to_json(&self) -> Value {
        let mut v = Value::object()
            .with("id", self.id.as_str())
            .with("file", self.file.as_str())
            .with("owner", self.owner.as_str())
            .with("created", self.created)
            .with("expires", self.expires)
            .with("wrapped", b64(&self.wrapped));
        if let Some((salt, params, hash)) = &self.password {
            v = v.with(
                "password",
                Value::object()
                    .with("salt", b64(salt))
                    .with("params", params_json(*params))
                    .with("authHash", b64(hash)),
            );
        }
        v
    }

    fn from_json(v: &Value) -> Option<Link> {
        let password = match v.get("password") {
            p if p.is_null() => None,
            p => Some((
                super::array::<SALT_LEN>(p, "salt").ok()?,
                params_from(p.get("params")).ok()?,
                super::array::<32>(p, "authHash").ok()?,
            )),
        };
        Some(Link {
            id: v.str_or("id", "").to_string(),
            file: v.str_or("file", "").to_string(),
            owner: v.str_or("owner", "").to_string(),
            created: v.get("created").as_u64()?,
            expires: v.get("expires").as_u64()?,
            wrapped: bytes(v, "wrapped", Some(bl_vault::link::WRAPPED_LEN)).ok()?,
            password,
        })
    }

    /// What the owner sees in the list.
    fn entry(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("file", self.file.as_str())
            .with("created", self.created)
            .with("expires", self.expires)
            .with("password", self.password.is_some())
    }

    fn expired(&self, now: u64) -> bool {
        self.expires != 0 && now >= self.expires
    }
}

fn valid_link_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Loads the stored links (expired ones are removed).
pub(super) fn load(dir: &Path) -> HashMap<String, Link> {
    let mut out = HashMap::new();
    let now = util::now_secs();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        match read_json(&p).as_ref().and_then(Link::from_json) {
            Some(l) if valid_link_id(&l.id) && !l.expired(now) => {
                out.insert(l.id.clone(), l);
            }
            _ => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    out
}

impl Vault {
    fn link_path(&self, id: &str) -> std::path::PathBuf {
        self.links_dir.join(format!("{id}.json"))
    }

    /// `POST /api/vault/files/<id>/links` (owner only).
    pub(super) fn create_link(
        &self,
        req: &Request,
        me: &str,
        f: &FileRec,
    ) -> Result<Response, Failure> {
        if !f.complete() {
            return fail(409, "upload not finished");
        }
        let v = body(req)?;
        let wrapped = bytes(&v, "wrapped", Some(bl_vault::link::WRAPPED_LEN))?;
        let lifetime = v.get("expiresIn").as_u64().unwrap_or(0);
        if lifetime > MAX_LIFETIME {
            return fail(400, "expiry too far away");
        }
        let password = match v.get("password") {
            p if p.is_null() => None,
            p => Some((
                super::array::<SALT_LEN>(p, "salt")?,
                params_from(p.get("params"))?,
                super::array::<32>(p, "authHash")?,
            )),
        };
        let now = util::now_secs();
        let id = crate::random::hex(16).map_err(|e| (500, e.to_string()))?;
        let link = Link {
            id: id.clone(),
            file: f.id.clone(),
            owner: me.to_string(),
            created: now,
            expires: if lifetime == 0 { 0 } else { now + lifetime },
            wrapped,
            password,
        };
        let mut links = self.links.write().unwrap_or_else(|e| e.into_inner());
        if links.values().filter(|l| l.owner == me).count() >= MAX_LINKS_PER_USER {
            return fail(507, "too many links");
        }
        std::fs::create_dir_all(&self.links_dir).map_err(|_| (500, "cannot save".to_string()))?;
        save_json(&self.link_path(&id), &link.to_json())?;
        let entry = link.entry();
        links.insert(id, link);
        Ok(Response::json(201, &entry))
    }

    /// `GET /api/vault/links`: the user's links (optionally of one file).
    pub(super) fn list_links(&self, req: &Request, me: &str) -> Response {
        let now = util::now_secs();
        let file = req.query("file");
        let links = self.links.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<&Link> = links
            .values()
            .filter(|l| l.owner == me && !l.expired(now) && file.is_none_or(|f| l.file == f))
            .collect();
        list.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Response::json(
            200,
            &Value::Array(list.into_iter().map(Link::entry).collect()),
        )
    }

    /// `DELETE /api/vault/links/<id>` (the link's owner).
    pub(super) fn delete_link(&self, me: &str, id: &str) -> Result<Response, Failure> {
        let mut links = self.links.write().unwrap_or_else(|e| e.into_inner());
        match links.get(id) {
            Some(l) if l.owner == me => {
                let _ = std::fs::remove_file(self.link_path(id));
                links.remove(id);
                Ok(Response::new(204))
            }
            _ => fail(404, "no such link"),
        }
    }

    /// Removes the links of a file (deleted) or of a user (account deleted).
    pub(super) fn drop_links(&self, pred: impl Fn(&str, &str) -> bool) {
        let mut links = self.links.write().unwrap_or_else(|e| e.into_inner());
        links.retain(|id, l| {
            let drop = pred(&l.file, &l.owner);
            if drop {
                let _ = std::fs::remove_file(self.links_dir.join(format!("{id}.json")));
            }
            !drop
        });
    }

    /// `/api/share/<id>[/unlock|/data]`, without an account.
    pub fn public_link(&self, req: &Request, segs: &[&str], ip: Option<IpAddr>) -> Response {
        match self.public_route(req, segs, ip) {
            Ok(r) => r.with_header("Cache-Control", "no-store"),
            Err((code, msg)) => Response::error(code, &msg),
        }
    }

    fn public_route(
        &self,
        req: &Request,
        segs: &[&str],
        ip: Option<IpAddr>,
    ) -> Result<Response, Failure> {
        let m = req.method.as_str();
        let get = m == "GET" || m == "HEAD";
        let Some(id) = segs.first().filter(|id| valid_link_id(id)) else {
            return fail(404, "link not found");
        };
        let now = util::now_secs();
        let (file_id, has_password, wrapped, password_json, auth_hash, expires) = {
            let mut links = self.links.write().unwrap_or_else(|e| e.into_inner());
            let Some(l) = links.get(*id) else {
                return fail(404, "link not found");
            };
            if l.expired(now) {
                let _ = std::fs::remove_file(self.link_path(id));
                links.remove(*id);
                return fail(404, "link not found");
            }
            (
                l.file.clone(),
                l.password.is_some(),
                l.wrapped.clone(),
                l.password.as_ref().map(|(s, p, _)| {
                    Value::object()
                        .with("salt", b64(s))
                        .with("params", params_json(*p))
                }),
                l.password.as_ref().map(|(_, _, h)| *h),
                l.expires,
            )
        };
        let files = self.files.read().unwrap_or_else(|e| e.into_inner());
        let Some(f) = files.get(&file_id).filter(|f| f.complete()) else {
            return fail(404, "link not found");
        };
        match &segs[1..] {
            [] if get => {
                let mut v = Value::object()
                    .with("file", f.id.as_str())
                    .with("header", b64(&f.header))
                    .with("size", f.size)
                    .with("expires", expires);
                v = match password_json {
                    Some(p) => v.with("password", p),
                    // Without a password the wrapped key and the encrypted
                    // name are public: the secret in the link opens them.
                    None => v.with("wrapped", b64(&wrapped)).with("meta", b64(&f.meta)),
                };
                Ok(Response::json(200, &v))
            }
            ["unlock"] if m == "POST" => {
                let Some(hash) = auth_hash else {
                    return fail(400, "this link has no password");
                };
                let keys = [
                    format!("link:{id}"),
                    format!("linkip:{}", ip.map(|a| a.to_string()).unwrap_or_default()),
                ];
                if keys.iter().any(|k| self.link_blocked(k)) {
                    return fail(429, "too many attempts, try again later");
                }
                let v = body(req)?;
                let auth = bytes(&v, "auth", Some(32))?;
                if !ct_eq(&bl_vault::sha256(&[&auth]), &hash) {
                    for k in &keys {
                        self.link_failed(k);
                    }
                    std::thread::sleep(super::FAILURE_DELAY);
                    return fail(403, "wrong password");
                }
                let token = crate::random::hex(16).map_err(|e| (500, e.to_string()))?;
                let mut tokens = self.link_tokens.lock().unwrap_or_else(|e| e.into_inner());
                tokens.retain(|_, (_, t)| t.elapsed() < TOKEN_LIFETIME);
                tokens.insert(token.clone(), (id.to_string(), Instant::now()));
                Ok(Response::json(
                    200,
                    &Value::object()
                        .with("wrapped", b64(&wrapped))
                        .with("meta", b64(&f.meta))
                        .with("token", token),
                ))
            }
            ["data"] if get => {
                if has_password {
                    let tokens = self.link_tokens.lock().unwrap_or_else(|e| e.into_inner());
                    let ok = req
                        .query("token")
                        .and_then(|t| tokens.get(t))
                        .is_some_and(|(link, t)| link == id && t.elapsed() < TOKEN_LIFETIME);
                    if !ok {
                        return fail(403, "password required");
                    }
                }
                let path = self.files_dir.join(format!("{}.blob", f.id));
                Ok(Response::file(
                    &path,
                    "application/octet-stream",
                    req.header("range"),
                ))
            }
            _ => fail(405, "method not allowed"),
        }
    }

    fn link_blocked(&self, key: &str) -> bool {
        let f = self.link_failures.lock().unwrap_or_else(|e| e.into_inner());
        f.get(key)
            .is_some_and(|(n, t)| *n >= MAX_LINK_FAILURES && t.elapsed() < LINK_FAILURE_WINDOW)
    }

    fn link_failed(&self, key: &str) {
        let mut f = self.link_failures.lock().unwrap_or_else(|e| e.into_inner());
        if f.len() > 100_000 {
            f.retain(|_, (_, t)| t.elapsed() < LINK_FAILURE_WINDOW);
        }
        let e = f.entry(key.to_string()).or_insert((0, Instant::now()));
        if e.1.elapsed() >= LINK_FAILURE_WINDOW {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }
}
