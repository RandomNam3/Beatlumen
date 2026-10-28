//! Password login and bearer tokens.
//!
//! * Tokens are 128-bit random values (hex) from the OS random source.
//! * Only SHA-256 digests of tokens are kept (in memory and in
//!   `data/tokens.json`), so a leaked data folder does not leak usable
//!   tokens.
//! * The password is compared in constant time (digests of equal length).
//! * Tokens do not expire; `POST /api/logout` revokes one. Changing the
//!   password invalidates all tokens (a salted password digest is stored to
//!   detect that).
//! * Failed logins are delayed and limited per client address.

use crate::sha256::{sha256, sha256_parts};
use crate::util::{self, ct_eq};
use bl_json::Value;
use bl_net::server::Request;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Failed attempts per address and window before logins are refused.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const FAILURE_DELAY: Duration = Duration::from_millis(300);
/// Upper bound for stored tokens (oldest are dropped).
const MAX_TOKENS: usize = 10_000;
/// Logins expire after this time (clients then ask for the password again).
const TOKEN_LIFETIME: u64 = 180 * 24 * 3600;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Token digests in issue order (for trimming) plus a set for lookups.
#[derive(Default)]
struct TokenStore {
    order: Vec<[u8; 32]>,
    /// Digest -> issue time (seconds since the Unix epoch).
    set: HashMap<[u8; 32], u64>,
}

pub struct Auth {
    password_digest: Option<[u8; 32]>,
    path: PathBuf,
    salt: String,
    check: String,
    tokens: RwLock<TokenStore>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// Serializes password checks.
    check_lock: Mutex<()>,
}

pub enum LoginError {
    BadRequest(String),
    Wrong,
    TooMany,
    Internal(String),
}

impl Auth {
    /// Creates the authenticator and loads persisted tokens (discarded when
    /// the password changed).
    pub fn new(password: Option<&str>, path: PathBuf) -> Auth {
        let password_digest = password.map(|p| sha256(p.as_bytes()));
        let stored = std::fs::read(&path)
            .ok()
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .unwrap_or_default();
        let mut salt = stored.str_or("salt", "").to_string();
        if !matches!(util::unhex(&salt), Some(s) if s.len() >= 16) {
            salt = crate::random::hex(16)
                .unwrap_or_else(|_| util::hex(&sha256(path.to_string_lossy().as_bytes())[..16]));
        }
        let check = match password {
            Some(p) => util::hex(&sha256_parts(&[salt.as_bytes(), b"\0", p.as_bytes()])),
            None => String::new(),
        };
        let mut list = Vec::new();
        if password.is_some()
            && ct_eq(
                stored.str_or("passwordCheck", "").as_bytes(),
                check.as_bytes(),
            )
        {
            // "digest:issued" (older files: digest only, counted from now).
            let now = unix_now();
            for t in stored.get("tokens").as_array().into_iter().flatten() {
                let Some(t) = t.as_str() else { continue };
                let (hex, issued) = match t.split_once(':') {
                    Some((h, i)) => (h, i.parse().unwrap_or(now)),
                    None => (t, now),
                };
                if let Some(d) = util::unhex(hex).and_then(|v| <[u8; 32]>::try_from(v).ok()) {
                    if now.saturating_sub(issued) < TOKEN_LIFETIME {
                        list.push((d, issued));
                    }
                }
            }
        }
        let set: HashMap<[u8; 32], u64> = list.iter().copied().collect();
        let list: Vec<[u8; 32]> = list.into_iter().map(|(d, _)| d).collect();
        let auth = Auth {
            password_digest,
            path,
            salt,
            check,
            tokens: RwLock::new(TokenStore { order: list, set }),
            failures: Mutex::new(HashMap::new()),
            check_lock: Mutex::new(()),
        };
        if password.is_some() {
            auth.save(&auth.tokens.read().unwrap_or_else(|e| e.into_inner()));
        }
        auth
    }

    pub fn required(&self) -> bool {
        self.password_digest.is_some()
    }

