//! User accounts and the end-to-end encrypted file store
//! (design: `docs/ENCRYPTION.md`).
//!
//! The server never sees passwords, keys or file contents. It stores what
//! the apps encrypted, checks hashes of the apps' authentication keys and
//! enforces who may read and change which file. Even a server that ignores
//! these checks can only withhold or destroy data, not read or forge it.
//!
//! Storage (inside the data folder):
//! * `vault/users/<name>.json`: salt, Argon2 parameters, the verifiers
//!   (hashes of the auth keys with the pepper) and the wrapped keys.
//! * `vault/files/<id>.blob`: the encrypted file; `<id>.json`: owner,
//!   encrypted metadata, owner signature, sealed keys of all recipients.
//! * `vault/invites/<hash>.json`: open invitations (from the command line).
//! * `vault/sessions.json`: hashes of session tokens.
//! * `vault/pepper`: the server secret mixed into the verifiers (unless
//!   kept elsewhere with `--vault-pepper-file`).

use crate::util::{self, ct_eq};
use bl_json::Value;
use bl_net::server::{Request, Response};
use bl_net::{base64_decode, base64_encode};
use bl_vault::file::{Header, HEADER_LEN};
use bl_vault::identity::PublicIdentity;
use bl_vault::keys::{self, KdfParams, SALT_LEN, WRAPPED_IDENTITY_LEN, WRAPPED_MASTER_LEN};
use bl_vault::sealed::SEALED_KEY_LEN;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Failed logins per user and per address in [`FAILURE_WINDOW`].
const MAX_USER_FAILURES: u32 = 10;
const MAX_IP_FAILURES: u32 = 20;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const FAILURE_DELAY: Duration = Duration::from_millis(300);
const SESSION_LIFETIME: u64 = 90 * 24 * 3600;
const MAX_SESSIONS: usize = 10_000;
/// Largest encrypted file.
const MAX_FILE: u64 = 4 << 30;
const MAX_META: usize = 64 * 1024;
const MAX_FILES_PER_USER: usize = 100_000;
const MAX_RECIPIENTS: usize = 1000;
/// Unfinished uploads are removed after this time.
const PENDING_LIFETIME: u64 = 2 * 24 * 3600;
pub const INVITE_DAYS: u64 = 7;

type Failure = (u16, String);

fn fail<T>(code: u16, msg: &str) -> Result<T, Failure> {
    Err((code, msg.to_string()))
}

fn b64(data: &[u8]) -> Value {
    Value::from(base64_encode(data))
}

/// A base64 field of a JSON body, optionally of an exact length.
fn bytes(v: &Value, key: &str, len: Option<usize>) -> Result<Vec<u8>, Failure> {
    let b = v
        .get(key)
        .as_str()
        .and_then(base64_decode)
        .ok_or_else(|| (400, format!("missing or invalid \"{key}\"")))?;
    match len {
        Some(n) if b.len() != n => Err((400, format!("\"{key}\" must be {n} bytes"))),
        _ => Ok(b),
    }
}

fn array<const N: usize>(v: &Value, key: &str) -> Result<[u8; N], Failure> {
    Ok(bytes(v, key, Some(N))?.try_into().expect("checked length"))
}

fn body(req: &Request) -> Result<Value, Failure> {
    bl_json::parse_bytes(&req.body).map_err(|e| (400, format!("invalid JSON: {e}")))
}

fn user_name(v: &Value, key: &str) -> Result<String, Failure> {
    keys::normalize_user(v.str_or(key, "")).map_err(|e| (400, e.to_string()))
}

fn params_json(p: KdfParams) -> Value {
    Value::object()
        .with("m", p.m_kib as u64)
        .with("t", p.t as u64)
        .with("p", p.p as u64)
}

fn params_from(v: &Value) -> Result<KdfParams, Failure> {
    let n = |k: &str| v.get(k).as_u64().and_then(|x| u32::try_from(x).ok());
    let p = match (n("m"), n("t"), n("p")) {
        (Some(m_kib), Some(t), Some(p)) => KdfParams { m_kib, t, p },
        _ => return fail(400, "invalid \"params\""),
    };
    p.check().map_err(|e| (400, e.to_string()))?;
    Ok(p)
}

fn sha(parts: &[&[u8]]) -> [u8; 32] {
    crate::sha256::sha256_parts(parts)
}

#[derive(Clone)]
struct User {
    name: String,
    salt: [u8; SALT_LEN],
    params: KdfParams,
    verifier: [u8; 32],
    recovery_verifier: [u8; 32],
    master_by_password: Vec<u8>,
    master_by_recovery: Vec<u8>,
    identity: Vec<u8>,
    public: Vec<u8>,
    /// Raised when the password changes: older sessions end.
    generation: u64,
    created: u64,
}

