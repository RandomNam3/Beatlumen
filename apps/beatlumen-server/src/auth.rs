//! Password login and bearer tokens.
//!
//! * Tokens are 128-bit random values (hex) from the OS random source.
//! * Only SHA-256 digests of tokens are kept (in memory and in the
//!   database: `auth/token/<digest>` with the time it was issued), so a
//!   leaked data folder does not leak usable tokens.
//! * A password from the configuration is compared in constant time
//!   (digests of equal length); one set in the admin interface is kept only
//!   as an Argon2id verifier.
//! * Tokens expire after 180 days; `POST /api/logout` revokes one. Changing
//!   the password invalidates all tokens (a salted password digest is
//!   stored to detect that).
//! * Failed logins are delayed and limited per client address.

use crate::sha256::{sha256, sha256_parts};
use crate::store::Store;
use crate::util::{self, ct_eq};
use bl_json::Value;
use bl_net::server::Request;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Failed attempts per address and window before logins are refused.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const FAILURE_DELAY: Duration = Duration::from_millis(300);
/// Upper bound for stored tokens (oldest are dropped).
const MAX_TOKENS: usize = 10_000;
/// Logins expire after this time (clients then ask for the password again).
const TOKEN_LIFETIME: u64 = 180 * 24 * 3600;
/// The salt and the password check in the database.
const META_KEY: &str = "auth/meta";
/// One entry per token: `auth/token/<digest>` = when it was issued.
const TOKENS: &str = "auth/token/";

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The server password: from the configuration (digest) or set in the
/// admin interface (Argon2id verifier).
pub enum Secret {
    Plain([u8; 32], String),
    Verifier(bl_vault::keys::Verifier),
}

impl Secret {
    pub fn plain(password: &str) -> Secret {
        Secret::Plain(sha256(password.as_bytes()), password.to_string())
    }

    fn matches(&self, password: &str) -> bool {
        match self {
            Secret::Plain(d, _) => ct_eq(&sha256(password.as_bytes()), d),
            Secret::Verifier(v) => v.matches(password),
        }
    }

    /// Salted digest stored next to the tokens: tokens issued under another
    /// password are dropped at start.
    fn check(&self, salt: &str) -> String {
        let material: &[u8] = match self {
            Secret::Plain(_, p) => p.as_bytes(),
            Secret::Verifier(v) => &v.hash,
        };
        util::hex(&sha256_parts(&[salt.as_bytes(), b"\0", material]))
    }
}

/// Token digests in issue order (for trimming) plus a set for lookups.
#[derive(Default)]
struct TokenStore {
    order: Vec<[u8; 32]>,
    /// Digest -> issue time (seconds since the Unix epoch).
    set: HashMap<[u8; 32], u64>,
}

