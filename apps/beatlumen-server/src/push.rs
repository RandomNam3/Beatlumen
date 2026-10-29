//! Push notifications through UnifiedPush (Web Push, RFC 8030/8291/8292).
//!
//! The apps register a push endpoint (given to them by their UnifiedPush
//! distributor, e.g. ntfy) with a P-256 key and an auth secret. When a file
//! is shared with a user, the server sends a small message to each of the
//! user's endpoints, encrypted for the app: the push service only sees
//! ciphertext. No phone number or e-mail address is involved.
//!
//! The server connects only to the endpoints the users registered; private
//! and local addresses are refused (unless `--push-allow-private`, for a
//! push service in the home network), so a user cannot make the server
//! talk to internal services.

use crate::util;
use bl_json::Value;
use bl_net::{base64_decode, base64_encode};
use bl_vault::webpush::{self, Vapid};
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

const MAX_SUBSCRIPTIONS: usize = 10;
const TTL_SECS: u64 = 2 * 24 * 3600;

#[derive(Clone)]
struct Sub {
    id: String,
    endpoint: String,
    p256dh: Vec<u8>,
    auth: [u8; 16],
    name: String,
    created: u64,
}

impl Sub {
    fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("endpoint", self.endpoint.as_str())
            .with("p256dh", base64_encode(&self.p256dh))
            .with("auth", base64_encode(&self.auth))
            .with("name", self.name.as_str())
            .with("created", self.created)
    }

    fn from_json(v: &Value) -> Option<Sub> {
        Some(Sub {
            id: v.str_or("id", "").to_string(),
            endpoint: v.str_or("endpoint", "").to_string(),
            p256dh: base64_decode(v.str_or("p256dh", ""))?,
            auth: base64_decode(v.str_or("auth", ""))?.try_into().ok()?,
            name: v.str_or("name", "").to_string(),
            created: v.get("created").as_u64().unwrap_or(0),
        })
    }
}

struct Shared {
    dir: PathBuf,
    subs: Mutex<HashMap<String, Vec<Sub>>>,
    vapid: Vapid,
    subject: String,
    allow_private: bool,
}

pub struct Push {
    shared: Arc<Shared>,
    queue: Mutex<Sender<(String, Vec<u8>)>>,
}

fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                // Carrier-grade NAT (100.64.0.0/10).
                || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private(IpAddr::V4(v4));
            }
            let s = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || s & 0xfe00 == 0xfc00
                || s & 0xffc0 == 0xfe80
        }
    }
}

impl Shared {
    fn path(&self, user: &str) -> PathBuf {
        self.dir.join(format!("{user}.json"))
    }

    fn save(&self, user: &str, subs: &[Sub]) {
        let path = self.path(user);
        if subs.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        let v = Value::Array(subs.iter().map(Sub::to_json).collect());
        match util::write_atomic(&path, v.to_json().as_bytes()) {
            Ok(()) => util::make_private(&path),
            Err(e) => crate::warn!("cannot write {}: {e}", path.display()),
        }
    }

    /// Checks an endpoint URL (scheme and addresses).
    fn check_endpoint(&self, endpoint: &str) -> Result<bl_net::client::Url, String> {
        let u = bl_net::client::Url::parse(endpoint).map_err(|_| "invalid endpoint".to_string())?;
        if u.scheme != "https" && !(self.allow_private && u.scheme == "http") {
            return Err("the endpoint must use https".into());
        }
        if !self.allow_private {
            let addrs: Vec<IpAddr> = (u.host.as_str(), u.port)
                .to_socket_addrs()
                .map_err(|e| format!("cannot resolve {}: {e}", u.host))?
                .map(|a| a.ip())
                .collect();
            if addrs.is_empty() || addrs.iter().any(|a| private(*a)) {
                return Err(
                    "the endpoint is in a private network (allow it with --push-allow-private)"
                        .into(),
                );
            }
        }
        Ok(u)
    }

    fn send(&self, user: &str, payload: &[u8]) {
        let subs = self
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default();
        let mut gone = Vec::new();
        for s in subs {
            let host = bl_net::client::Url::parse(&s.endpoint)
                .map(|u| u.host)
                .unwrap_or_default();
            let url = match self.check_endpoint(&s.endpoint) {
                Ok(u) => u,
                Err(e) => {
                    crate::warn!("push to {host} skipped: {e}");
                    continue;
                }
            };
            let body = match webpush::encrypt(&s.p256dh, &s.auth, payload) {
                Ok(b) => b,
                Err(e) => {
                    crate::warn!("push to {host}: {e}");
                    continue;
                }
            };
            let audience = if (url.scheme == "https" && url.port == 443)
                || (url.scheme == "http" && url.port == 80)
            {
                format!("{}://{}", url.scheme, url.host)
            } else {
                format!("{}://{}:{}", url.scheme, url.host, url.port)
            };
            let auth =
                self.vapid
                    .authorization(&audience, util::now_secs() + 12 * 3600, &self.subject);
            let headers = vec![
                (
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                ),
                ("Content-Encoding".into(), "aes128gcm".into()),
                ("TTL".into(), TTL_SECS.to_string()),
                ("Urgency".into(), "normal".into()),
                ("Authorization".into(), auth),
            ];
            let opts = bl_net::client::Options {
                max_body: 64 * 1024,
                max_redirects: 0,
                ..Default::default()
            };
            match bl_net::client::request("POST", &s.endpoint, &headers, &body, &opts) {
                Ok(r) if (200..300).contains(&r.status) => {
                    crate::debug!("push to {host}: {}", r.status)
                }
                // The subscription is gone (app uninstalled, distributor changed).
                Ok(r) if r.status == 404 || r.status == 410 => gone.push(s.id.clone()),
                Ok(r) => crate::warn!("push to {host}: HTTP {}", r.status),
                Err(e) => crate::warn!("push to {host}: {e}"),
            }
        }
        if !gone.is_empty() {
            let mut all = self.subs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(list) = all.get_mut(user) {
                list.retain(|s| !gone.contains(&s.id));
                self.save(user, list);
            }
        }
    }
}