    fn save(&self, store: &TokenStore) {
        let v = Value::object()
            .with("salt", self.salt.as_str())
            .with("passwordCheck", self.check.as_str())
            .with(
                "tokens",
                Value::Array(
                    store
                        .order
                        .iter()
                        .map(|d| {
                            Value::from(format!(
                                "{}:{}",
                                util::hex(d),
                                store.set.get(d).copied().unwrap_or(0)
                            ))
                        })
                        .collect(),
                ),
            );
        match util::write_atomic(&self.path, v.to_json_pretty().as_bytes()) {
            Ok(()) => util::make_private(&self.path),
            Err(e) => crate::warn!("cannot write {}: {e}", self.path.display()),
        }
    }

    /// The token presented with a request (`Authorization: Bearer` or
    /// `?token=`).
    pub fn request_token(req: &Request) -> Option<String> {
        if let Some(h) = req.header("authorization") {
            let h = h.trim();
            if h.get(..7)
                .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
            {
                return Some(h[7..].trim().to_string());
            }
        }
        req.query("token")
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    }

    /// True when the request may access protected endpoints.
    pub fn authorized(&self, req: &Request) -> bool {
        if !self.required() {
            return true;
        }
        let Some(t) = Self::request_token(req) else {
            return false;
        };
        let d = sha256(t.as_bytes());
        self.tokens
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .set
            .get(&d)
            .is_some_and(|issued| unix_now().saturating_sub(*issued) < TOKEN_LIFETIME)
    }

    /// Checks the password and issues a new token.
    pub fn login(&self, body: &[u8], remote: Option<IpAddr>) -> Result<String, LoginError> {
        let Some(expected) = self.password_digest else {
            // No password configured: nothing to log in to; an empty token
            // works because no endpoint checks it.
            return Ok(String::new());
        };
        let v = bl_json::parse_bytes(body)
            .map_err(|e| LoginError::BadRequest(format!("invalid JSON: {e}")))?;
        let pw = v
            .get("password")
            .as_str()
            .ok_or_else(|| LoginError::BadRequest("missing \"password\"".into()))?;
        // IPv6 clients usually own a whole /64: count them per prefix.
        let ip = match remote.unwrap_or(IpAddr::from([0, 0, 0, 0])) {
            IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
                let s = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
            IpAddr::V6(v6) => IpAddr::V4(
                v6.to_ipv4_mapped()
                    .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
            ),
            v4 => v4,
        };
        {
            let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
            if f.get(&ip).is_some_and(|(n, _)| *n >= MAX_FAILURES) {
                return Err(LoginError::TooMany);
            }
        }
        // Password checks run one at a time and a wrong one costs
        // FAILURE_DELAY, so guessing is slow however many addresses an
        // attacker uses.
        let _serial = self.check_lock.lock().unwrap_or_else(|e| e.into_inner());
        if !ct_eq(&sha256(pw.as_bytes()), &expected) {
            let recent: u32 = {
                let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
                let e = f.entry(ip).or_insert((0, Instant::now()));
                e.0 += 1;
                f.values().map(|(n, _)| *n).sum()
            };
            crate::warn!("failed login from {ip}");
            // Many failures overall (e.g. from many addresses): slow down
            // further, up to 3 s per attempt.
            std::thread::sleep(FAILURE_DELAY * (1 + recent / 20).min(10));
            return Err(LoginError::Wrong);
        }
        let token = crate::random::hex(16)
            .map_err(|e| LoginError::Internal(format!("no random source: {e}")))?;
        let d = sha256(token.as_bytes());
        let mut g = self.tokens.write().unwrap_or_else(|e| e.into_inner());
        g.order.push(d);
        g.set.insert(d, unix_now());
        if g.order.len() > MAX_TOKENS {
            let old = g.order.remove(0);
            g.set.remove(&old);
        }
        self.save(&g);
        Ok(token)
    }

    /// Revokes the token of the request.
    pub fn logout(&self, req: &Request) {
        let Some(t) = Self::request_token(req) else {
            return;
        };
        let d = sha256(t.as_bytes());
        let mut g = self.tokens.write().unwrap_or_else(|e| e.into_inner());
        if g.set.remove(&d).is_some() {
            g.order.retain(|x| *x != d);
            self.save(&g);
        }
    }
}