pub struct Auth {
    secret: RwLock<Option<Secret>>,
    store: Arc<Store>,
    salt: String,
    check: RwLock<String>,
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
    pub fn new(secret: Option<Secret>, store: Arc<Store>) -> Auth {
        let stored = store.get_json(META_KEY).unwrap_or_default();
        let mut salt = stored.str_or("salt", "").to_string();
        if !matches!(util::unhex(&salt), Some(s) if s.len() >= 16) {
            salt = crate::random::hex(16).unwrap_or_else(|_| {
                util::hex(&sha256(store.path().to_string_lossy().as_bytes())[..16])
            });
        }
        let check = secret.as_ref().map(|s| s.check(&salt)).unwrap_or_default();
        let same = secret.is_some()
            && ct_eq(
                stored.str_or("passwordCheck", "").as_bytes(),
                check.as_bytes(),
            );
        let now = unix_now();
        let mut list = Vec::new();
        let mut gone = Vec::new();
        for (hex, issued) in store.list(TOKENS) {
            let issued: Option<u64> = std::str::from_utf8(&issued)
                .ok()
                .and_then(|i| i.parse().ok());
            let d = util::unhex(&hex).and_then(|v| <[u8; 32]>::try_from(v).ok());
            match (d, issued) {
                (Some(d), Some(i)) if same && now.saturating_sub(i) < TOKEN_LIFETIME => {
                    list.push((d, i))
                }
                _ => gone.push(hex),
            }
        }
        list.sort_by_key(|(_, issued)| *issued);
        let set: HashMap<[u8; 32], u64> = list.iter().copied().collect();
        let list: Vec<[u8; 32]> = list.into_iter().map(|(d, _)| d).collect();
        // Expired tokens and those of another password go.
        let meta = Value::object()
            .with("salt", salt.as_str())
            .with("passwordCheck", check.as_str());
        let saved = store.write(|tx| {
            tx.put_json(META_KEY, &meta)?;
            for hex in &gone {
                tx.delete(&format!("{TOKENS}{hex}"))?;
            }
            Ok(())
        });
        if let Err(e) = saved {
            crate::warn!("cannot save the logins: {e}");
        }
        Auth {
            secret: RwLock::new(secret),
            store,
            salt,
            check: RwLock::new(check),
            tokens: RwLock::new(TokenStore { order: list, set }),
            failures: Mutex::new(HashMap::new()),
            check_lock: Mutex::new(()),
        }
    }

    pub fn required(&self) -> bool {
        self.secret
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Sets or removes the password (admin interface); all logins end.
    pub fn set_secret(&self, secret: Option<Secret>) {
        let _serial = self.check_lock.lock().unwrap_or_else(|e| e.into_inner());
        *self.check.write().unwrap_or_else(|e| e.into_inner()) = secret
            .as_ref()
            .map(|s| s.check(&self.salt))
            .unwrap_or_default();
        *self.secret.write().unwrap_or_else(|e| e.into_inner()) = secret;
        self.revoke_all();
    }

    /// Ends every login (apps ask for the password again).
    pub fn revoke_all(&self) {
        let mut g = self.tokens.write().unwrap_or_else(|e| e.into_inner());
        g.order.clear();
        g.set.clear();
        let meta = Value::object().with("salt", self.salt.as_str()).with(
            "passwordCheck",
            self.check
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_str(),
        );
        let saved = self.store.write(|tx| {
            tx.delete_prefix(TOKENS)?;
            tx.put_json(META_KEY, &meta)
        });
        if let Err(e) = saved {
            crate::warn!("cannot save the logins: {e}");
        }
    }

    /// Logins currently valid.
    pub fn token_count(&self) -> usize {
        let now = unix_now();
        self.tokens
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .set
            .values()
            .filter(|issued| now.saturating_sub(**issued) < TOKEN_LIFETIME)
            .count()
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
        if !self.required() {
            // No password configured: nothing to log in to; an empty token
            // works because no endpoint checks it.
            return Ok(String::new());
        }
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
        let ok = self
            .secret
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|s| s.matches(pw));
        if !ok {
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
        let now = unix_now();
        let mut g = self.tokens.write().unwrap_or_else(|e| e.into_inner());
        let oldest = (g.order.len() >= MAX_TOKENS).then(|| g.order[0]);
        self.store
            .write(|tx| {
                tx.put(
                    &format!("{TOKENS}{}", util::hex(&d)),
                    now.to_string().as_bytes(),
                )?;
                if let Some(old) = oldest {
                    tx.delete(&format!("{TOKENS}{}", util::hex(&old)))?;
                }
                Ok(())
            })
            .map_err(|e| LoginError::Internal(format!("cannot save the login: {e}")))?;
        g.order.push(d);
        g.set.insert(d, now);
        if let Some(old) = oldest {
            g.order.remove(0);
            g.set.remove(&old);
        }
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
            if let Err(e) = self.store.delete(&format!("{TOKENS}{}", util::hex(&d))) {
                crate::warn!("cannot save the logout: {e}");
            }
        }
    }
}