impl User {
    fn to_json(&self) -> Value {
        Value::object()
            .with("name", self.name.as_str())
            .with("salt", b64(&self.salt))
            .with("params", params_json(self.params))
            .with("verifier", b64(&self.verifier))
            .with("recoveryVerifier", b64(&self.recovery_verifier))
            .with("masterByPassword", b64(&self.master_by_password))
            .with("masterByRecovery", b64(&self.master_by_recovery))
            .with("identity", b64(&self.identity))
            .with("public", b64(&self.public))
            .with("generation", self.generation)
            .with("created", self.created)
    }

    fn from_json(v: &Value) -> Result<User, Failure> {
        Ok(User {
            name: user_name(v, "name")?,
            salt: array(v, "salt")?,
            params: params_from(v.get("params"))?,
            verifier: array(v, "verifier")?,
            recovery_verifier: array(v, "recoveryVerifier")?,
            master_by_password: bytes(v, "masterByPassword", Some(WRAPPED_MASTER_LEN))?,
            master_by_recovery: bytes(v, "masterByRecovery", Some(WRAPPED_MASTER_LEN))?,
            identity: bytes(v, "identity", Some(WRAPPED_IDENTITY_LEN))?,
            public: bytes(v, "public", Some(128))?,
            generation: v.get("generation").as_u64().unwrap_or(0),
            created: v.get("created").as_u64().unwrap_or(0),
        })
    }

    /// What the app needs to unlock the account.
    fn bundle(&self) -> Value {
        Value::object()
            .with("salt", b64(&self.salt))
            .with("params", params_json(self.params))
            .with("masterByPassword", b64(&self.master_by_password))
            .with("masterByRecovery", b64(&self.master_by_recovery))
            .with("identity", b64(&self.identity))
            .with("public", b64(&self.public))
    }
}

#[derive(Clone)]
struct FileRec {
    id: String,
    owner: String,
    header: Vec<u8>,
    /// Size of the complete encrypted file.
    size: u64,
    /// Bytes stored so far.
    received: u64,
    meta: Vec<u8>,
    signature: Vec<u8>,
    /// Sealed file key per recipient (the owner included).
    keys: BTreeMap<String, Vec<u8>>,
    created: u64,
    updated: u64,
}

impl FileRec {
    fn complete(&self) -> bool {
        self.received == self.size
    }

    fn to_json(&self) -> Value {
        let keys: bl_json::Map = self.keys.iter().map(|(u, k)| (u.clone(), b64(k))).collect();
        Value::object()
            .with("id", self.id.as_str())
            .with("owner", self.owner.as_str())
            .with("header", b64(&self.header))
            .with("meta", b64(&self.meta))
            .with("signature", b64(&self.signature))
            .with("keys", keys)
            .with("created", self.created)
            .with("updated", self.updated)
    }

    fn from_json(v: &Value) -> Result<FileRec, Failure> {
        let header = bytes(v, "header", Some(HEADER_LEN))?;
        let h = Header::parse(&header).map_err(|e| (400, e.to_string()))?;
        let mut keys = BTreeMap::new();
        for (u, k) in v.get("keys").as_object().into_iter().flat_map(|m| m.iter()) {
            let k = k
                .as_str()
                .and_then(base64_decode)
                .filter(|k| k.len() == SEALED_KEY_LEN);
            keys.insert(u.to_string(), k.ok_or((400, "invalid key".to_string()))?);
        }
        Ok(FileRec {
            id: util::hex(&h.file_id),
            owner: user_name(v, "owner")?,
            size: h.encrypted_len(),
            header,
            received: 0,
            meta: bytes(v, "meta", None)?,
            signature: bytes(v, "signature", Some(64))?,
            keys,
            created: v.get("created").as_u64().unwrap_or(0),
            updated: v.get("updated").as_u64().unwrap_or(0),
        })
    }

    /// The listing entry for `user` (their own sealed key only; the owner
    /// also sees who else has access).
    fn entry(&self, user: &str) -> Value {
        let mut v = Value::object()
            .with("id", self.id.as_str())
            .with("owner", self.owner.as_str())
            .with("size", self.size)
            .with("complete", self.complete())
            .with("received", self.received)
            .with("header", b64(&self.header))
            .with("meta", b64(&self.meta))
            .with("signature", b64(&self.signature))
            .with("key", self.keys.get(user).map(|k| base64_encode(k)))
            .with("created", self.created)
            .with("updated", self.updated);
        if user == self.owner {
            v.set(
                "recipients",
                Value::Array(
                    self.keys
                        .keys()
                        .filter(|u| **u != self.owner)
                        .map(|u| Value::from(u.as_str()))
                        .collect(),
                ),
            );
        }
        v
    }
}