impl Push {
    pub fn open(vault_dir: &Path, subject: &str, allow_private: bool) -> Result<Push, String> {
        let dir = vault_dir.join("push");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let key_path = vault_dir.join("vapid.key");
        let vapid = match std::fs::read(&key_path)
            .ok()
            .and_then(|k| Vapid::from_bytes(&k).ok())
        {
            Some(v) => v,
            None => {
                let v = Vapid::generate().map_err(|e| e.to_string())?;
                util::write_atomic(&key_path, &v.to_bytes())
                    .map_err(|e| format!("cannot write {}: {e}", key_path.display()))?;
                util::make_private(&key_path);
                v
            }
        };
        let mut subs = HashMap::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Some(user) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                continue;
            };
            let list: Vec<Sub> = std::fs::read(&p)
                .ok()
                .and_then(|d| bl_json::parse_bytes(&d).ok())
                .and_then(|v| {
                    v.as_array()
                        .map(|a| a.iter().filter_map(Sub::from_json).collect())
                })
                .unwrap_or_default();
            subs.insert(user, list);
        }
        let shared = Arc::new(Shared {
            dir,
            subs: Mutex::new(subs),
            vapid,
            subject: subject.to_string(),
            allow_private,
        });
        let (tx, rx) = channel::<(String, Vec<u8>)>();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("push".into())
            .spawn(move || {
                for (user, payload) in rx {
                    let _ = util::catch("push", || worker.send(&user, &payload));
                }
            })
            .map_err(|e| format!("cannot start the push thread: {e}"))?;
        Ok(Push {
            shared,
            queue: Mutex::new(tx),
        })
    }

    /// The VAPID public key (base64url) for the apps' distributors.
    pub fn vapid_key(&self) -> String {
        webpush::base64url(&self.shared.vapid.public())
    }

    pub fn list_json(&self, user: &str) -> Value {
        let subs = self.shared.subs.lock().unwrap_or_else(|e| e.into_inner());
        Value::Array(
            subs.get(user)
                .into_iter()
                .flatten()
                .map(|s| {
                    Value::object()
                        .with("id", s.id.as_str())
                        .with("name", s.name.as_str())
                        .with("created", s.created)
                })
                .collect(),
        )
    }

    /// Registers (or renews, same endpoint) a subscription; returns its id.
    pub fn subscribe(&self, user: &str, v: &Value) -> Result<String, (u16, String)> {
        let endpoint = v.str_or("endpoint", "").trim().to_string();
        if endpoint.len() > 2048 {
            return Err((400, "endpoint too long".into()));
        }
        self.shared
            .check_endpoint(&endpoint)
            .map_err(|e| (400, e))?;
        let p256dh = base64_decode(v.str_or("p256dh", ""))
            .filter(|k| k.len() == 65 && k[0] == 4)
            .ok_or((400, "invalid \"p256dh\"".to_string()))?;
        // The key must be a point on the curve.
        webpush::encrypt(&p256dh, &[0; 16], b"")
            .map_err(|_| (400, "invalid \"p256dh\"".to_string()))?;
        let auth: [u8; 16] = base64_decode(v.str_or("auth", ""))
            .and_then(|a| a.try_into().ok())
            .ok_or((400, "invalid \"auth\"".to_string()))?;
        let name: String = v
            .str_or("name", "")
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let mut all = self.shared.subs.lock().unwrap_or_else(|e| e.into_inner());
        let list = all.entry(user.to_string()).or_default();
        let id = match list.iter_mut().find(|s| s.endpoint == endpoint) {
            Some(s) => {
                s.p256dh = p256dh;
                s.auth = auth;
                s.name = name;
                s.id.clone()
            }
            None => {
                if list.len() >= MAX_SUBSCRIPTIONS {
                    return Err((507, "too many devices registered for notifications".into()));
                }
                let id = crate::random::hex(8).map_err(|e| (500, e.to_string()))?;
                list.push(Sub {
                    id: id.clone(),
                    endpoint,
                    p256dh,
                    auth,
                    name,
                    created: util::now_secs(),
                });
                id
            }
        };
        self.shared.save(user, list);
        Ok(id)
    }

    pub fn unsubscribe(&self, user: &str, id: &str) -> bool {
        let mut all = self.shared.subs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(list) = all.get_mut(user) else {
            return false;
        };
        let before = list.len();
        list.retain(|s| s.id != id);
        let removed = list.len() != before;
        self.shared.save(user, list);
        removed
    }

    /// Forgets all subscriptions of a deleted account.
    pub fn forget(&self, user: &str) {
        let mut all = self.shared.subs.lock().unwrap_or_else(|e| e.into_inner());
        all.remove(user);
        let _ = std::fs::remove_file(self.shared.path(user));
    }

    /// Sends `payload` to all devices of `user` in the background.
    pub fn notify(&self, user: &str, payload: &Value) {
        let has = self
            .shared
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .is_some_and(|l| !l.is_empty());
        if has {
            let _ = self
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send((user.to_string(), payload.to_json().into_bytes()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::private;

    #[test]
    fn private_addresses() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.5.4",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(private(a.parse().unwrap()), "{a}");
        }
        for a in ["1.1.1.1", "100.128.0.1", "2a01:4f8::1"] {
            assert!(!private(a.parse().unwrap()), "{a}");
        }
    }
}