struct Session {
    user: String,
    issued: u64,
    generation: u64,
}

pub struct Vault {
    users_dir: PathBuf,
    files_dir: PathBuf,
    invites_dir: PathBuf,
    sessions_path: PathBuf,
    pepper: Vec<u8>,
    /// Bytes per user (0 = unlimited).
    quota: u64,
    users: RwLock<HashMap<String, User>>,
    files: RwLock<HashMap<String, FileRec>>,
    sessions: RwLock<HashMap<[u8; 32], Session>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

fn invites_dir(data: &Path) -> PathBuf {
    data.join("vault").join("invites")
}

fn invite_key(code: &str) -> String {
    let clean: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    util::hex(&sha(&[b"beatlumen invite v1\0", clean.as_bytes()]))
}

/// Creates an invitation (command line, by the server's admin) and returns
/// the code to hand to the new user.
pub fn create_invite(data: &Path, days: u64) -> Result<String, String> {
    let dir = invites_dir(data);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let raw = crate::random::hex(12).map_err(|e| format!("no random source: {e}"))?;
    let code = raw
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("hex"))
        .collect::<Vec<_>>()
        .join("-");
    let path = dir.join(format!("{}.json", invite_key(&code)));
    let v = Value::object().with("expires", util::now_secs() + days.max(1) * 24 * 3600);
    util::write_atomic(&path, v.to_json().as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    util::make_private(&path);
    Ok(code)
}

/// Registered user names (command line).
pub fn list_users(data: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(data.join("vault").join("users"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

fn read_json(path: &Path) -> Option<Value> {
    bl_json::parse_bytes(&std::fs::read(path).ok()?).ok()
}

fn save_json(path: &Path, v: &Value) -> Result<(), Failure> {
    util::write_atomic(path, v.to_json().as_bytes()).map_err(|e| {
        crate::warn!("cannot write {}: {e}", path.display());
        (500, "cannot save".to_string())
    })?;
    util::make_private(path);
    Ok(())
}

fn load_pepper(file: Option<&Path>, default: &Path) -> Result<Vec<u8>, String> {
    let path = file.unwrap_or(default);
    match std::fs::read(path) {
        Ok(p) if p.len() >= 32 => return Ok(p),
        Ok(_) => {
            return Err(format!(
                "{}: the pepper must be at least 32 bytes",
                path.display()
            ))
        }
        Err(e) if file.is_some() => return Err(format!("cannot read {}: {e}", path.display())),
        Err(_) => {}
    }
    let mut p = vec![0u8; 32];
    crate::random::fill(&mut p).map_err(|e| format!("no random source: {e}"))?;
    util::write_atomic(path, &p).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    util::make_private(path);
    Ok(p)
}

impl Vault {
    pub fn open(data: &Path, pepper_file: Option<&Path>, quota: u64) -> Result<Vault, String> {
        let dir = data.join("vault");
        let users_dir = dir.join("users");
        let files_dir = dir.join("files");
        for d in [&users_dir, &files_dir] {
            std::fs::create_dir_all(d)
                .map_err(|e| format!("cannot create {}: {e}", d.display()))?;
        }
        let pepper = load_pepper(pepper_file, &dir.join("pepper"))?;

        let mut users = HashMap::new();
        for e in std::fs::read_dir(&users_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                match read_json(&p)
                    .ok_or((0, "unreadable".into()))
                    .and_then(|v| User::from_json(&v))
                {
                    Ok(u) => {
                        users.insert(u.name.clone(), u);
                    }
                    Err((_, m)) => crate::warn!("ignoring {}: {m}", p.display()),
                }
            }
        }

        let now = util::now_secs();
        let mut files = HashMap::new();
        for e in std::fs::read_dir(&files_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "json") {
                continue;
            }
            let rec = read_json(&p)
                .ok_or((0, "unreadable".into()))
                .and_then(|v| FileRec::from_json(&v));
            let mut rec = match rec {
                Ok(r) if p.file_stem().is_some_and(|s| *s == *r.id) => r,
                Ok(_) => {
                    crate::warn!("ignoring {}: wrong name", p.display());
                    continue;
                }
                Err((_, m)) => {
                    crate::warn!("ignoring {}: {m}", p.display());
                    continue;
                }
            };
            let blob = p.with_extension("blob");
            rec.received = std::fs::metadata(&blob).map_or(0, |m| m.len());
            let stale = !rec.complete() && now.saturating_sub(rec.created) > PENDING_LIFETIME;
            if rec.received > rec.size || stale || !users.contains_key(&rec.owner) {
                let _ = std::fs::remove_file(&blob);
                let _ = std::fs::remove_file(&p);
                continue;
            }
            files.insert(rec.id.clone(), rec);
        }

        let sessions_path = dir.join("sessions.json");
        let mut sessions = HashMap::new();
        for s in read_json(&sessions_path)
            .and_then(|v| v.get("sessions").as_array().cloned())
            .into_iter()
            .flatten()
        {
            // "digest:issued:generation:user"
            let Some(parts) = s.as_str().map(|s| s.splitn(4, ':').collect::<Vec<_>>()) else {
                continue;
            };
            let [d, issued, generation, user] = parts[..] else {
                continue;
            };
            let (Some(d), Ok(issued), Ok(generation)) = (
                util::unhex(d).and_then(|d| <[u8; 32]>::try_from(d).ok()),
                issued.parse::<u64>(),
                generation.parse::<u64>(),
            ) else {
                continue;
            };
            if now.saturating_sub(issued) < SESSION_LIFETIME && users.contains_key(user) {
                sessions.insert(
                    d,
                    Session {
                        user: user.to_string(),
                        issued,
                        generation,
                    },
                );
            }
        }
        if !users.is_empty() {
            crate::info!(
                "vault: {} users, {} encrypted files",
                users.len(),
                files.len()
            );
        }
        Ok(Vault {
            users_dir,
            files_dir,
            invites_dir: invites_dir(data),
            sessions_path,
            pepper,
            quota,
            users: RwLock::new(users),
            files: RwLock::new(files),
            sessions: RwLock::new(sessions),
            failures: Mutex::new(HashMap::new()),
        })
    }

    fn save_sessions(&self, s: &HashMap<[u8; 32], Session>) {
        let list: Vec<Value> = s
            .iter()
            .map(|(d, s)| {
                Value::from(format!(
                    "{}:{}:{}:{}",
                    util::hex(d),
                    s.issued,
                    s.generation,
                    s.user
                ))
            })
            .collect();
        let _ = save_json(
            &self.sessions_path,
            &Value::object().with("sessions", Value::Array(list)),
        );
    }

    fn new_session(&self, user: &User) -> Result<String, Failure> {
        let token = crate::random::hex(16).map_err(|e| (500, format!("no random source: {e}")))?;
        let mut s = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if s.len() >= MAX_SESSIONS {
            if let Some(oldest) = s.iter().min_by_key(|(_, v)| v.issued).map(|(k, _)| *k) {
                s.remove(&oldest);
            }
        }
        s.insert(
            crate::sha256::sha256(token.as_bytes()),
            Session {
                user: user.name.clone(),
                issued: util::now_secs(),
                generation: user.generation,
            },
        );
        self.save_sessions(&s);
        Ok(token)
    }

    /// The user a request is logged in as.
    pub fn session_user(&self, req: &Request) -> Option<String> {
        let t = crate::auth::Auth::request_token(req)?;
        let d = crate::sha256::sha256(t.as_bytes());
        let s = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        let s = s.get(&d)?;
        let users = self.users.read().unwrap_or_else(|e| e.into_inner());
        let u = users.get(&s.user)?;
        (u.generation == s.generation
            && util::now_secs().saturating_sub(s.issued) < SESSION_LIFETIME)
            .then(|| s.user.clone())
    }

    fn end_session(&self, req: &Request) {
        if let Some(t) = crate::auth::Auth::request_token(req) {
            let mut s = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            if s.remove(&crate::sha256::sha256(t.as_bytes())).is_some() {
                self.save_sessions(&s);
            }
        }
    }

    fn ip_key(ip: Option<IpAddr>) -> String {
        // IPv6 clients usually own a whole /64.
        match ip {
            Some(IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_none() => {
                let s = v6.segments();
                format!("ip:{:x}:{:x}:{:x}:{:x}::", s[0], s[1], s[2], s[3])
            }
            Some(IpAddr::V6(v6)) => format!("ip:{}", v6.to_ipv4_mapped().expect("mapped")),
            Some(v4) => format!("ip:{v4}"),
            None => "ip:?".into(),
        }
    }

    fn check_limits(&self, ip: Option<IpAddr>, user: Option<&str>) -> Result<(), Failure> {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        let over = |k: &str, max| f.get(k).is_some_and(|(n, _)| *n >= max);
        if over(&Self::ip_key(ip), MAX_IP_FAILURES)
            || user.is_some_and(|u| over(&format!("user:{u}"), MAX_USER_FAILURES))
        {
            return fail(429, "too many failed attempts; try again later");
        }
        Ok(())
    }

    fn failed(&self, ip: Option<IpAddr>, user: Option<&str>) {
        {
            let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            for k in [Some(Self::ip_key(ip)), user.map(|u| format!("user:{u}"))]
                .into_iter()
                .flatten()
            {
                let e = f.entry(k).or_insert((0, Instant::now()));
                e.0 += 1;
            }
        }
        std::thread::sleep(FAILURE_DELAY);
    }

    fn verifier(&self, auth: &[u8]) -> [u8; 32] {
        keys::verifier(auth, &self.pepper)
    }

    /// Handles `/api/vault/...` (`segs` without the prefix).
    pub fn handle(&self, req: &Request, segs: &[&str], ip: Option<IpAddr>) -> Response {
        let r = self.route(req, segs, ip);
        match r {
            Ok(resp) => resp,
            Err((code, msg)) => {
                let resp = Response::error(code, &msg);
                if code == 401 {
                    resp.with_header("WWW-Authenticate", "Bearer realm=\"Beatlumen\"")
                } else {
                    resp
                }
            }
        }
    }

    fn route(&self, req: &Request, segs: &[&str], ip: Option<IpAddr>) -> Result<Response, Failure> {
        let m = req.method.as_str();
        let post = m == "POST";
        match segs {
            ["prelogin"] if post => return self.prelogin(req),
            ["register"] if post => return self.register(req, ip),
            ["login"] if post => return self.login(req, ip, false),
            ["recover"] if post => return self.login(req, ip, true),
            ["prelogin" | "register" | "login" | "recover"] => {
                return fail(405, "method not allowed")
            }
            _ => {}
        }
        let Some(me) = self.session_user(req) else {
            return fail(401, "authentication required");
        };
        let get = m == "GET" || m == "HEAD";
        match segs {
            ["logout"] if post => {
                self.end_session(req);
                Ok(Response::new(204))
            }
            ["account"] if get => {
                let users = self.users.read().unwrap_or_else(|e| e.into_inner());
                let u = users.get(&me).ok_or((401, "no such user".to_string()))?;
                Ok(Response::json(
                    200,
                    &Value::object()
                        .with("user", me.as_str())
                        .with("bundle", u.bundle()),
                ))
            }
            ["account"] if m == "DELETE" => self.delete_account(req, &me, ip),
            ["account", "keys"] if m == "PUT" => self.change_keys(req, &me, ip),
            ["users"] if get => {
                let users = self.users.read().unwrap_or_else(|e| e.into_inner());
                let mut list: Vec<&User> = users.values().collect();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                let v: Vec<Value> = list
                    .into_iter()
                    .map(|u| {
                        Value::object()
                            .with("name", u.name.as_str())
                            .with("public", b64(&u.public))
                    })
                    .collect();
                Ok(Response::json(200, &Value::Array(v)))
            }
            ["users", name] if get => {
                let users = self.users.read().unwrap_or_else(|e| e.into_inner());
                let u = users.get(*name).ok_or((404, "no such user".to_string()))?;
                Ok(Response::json(
                    200,
                    &Value::object()
                        .with("name", u.name.as_str())
                        .with("public", b64(&u.public)),
                ))
            }
            ["files"] if get => {
                let files = self.files.read().unwrap_or_else(|e| e.into_inner());
                let mut list: Vec<&FileRec> = files
                    .values()
                    .filter(|f| f.keys.contains_key(&me) && (f.complete() || f.owner == me))
                    .collect();
                list.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
                let used: u64 = files
                    .values()
                    .filter(|f| f.owner == me)
                    .map(|f| f.size)
                    .sum();
                let v = Value::object()
                    .with(
                        "files",
                        Value::Array(list.into_iter().map(|f| f.entry(&me)).collect()),
                    )
                    .with("used", used)
                    .with("quota", self.quota);
                Ok(Response::json(200, &v))
            }
            ["files"] if post => self.create_file(req, &me),
            ["files", id, rest @ ..] => {
                if !(id.len() == 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
                {
                    return fail(404, "file not found");
                }
                self.file_op(req, &me, id, rest)
            }
            _ => fail(404, "not found"),
        }
    }

    fn prelogin(&self, req: &Request) -> Result<Response, Failure> {
        let v = body(req)?;
        let name = user_name(&v, "user")?;
        let users = self.users.read().unwrap_or_else(|e| e.into_inner());
        // Unknown names get a stable made-up salt, so prelogin does not
        // tell which names exist.
        let (salt, params) = match users.get(&name) {
            Some(u) => (u.salt, u.params),
            None => (keys::fake_salt(&self.pepper, &name), KdfParams::DEFAULT),
        };
        Ok(Response::json(
            200,
            &Value::object()
                .with("salt", b64(&salt))
                .with("params", params_json(params)),
        ))
    }

    fn login(
        &self,
        req: &Request,
        ip: Option<IpAddr>,
        recovery: bool,
    ) -> Result<Response, Failure> {
        let v = body(req)?;
        let name = user_name(&v, "user")?;
        let auth = bytes(&v, if recovery { "recoveryAuth" } else { "auth" }, Some(32))?;
        self.check_limits(ip, Some(&name))?;
        let check = self.verifier(&auth);
        let user = {
            let users = self.users.read().unwrap_or_else(|e| e.into_inner());
            users
                .get(&name)
                .filter(|u| {
                    ct_eq(
                        &check,
                        if recovery {
                            &u.recovery_verifier
                        } else {
                            &u.verifier
                        },
                    )
                })
                .cloned()
        };
        let Some(user) = user else {
            crate::warn!(
                "vault: failed {} for {name:?} from {}",
                if recovery { "recovery" } else { "login" },
                Self::ip_key(ip)
            );
            self.failed(ip, Some(&name));
            return fail(401, "wrong user name or password");
        };
        let token = self.new_session(&user)?;
        if recovery {
            crate::info!("vault: {name} logged in with the recovery code");
        }
        Ok(Response::json(
            200,
            &Value::object()
                .with("token", token)
                .with("user", name.as_str())
                .with("bundle", user.bundle()),
        ))
    }

    fn register(&self, req: &Request, ip: Option<IpAddr>) -> Result<Response, Failure> {
        let v = body(req)?;
        self.check_limits(ip, None)?;
        let name = user_name(&v, "user")?;
        let invite = self
            .invites_dir
            .join(format!("{}.json", invite_key(v.str_or("invite", ""))));
        let valid = read_json(&invite)
            .and_then(|i| i.get("expires").as_u64())
            .is_some_and(|t| t > util::now_secs());
        if !valid {
            self.failed(ip, None);
            return fail(403, "invalid or expired invitation");
        }
        let b = v.get("bundle");
        let public = bytes(b, "public", Some(128))?;
        PublicIdentity::from_bytes(&public).map_err(|_| (400, "invalid public key".to_string()))?;
        let user = User {
            salt: array(b, "salt")?,
            params: params_from(b.get("params"))?,
            verifier: self.verifier(&bytes(&v, "auth", Some(32))?),
            recovery_verifier: self.verifier(&bytes(&v, "recoveryAuth", Some(32))?),
            master_by_password: bytes(b, "masterByPassword", Some(WRAPPED_MASTER_LEN))?,
            master_by_recovery: bytes(b, "masterByRecovery", Some(WRAPPED_MASTER_LEN))?,
            identity: bytes(b, "identity", Some(WRAPPED_IDENTITY_LEN))?,
            public,
            generation: 0,
            created: util::now_secs(),
            name: name.clone(),
        };
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        if users.contains_key(&name) {
            return fail(409, "user name taken");
        }
        // Removing the invitation is what makes it single-use.
        if std::fs::remove_file(&invite).is_err() {
            return fail(403, "invalid or expired invitation");
        }
        save_json(
            &self.users_dir.join(format!("{name}.json")),
            &user.to_json(),
        )?;
        users.insert(name.clone(), user.clone());
        drop(users);
        crate::info!("vault: user {name} registered");
        let token = self.new_session(&user)?;
        Ok(Response::json(
            201,
            &Value::object()
                .with("token", token)
                .with("user", name.as_str()),
        ))
    }

    /// Checks the proof of a sensitive change: the current auth key or the
    /// recovery auth key (a stolen session token alone is not enough).
    fn check_proof(&self, v: &Value, user: &User, ip: Option<IpAddr>) -> Result<(), Failure> {
        self.check_limits(ip, Some(&user.name))?;
        let p = v.get("proof");
        let ok = match (p.get("auth").as_str(), p.get("recoveryAuth").as_str()) {
            (Some(_), _) => ct_eq(&self.verifier(&bytes(p, "auth", Some(32))?), &user.verifier),
            (_, Some(_)) => ct_eq(
                &self.verifier(&bytes(p, "recoveryAuth", Some(32))?),
                &user.recovery_verifier,
            ),
            _ => return fail(400, "missing \"proof\""),
        };
        if !ok {
            self.failed(ip, Some(&user.name));
            return fail(403, "wrong password");
        }
        Ok(())
    }

    /// New password and/or new recovery code.
    fn change_keys(
        &self,
        req: &Request,
        me: &str,
        ip: Option<IpAddr>,
    ) -> Result<Response, Failure> {
        let v = body(req)?;
        let mut user = {
            let users = self.users.read().unwrap_or_else(|e| e.into_inner());
            users
                .get(me)
                .cloned()
                .ok_or((401, "no such user".to_string()))?
        };
        self.check_proof(&v, &user, ip)?;
        let (pw, rc) = (v.get("password"), v.get("recovery"));
        if pw.is_null() && rc.is_null() {
            return fail(400, "nothing to change");
        }
        if !pw.is_null() {
            user.salt = array(pw, "salt")?;
            user.params = params_from(pw.get("params"))?;
            user.verifier = self.verifier(&bytes(pw, "auth", Some(32))?);
            user.master_by_password = bytes(pw, "masterByPassword", Some(WRAPPED_MASTER_LEN))?;
            // Logs out everywhere else.
            user.generation += 1;
        }
        if !rc.is_null() {
            user.recovery_verifier = self.verifier(&bytes(rc, "recoveryAuth", Some(32))?);
            user.master_by_recovery = bytes(rc, "masterByRecovery", Some(WRAPPED_MASTER_LEN))?;
        }
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        save_json(&self.users_dir.join(format!("{me}.json")), &user.to_json())?;
        users.insert(me.to_string(), user.clone());
        drop(users);
        crate::info!(
            "vault: {me} changed{}{}",
            if pw.is_null() { "" } else { " the password" },
            if rc.is_null() {
                ""
            } else {
                " the recovery code"
            }
        );
        let mut out = Value::object();
        if !pw.is_null() {
            self.end_session(req);
            out.set("token", self.new_session(&user)?);
        }
        Ok(Response::json(200, &out))
    }

    fn delete_account(
        &self,
        req: &Request,
        me: &str,
        ip: Option<IpAddr>,
    ) -> Result<Response, Failure> {
        let v = body(req)?;
        let user = {
            let users = self.users.read().unwrap_or_else(|e| e.into_inner());
            users
                .get(me)
                .cloned()
                .ok_or((401, "no such user".to_string()))?
        };
        self.check_proof(&v, &user, ip)?;
        {
            let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<String> = files.keys().cloned().collect();
            for id in ids {
                let f = files.get_mut(&id).expect("listed");
                if f.owner == me {
                    self.remove_file(&id);
                    files.remove(&id);
                } else if f.keys.remove(me).is_some() {
                    let _ = save_json(&self.files_dir.join(format!("{id}.json")), &f.to_json());
                }
            }
        }
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        std::fs::remove_file(self.users_dir.join(format!("{me}.json")))
            .map_err(|_| (500, "cannot delete".to_string()))?;
        users.remove(me);
        drop(users);
        let mut s = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        s.retain(|_, s| s.user != me);
        self.save_sessions(&s);
        crate::info!("vault: user {me} deleted their account");
        Ok(Response::new(204))
    }

    fn remove_file(&self, id: &str) {
        let _ = std::fs::remove_file(self.files_dir.join(format!("{id}.blob")));
        let _ = std::fs::remove_file(self.files_dir.join(format!("{id}.json")));
    }

    fn save_file(&self, f: &FileRec) -> Result<(), Failure> {
        save_json(&self.files_dir.join(format!("{}.json", f.id)), &f.to_json())
    }

    fn meta_and_signature(v: &Value) -> Result<(Vec<u8>, Vec<u8>), Failure> {
        let meta = bytes(v, "meta", None)?;
        if meta.len() < bl_vault::aead::OVERHEAD || meta.len() > MAX_META {
            return fail(400, "invalid \"meta\"");
        }
        Ok((meta, bytes(v, "signature", Some(64))?))
    }

    fn create_file(&self, req: &Request, me: &str) -> Result<Response, Failure> {
        let v = body(req)?;
        let header = bytes(&v, "header", Some(HEADER_LEN))?;
        let h = Header::parse(&header).map_err(|e| (400, e.to_string()))?;
        let (meta, signature) = Self::meta_and_signature(&v)?;
        let key = bytes(&v, "key", Some(SEALED_KEY_LEN))?;
        let size = h.encrypted_len();
        if size > MAX_FILE {
            return fail(413, "file too large");
        }
        let id = util::hex(&h.file_id);
        let now = util::now_secs();
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        // Drop abandoned uploads.
        let stale: Vec<String> = files
            .values()
            .filter(|f| !f.complete() && now.saturating_sub(f.created) > PENDING_LIFETIME)
            .map(|f| f.id.clone())
            .collect();
        for s in stale {
            self.remove_file(&s);
            files.remove(&s);
        }
        if files.contains_key(&id) {
            return fail(409, "file exists");
        }
        let mine: Vec<&FileRec> = files.values().filter(|f| f.owner == me).collect();
        if mine.len() >= MAX_FILES_PER_USER {
            return fail(507, "too many files");
        }
        let used: u64 = mine.iter().map(|f| f.size).sum();
        if self.quota > 0 && used + size > self.quota {
            return fail(507, "storage quota exceeded");
        }
        let rec = FileRec {
            id: id.clone(),
            owner: me.to_string(),
            received: header.len() as u64,
            header,
            size,
            meta,
            signature,
            keys: [(me.to_string(), key)].into_iter().collect(),
            created: now,
            updated: now,
        };
        let blob = self.files_dir.join(format!("{id}.blob"));
        std::fs::write(&blob, &rec.header).map_err(|e| {
            crate::warn!("cannot write {}: {e}", blob.display());
            (500, "cannot save".to_string())
        })?;
        util::make_private(&blob);
        self.save_file(&rec)?;
        let entry = rec.entry(me);
        files.insert(id, rec);
        Ok(Response::json(201, &entry))
    }

    fn file_op(
        &self,
        req: &Request,
        me: &str,
        id: &str,
        rest: &[&str],
    ) -> Result<Response, Failure> {
        let m = req.method.as_str();
        let get = m == "GET" || m == "HEAD";
        let not_found = || (404, "file not found".to_string());
        if get {
            let files = self.files.read().unwrap_or_else(|e| e.into_inner());
            let f = files
                .get(id)
                .filter(|f| f.keys.contains_key(me))
                .ok_or_else(not_found)?;
            return match rest {
                [] => Ok(Response::json(200, &f.entry(me))),
                ["data"] if f.complete() => {
                    let path = self.files_dir.join(format!("{id}.blob"));
                    let resp =
                        Response::file(&path, "application/octet-stream", req.header("range"));
                    // Contents never change under an id.
                    Ok(resp.with_header("Cache-Control", "private, max-age=31536000, immutable"))
                }
                ["data"] => fail(409, "upload not finished"),
                _ => Err(not_found()),
            };
        }
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        let f = files
            .get_mut(id)
            .filter(|f| f.keys.contains_key(me))
            .ok_or_else(not_found)?;
        let owner = f.owner == me;
        let now = util::now_secs();
        match (m, rest) {
            ("PUT", ["data"]) if owner => {
                let offset: u64 = req
                    .query("offset")
                    .and_then(|o| o.parse().ok())
                    .ok_or((400, "missing offset".to_string()))?;
                if f.complete() {
                    return fail(409, "upload already finished");
                }
                if offset != f.received {
                    return Err((409, format!("expected offset {}", f.received)));
                }
                if req.body.is_empty() || offset + req.body.len() as u64 > f.size {
                    return fail(400, "invalid length");
                }
                let path = self.files_dir.join(format!("{id}.blob"));
                let written = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .and_then(|mut file| {
                        file.write_all(&req.body)?;
                        file.sync_data()
                    });
                if let Err(e) = written {
                    // Cut back to what was stored before.
                    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
                        let _ = file.set_len(f.received);
                    }
                    crate::warn!("cannot write {}: {e}", path.display());
                    return fail(500, "cannot save");
                }
                f.received += req.body.len() as u64;
                if f.complete() {
                    f.updated = now;
                    self.save_file(f)?;
                }
                Ok(Response::json(
                    200,
                    &Value::object()
                        .with("received", f.received)
                        .with("complete", f.complete()),
                ))
            }
            ("PUT", ["meta"]) if owner => {
                let v = body(req)?;
                let (meta, signature) = Self::meta_and_signature(&v)?;
                f.meta = meta;
                f.signature = signature;
                f.updated = now;
                self.save_file(f)?;
                Ok(Response::new(204))
            }
            ("POST", ["keys"]) if owner => {
                let v = body(req)?;
                let to = user_name(&v, "user")?;
                let key = bytes(&v, "key", Some(SEALED_KEY_LEN))?;
                if !self
                    .users
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&to)
                {
                    return fail(404, "no such user");
                }
                if to == me {
                    return fail(400, "cannot share with yourself");
                }
                if f.keys.len() >= MAX_RECIPIENTS && !f.keys.contains_key(&to) {
                    return fail(507, "too many recipients");
                }
                f.keys.insert(to, key);
                f.updated = now;
                self.save_file(f)?;
                Ok(Response::new(204))
            }
            // The owner revokes access, or a recipient removes a file
            // shared with them.
            ("DELETE", ["keys", user]) if (owner && *user != me) || (!owner && *user == me) => {
                if f.keys.remove(*user).is_none() {
                    return fail(404, "not shared with this user");
                }
                f.updated = now;
                self.save_file(f)?;
                Ok(Response::new(204))
            }
            ("DELETE", []) if owner => {
                self.remove_file(id);
                files.remove(id);
                Ok(Response::new(204))
            }
            _ if !owner => fail(403, "only the owner can do this"),
            _ => fail(405, "method not allowed"),
        }
    }
}
