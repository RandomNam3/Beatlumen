//! Account and end-to-end encrypted files on the server (design:
//! `docs/ENCRYPTION.md`, server side: `beatlumen-server/src/vault.rs`).
//!
//! The keys exist only in memory while the account is unlocked; the
//! password is needed after every start. Everything sent to the server is
//! encrypted here first. Public keys of other users are pinned on first use
//! (per server and account) and a changed key is reported, never used
//! silently.

use crate::core::{Core, Req};
use crate::host::{Host, HttpRequest, HttpResponse};
use crate::jobs::{extension, Mailbox};
use crate::player::SourceOpener;
use bl_json::Value;
use bl_net::api::Track;
use bl_net::{base64_decode, base64_encode};
use bl_vault::file::{self, Header};
use bl_vault::identity::{safety_number, PublicIdentity};
use bl_vault::keys::{
    Account, KdfParams, KeyBundle, PasswordChange, PasswordKeys, RecoveryChange, RecoveryKeys,
    Registration,
};
use bl_vault::recovery::RecoveryCode;
use bl_vault::{sealed, Secret};
use std::collections::HashMap;
use std::sync::Arc;

/// Queue ids of encrypted files: `vault:<file id>`.
pub const ID_PREFIX: &str = "vault:";
/// Upload part size (the server accepts up to 4 MB per request).
const PART: usize = 3 * 1024 * 1024;
const PINS_KEY: &str = "vault-pins.json";
/// Decrypted files kept in memory where they cannot be streamed (web).
const PLAIN_CACHE: usize = 2;

#[derive(Debug, Clone)]
pub(crate) enum VaultReq {
    Prelogin {
        user: String,
    },
    Login,
    Register,
    Recover,
    Keys,
    Users,
    Files,
    Create(u64),
    Data(u64),
    Changed(&'static str),
    Logout,
    Download {
        id: String,
        autoplay: bool,
        generation: u64,
    },
}

/// Results of slow work (Argon2, encryption) done off the main thread.
enum Done {
    Keys {
        user: String,
        keys: PasswordKeys,
    },
    Registered {
        invite: String,
        account: Account,
        reg: Registration,
        code: RecoveryCode,
    },
    /// New password and/or recovery code, ready to upload.
    Change {
        account: Account,
        proof: Value,
        password: Option<PasswordChange>,
        recovery: Option<(RecoveryChange, RecoveryCode)>,
    },
    Encrypted {
        serial: u64,
        header: Header,
        data: Vec<u8>,
        meta: Vec<u8>,
        key: Secret,
    },
    Failed {
        account: Option<Account>,
        msg: String,
    },
    UploadFailed {
        serial: u64,
        msg: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyStatus {
    /// Pinned on first use, not compared in person.
    Pinned,
    /// Safety number confirmed by the user.
    Verified,
    /// The server now shows another key than the pinned one.
    Changed,
}

pub(crate) struct Contact {
    pub name: String,
    pub public: PublicIdentity,
    pub status: KeyStatus,
}

pub(crate) struct VaultFile {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub codec: String,
    pub created: u64,
    pub complete: bool,
    pub recipients: Vec<String>,
    /// Why the file cannot be trusted or opened (None = fine).
    pub problem: Option<String>,
    pub key: Option<Secret>,
    pub header: Option<Header>,
}

struct Upload {
    serial: u64,
    name: String,
    file_id: String,
    data: Vec<u8>,
    sent: usize,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Vault {
    pub user: String,
    token: String,
    account: Option<Account>,
    bundle: Option<KeyBundle>,
    pub files: Vec<VaultFile>,
    pub contacts: Vec<Contact>,
    used: u64,
    quota: u64,
    /// Pinned keys of this server and account: name -> (Ed25519 key hex, verified).
    pins: HashMap<String, (String, bool)>,
    pub busy: Option<String>,
    pub error: String,
    /// Shown once after registration or a new code; cleared by the user.
    pub recovery_code: Option<String>,
    pub rev: u64,
    jobs: Mailbox<Done>,
    pending_keys: Option<PasswordKeys>,
    pending_account: Option<Account>,
    pending_code: Option<RecoveryCode>,
    pending_password: Option<String>,
    pending_proof: Option<Value>,
    uploads: Vec<Upload>,
    next_serial: u64,
    /// The next file handed over as bytes (browser) is uploaded here.
    pub expect_upload: bool,
    plain: Vec<(String, Arc<Vec<u8>>)>,
}

fn b64(b: &[u8]) -> Value {
    Value::from(base64_encode(b))
}

fn unb64(v: &Value) -> Option<Vec<u8>> {
    v.as_str().and_then(base64_decode)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn params_json(p: KdfParams) -> Value {
    Value::object()
        .with("m", p.m_kib as u64)
        .with("t", p.t as u64)
        .with("p", p.p as u64)
}

fn params_from(v: &Value) -> Option<KdfParams> {
    let n = |k: &str| v.get(k).as_u64().and_then(|x| u32::try_from(x).ok());
    Some(KdfParams {
        m_kib: n("m")?,
        t: n("t")?,
        p: n("p")?,
    })
}

fn bundle_json(b: &KeyBundle) -> Value {
    Value::object()
        .with("salt", b64(&b.salt))
        .with("params", params_json(b.params))
        .with("masterByPassword", b64(&b.master_by_password))
        .with("masterByRecovery", b64(&b.master_by_recovery))
        .with("identity", b64(&b.identity))
        .with("public", b64(&b.public.to_bytes()))
}

fn bundle_from(v: &Value) -> Option<KeyBundle> {
    Some(KeyBundle {
        salt: unb64(v.get("salt"))?.try_into().ok()?,
        params: params_from(v.get("params"))?,
        master_by_password: unb64(v.get("masterByPassword"))?,
        master_by_recovery: unb64(v.get("masterByRecovery"))?,
        identity: unb64(v.get("identity"))?,
        public: PublicIdentity::from_bytes(&unb64(v.get("public"))?).ok()?,
    })
}

fn password_json(c: &PasswordChange) -> Value {
    Value::object()
        .with("salt", b64(&c.salt))
        .with("params", params_json(c.params))
        .with("auth", b64(c.auth.as_bytes()))
        .with("masterByPassword", b64(&c.master_by_password))
}

fn recovery_json(c: &RecoveryChange) -> Value {
    Value::object()
        .with("recoveryAuth", b64(c.recovery_auth.as_bytes()))
        .with("masterByRecovery", b64(&c.master_by_recovery))
}

impl Vault {
    pub fn file(&self, id: &str) -> Option<&VaultFile> {
        let id = id.strip_prefix(ID_PREFIX).unwrap_or(id);
        self.files.iter().find(|f| f.id == id)
    }

    /// Forgets keys, files and session.
    fn lock(&mut self) {
        let user = std::mem::take(&mut self.user);
        let expect = self.expect_upload;
        *self = Vault {
            jobs: self.jobs.clone(),
            next_serial: self.next_serial,
            rev: self.rev + 1,
            ..Default::default()
        };
        self.user = user;
        self.expect_upload = expect;
    }

    pub fn plain(&self, id: &str) -> Option<Arc<Vec<u8>>> {
        self.plain
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, d)| d.clone())
    }

    fn remember_plain(&mut self, id: &str, data: Arc<Vec<u8>>) {
        self.plain.retain(|(k, _)| k != id);
        self.plain.push((id.to_string(), data));
        if self.plain.len() > PLAIN_CACHE {
            self.plain.remove(0);
        }
    }
}

impl Core {
    fn vault_request(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        path: &str,
        body: Option<Value>,
        req: VaultReq,
    ) {
        self.vault_send(
            host,
            method,
            path,
            body.map(|b| b.to_json().into_bytes()),
            "application/json",
            req,
        );
    }

    fn vault_send(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
        req: VaultReq,
    ) {
        let mut headers = vec![("Content-Type".to_string(), content_type.to_string())];
        if !self.vault.token.is_empty() {
            headers.push((
                "Authorization".into(),
                format!("Bearer {}", self.vault.token),
            ));
        }
        let id = host.http_request(HttpRequest {
            method: method.into(),
            url: self.server.url(path),
            headers,
            body: body.unwrap_or_default(),
        });
        self.requests
            .insert(id, Req::Vault(req, self.vault.token.clone()));
    }

    fn vault_busy(&mut self, what: &str) {
        self.vault.busy = Some(self.tr(what).to_string());
        self.vault.error.clear();
        self.vault.rev += 1;
    }

    fn vault_fail(&mut self, msg: impl Into<String>) {
        self.vault.busy = None;
        self.vault.error = msg.into();
        self.vault.pending_password = None;
        self.vault.pending_keys = None;
        self.vault.rev += 1;
    }

    fn vault_ready(&self) -> bool {
        matches!(
            self.server.state,
            crate::server::ServerState::Connected(_) | crate::server::ServerState::NeedsLogin(_)
        )
    }

    /// Runs `work` in the background (directly where there are no threads)
    /// and delivers its result to the next tick.
    fn vault_job(&mut self, host: &mut dyn Host, work: impl FnOnce() -> Done + Send + 'static) {
        let mailbox = self.vault.jobs.clone();
        host.spawn(Box::new(move || mailbox.post(work())));
    }

    // ---- account ---------------------------------------------------------

    pub(crate) fn vault_login(&mut self, host: &mut dyn Host, user: &str, password: &str) {
        let user = match bl_vault::keys::normalize_user(user) {
            Ok(u) => u,
            Err(e) => return self.vault_fail(e.to_string()),
        };
        if !self.vault_ready() {
            return self.vault_fail(self.tr("Not connected to a server"));
        }
        self.vault_busy("Unlocking…");
        self.vault.pending_password = Some(password.to_string());
        let body = Value::object().with("user", user.as_str());
        self.vault_request(
            host,
            "POST",
            "/api/vault/prelogin",
            Some(body),
            VaultReq::Prelogin { user },
        );
    }

    pub(crate) fn vault_register(
        &mut self,
        host: &mut dyn Host,
        invite: &str,
        user: &str,
        password: &str,
    ) {
        let user = match bl_vault::keys::normalize_user(user) {
            Ok(u) => u,
            Err(e) => return self.vault_fail(e.to_string()),
        };
        if password.chars().count() < 10 {
            return self.vault_fail(self.tr("Use a password of at least 10 characters"));
        }
        self.vault_busy("Creating keys…");
        let (invite, password) = (invite.trim().to_string(), password.to_string());
        self.vault_job(host, move || match Account::register(&user, &password) {
            Ok((account, reg, code)) => Done::Registered {
                invite,
                account,
                reg,
                code,
            },
            Err(e) => Done::Failed {
                account: None,
                msg: e.to_string(),
            },
        });
    }

    pub(crate) fn vault_recover(
        &mut self,
        host: &mut dyn Host,
        user: &str,
        code: &str,
        password: &str,
    ) {
        let user = match bl_vault::keys::normalize_user(user) {
            Ok(u) => u,
            Err(e) => return self.vault_fail(e.to_string()),
        };
        let code = match RecoveryCode::parse(code) {
            Ok(c) => c,
            Err(_) => {
                return self.vault_fail(
                    self.tr("The recovery code is not valid (check for typing errors)"),
                )
            }
        };
        if password.chars().count() < 10 {
            return self.vault_fail(self.tr("Use a password of at least 10 characters"));
        }
        self.vault_busy("Recovering…");
        let rauth = RecoveryKeys::derive(&code).auth;
        self.vault.pending_proof =
            Some(Value::object().with("recoveryAuth", b64(rauth.as_bytes())));
        self.vault.pending_code = Some(code);
        self.vault.pending_password = Some(password.to_string());
        self.vault.user = user.clone();
        let body = Value::object()
            .with("user", user.as_str())
            .with("recoveryAuth", b64(rauth.as_bytes()));
        self.vault_request(
            host,
            "POST",
            "/api/vault/recover",
            Some(body),
            VaultReq::Recover,
        );
    }

    pub(crate) fn vault_change_password(&mut self, host: &mut dyn Host, current: &str, new: &str) {
        if new.chars().count() < 10 {
            return self.vault_fail(self.tr("Use a password of at least 10 characters"));
        }
        self.vault_change(host, current, Some(new.to_string()), false);
    }

    pub(crate) fn vault_new_recovery_code(&mut self, host: &mut dyn Host, current: &str) {
        self.vault_change(host, current, None, true);
    }

    /// Proves the current password locally and on the server, then sets a
    /// new password and/or recovery code.
    fn vault_change(
        &mut self,
        host: &mut dyn Host,
        current: &str,
        new: Option<String>,
        new_code: bool,
    ) {
        let (Some(account), Some(bundle)) = (self.vault.account.take(), self.vault.bundle.clone())
        else {
            return self.vault_fail(self.tr("The account is locked"));
        };
        self.vault_busy("Changing keys…");
        let current = current.to_string();
        self.vault_job(host, move || {
            let fail = |account, msg: String| Done::Failed {
                account: Some(account),
                msg,
            };
            let keys = match PasswordKeys::derive(&current, &bundle.salt, bundle.params) {
                Ok(k) => k,
                Err(e) => return fail(account, e.to_string()),
            };
            if Account::unlock(&account.user, &keys, &bundle).is_err() {
                return fail(account, "wrong password".into());
            }
            let proof = Value::object().with("auth", b64(keys.auth.as_bytes()));
            let password = match new.map(|p| account.change_password(&p)).transpose() {
                Ok(p) => p,
                Err(e) => return fail(account, e.to_string()),
            };
            let recovery = match new_code.then(|| account.new_recovery_code()).transpose() {
                Ok(r) => r,
                Err(e) => return fail(account, e.to_string()),
            };
            Done::Change {
                account,
                proof,
                password,
                recovery,
            }
        });
    }

    pub(crate) fn vault_logout(&mut self, host: &mut dyn Host) {
        if !self.vault.token.is_empty() {
            self.vault_request(host, "POST", "/api/vault/logout", None, VaultReq::Logout);
        }
        // The account's session may also have opened the library.
        if !self.vault.token.is_empty() && self.server.token == self.vault.token {
            self.server.token.clear();
            self.settings.server_token.clear();
            self.settings_changed();
        }
        self.vault_lock(host);
    }

    /// Forgets the keys and stops playing encrypted files.
    pub(crate) fn vault_lock(&mut self, host: &mut dyn Host) {
        self.vault.lock();
        if self
            .current
            .as_ref()
            .is_some_and(|t| t.id.starts_with(ID_PREFIX))
        {
            self.player.stop(host);
        }
    }

    fn vault_logged_in(
        &mut self,
        host: &mut dyn Host,
        token: String,
        account: Account,
        bundle: Option<KeyBundle>,
    ) {
        self.vault.user = account.user.clone();
        self.vault.token = token.clone();
        self.vault.account = Some(account);
        if bundle.is_some() {
            self.vault.bundle = bundle;
        }
        self.vault.busy = None;
        self.vault.error.clear();
        self.vault.pending_password = None;
        self.vault.pending_keys = None;
        if self.settings.vault_user != self.vault.user {
            self.settings.vault_user = self.vault.user.clone();
            self.settings_changed();
        }
        self.load_pins(host);
        // An account also opens a password-protected server's library.
        if let crate::server::ServerState::NeedsLogin(_) = self.server.state {
            self.server.token = token.clone();
            self.settings.server_token = token;
            self.settings_changed();
            self.connect_server(host);
        }
        self.vault.rev += 1;
        self.vault_refresh(host);
    }

    pub(crate) fn vault_refresh(&mut self, host: &mut dyn Host) {
        if self.vault.account.is_some() {
            self.vault_request(host, "GET", "/api/vault/users", None, VaultReq::Users);
        }
    }

    fn pins_scope(&self) -> String {
        format!("{}|{}", self.server.base, self.vault.user)
    }

    fn load_pins(&mut self, host: &mut dyn Host) {
        let all = host
            .storage_read(PINS_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .unwrap_or_default();
        self.vault.pins = all
            .get(&self.pins_scope())
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            (
                                v.str_or("key", "").to_string(),
                                v.bool_or("verified", false),
                            ),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    fn save_pins(&mut self, host: &mut dyn Host) {
        let mut all = host
            .storage_read(PINS_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .filter(|v| v.as_object().is_some())
            .unwrap_or_else(Value::object);
        let mine: bl_json::Map = self
            .vault
            .pins
            .iter()
            .map(|(k, (key, verified))| {
                (
                    k.clone(),
                    Value::object()
                        .with("key", key.as_str())
                        .with("verified", *verified),
                )
            })
            .collect();
        all.set(self.pins_scope(), mine);
        host.storage_write(PINS_KEY, all.to_json().as_bytes());
    }

    /// Marks a contact's key as confirmed in person (safety number), or
    /// accepts a changed key after the user was warned.
    pub(crate) fn vault_trust(&mut self, host: &mut dyn Host, user: &str, verified: bool) {
        let Some(c) = self.vault.contacts.iter_mut().find(|c| c.name == user) else {
            return;
        };
        c.status = if verified {
            KeyStatus::Verified
        } else {
            KeyStatus::Pinned
        };
        self.vault
            .pins
            .insert(c.name.clone(), (hex(&c.public.sign), verified));
        self.save_pins(host);
        self.vault.rev += 1;
        self.vault_refresh(host);
    }

    // ---- files -----------------------------------------------------------

    fn decode_file(&self, e: &Value) -> Option<VaultFile> {
        let account = self.vault.account.as_ref()?;
        let me = &account.user;
        let mut f = VaultFile {
            id: e.str_or("id", "").to_string(),
            owner: e.str_or("owner", "").to_string(),
            name: String::new(),
            mime: String::new(),
            size: e.get("size").as_u64().unwrap_or(0),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            codec: String::new(),
            created: e.get("created").as_u64().unwrap_or(0),
            complete: e.bool_or("complete", false),
            recipients: e
                .get("recipients")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            problem: None,
            key: None,
            header: None,
        };
        let header = unb64(e.get("header")).and_then(|h| Header::parse(&h).ok());
        let Some(header) = header.filter(|h| hex(&h.file_id) == f.id) else {
            f.problem = Some(self.tr("Damaged file").into());
            return Some(f);
        };
        let key = unb64(e.get("key"))
            .and_then(|k| {
                sealed::open(
                    &account.identity,
                    &sealed::file_key_aad(&header.file_id, me),
                    &k,
                )
                .ok()
            })
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .map(Secret::from_bytes);
        let Some(key) = key else {
            f.problem = Some(self.tr("Cannot decrypt the file key").into());
            return Some(f);
        };
        // The owner's key: our own, or the pinned one of a contact.
        let owner_key = if f.owner == *me {
            Some(account.identity.public())
        } else {
            match self.vault.contacts.iter().find(|c| c.name == f.owner) {
                Some(c) if c.status == KeyStatus::Changed => {
                    f.problem = Some(format!("{} {}", self.tr("Key changed:"), f.owner));
                    return Some(f);
                }
                Some(c) => Some(c.public.clone()),
                None => None,
            }
        };
        let meta = unb64(e.get("meta")).unwrap_or_default();
        let sig: Option<[u8; 64]> = unb64(e.get("signature")).and_then(|s| s.try_into().ok());
        let signed = match (owner_key, sig) {
            (Some(k), Some(s)) => file::verify(&k, &header, &meta, &key, &s).is_ok(),
            _ => false,
        };
        if !signed {
            f.problem = Some(self.tr("Signature invalid: not from the owner").into());
            return Some(f);
        }
        let m = file::open_meta(&key, &header, &meta)
            .ok()
            .and_then(|m| bl_json::parse_bytes(&m).ok())
            .unwrap_or_default();
        f.name = m.str_or("name", "").chars().take(200).collect();
        f.mime = m.str_or("type", "").to_string();
        f.size = m.get("size").as_u64().unwrap_or(f.size);
        f.title = m.str_or("title", "").to_string();
        f.artist = m.str_or("artist", "").to_string();
        f.album = m.str_or("album", "").to_string();
        f.duration_ms = m.get("durationMs").as_u64().unwrap_or(0);
        f.codec = m.str_or("codec", "").to_string();
        if f.title.is_empty() {
            f.title = f
                .name
                .rsplit_once('.')
                .map_or(f.name.clone(), |(s, _)| s.to_string());
        }
        f.key = Some(key);
        f.header = Some(header);
        Some(f)
    }

    /// Track metadata of an encrypted file for the queue and player.
    pub(crate) fn vault_track(&self, id: &str) -> Option<Track> {
        let f = self.vault.file(id)?;
        Some(Track {
            id: format!("{ID_PREFIX}{}", f.id),
            title: f.title.clone(),
            artists: if f.artist.is_empty() {
                Vec::new()
            } else {
                vec![f.artist.clone()]
            },
            album: f.album.clone(),
            duration_ms: f.duration_ms,
            codec: f.codec.clone(),
            size: f.size,
            modified: f.created,
            path: f.name.clone(),
            ..Default::default()
        })
    }

    fn vault_ext(f: &VaultFile) -> Option<String> {
        if f.codec.is_empty() {
            extension(&f.name)
        } else {
            Some(f.codec.clone())
        }
    }

    /// Opens an encrypted file for decoding: streamed with byte ranges and
    /// decrypted chunk by chunk (native), or from memory once downloaded.
    pub(crate) fn vault_opener(&self, host: &dyn Host, id: &str) -> Option<SourceOpener> {
        let f = self.vault.file(id)?;
        let (key, header) = (f.key.clone()?, f.header.clone()?);
        let ext = Self::vault_ext(f);
        if let Some(data) = self.vault.plain(&f.id) {
            return Some(Box::new(move || {
                Ok((
                    Box::new(std::io::Cursor::new(ArcBytes(data))) as Box<dyn bl_media::Source>,
                    ext,
                ))
            }));
        }
        if !host.has_threads() || host.audio_is_push() || cfg!(target_arch = "wasm32") {
            return None;
        }
        let url = self.server.url(&format!("/api/vault/files/{}/data", f.id));
        let headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.vault.token),
        )];
        Some(Box::new(move || {
            let reader = open_stream(&url, &headers, key, header)?;
            Ok((reader, ext))
        }))
    }

    /// Downloads and decrypts a whole file (platforms without streaming).
    pub(crate) fn vault_download(&mut self, host: &mut dyn Host, id: &str, autoplay: bool) {
        let Some(f) = self.vault.file(id) else {
            return self.toast(self.tr("The account is locked"), true);
        };
        let path = format!("/api/vault/files/{}/data", f.id);
        let req = VaultReq::Download {
            id: f.id.clone(),
            autoplay,
            generation: self.play_generation,
        };
        self.download_progress = Some(f.title.clone());
        self.vault_request(host, "GET", &path, None, req);
    }

    /// Reads, encrypts and uploads local files (paths) in the background.
    pub(crate) fn vault_upload_paths(&mut self, host: &mut dyn Host, paths: Vec<String>) {
        for p in paths {
            let name = std::path::Path::new(&p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".into());
            if host.has_threads() {
                let serial = self.vault_add_upload(&name);
                self.vault_job(host, move || match std::fs::read(&p) {
                    Ok(data) => encrypt_job(serial, &name, data),
                    Err(e) => Done::UploadFailed {
                        serial,
                        msg: format!("{p}: {e}"),
                    },
                });
            } else {
                match host.fs_read(&p) {
                    Ok(data) => self.vault_upload_data(host, name, data),
                    Err(e) => self.toast(e, true),
                }
            }
        }
    }

    pub(crate) fn vault_upload_data(&mut self, host: &mut dyn Host, name: String, data: Vec<u8>) {
        let serial = self.vault_add_upload(&name);
        self.vault_job(host, move || encrypt_job(serial, &name, data));
    }

    fn vault_add_upload(&mut self, name: &str) -> u64 {
        self.vault.next_serial += 1;
        let serial = self.vault.next_serial;
        self.vault.uploads.push(Upload {
            serial,
            name: name.to_string(),
            file_id: String::new(),
            data: Vec::new(),
            sent: 0,
            error: None,
        });
        self.vault.rev += 1;
        serial
    }

    fn vault_send_part(&mut self, host: &mut dyn Host, serial: u64) {
        let Some(u) = self.vault.uploads.iter().find(|u| u.serial == serial) else {
            return;
        };
        let end = (u.sent + PART).min(u.data.len());
        let part = u.data[u.sent..end].to_vec();
        let path = format!("/api/vault/files/{}/data?offset={}", u.file_id, u.sent);
        self.vault_send(
            host,
            "PUT",
            &path,
            Some(part),
            "application/octet-stream",
            VaultReq::Data(serial),
        );
    }

    fn vault_upload_failed(&mut self, serial: u64, msg: String) {
        if let Some(u) = self.vault.uploads.iter_mut().find(|u| u.serial == serial) {
            u.error = Some(msg.clone());
            u.data = Vec::new();
        }
        self.toast(format!("{} {msg}", self.tr("Upload failed:")), true);
        self.vault.rev += 1;
    }

    pub(crate) fn vault_share(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        user: &str,
    ) -> Result<(), String> {
        let f = self.vault.file(id).ok_or("no such file")?;
        let (key, header) = (
            f.key.clone().ok_or("locked")?,
            f.header.clone().ok_or("locked")?,
        );
        let user = bl_vault::keys::normalize_user(user).map_err(|e| e.to_string())?;
        let c = self
            .vault
            .contacts
            .iter()
            .find(|c| c.name == user)
            .ok_or_else(|| self.tr("Unknown user").to_string())?;
        if c.status == KeyStatus::Changed {
            return Err(format!(
                "{} {user}: {}",
                self.tr("Key changed:"),
                self.tr("compare the safety number before sharing")
            ));
        }
        let boxed = sealed::seal(
            &c.public.dh,
            &sealed::file_key_aad(&header.file_id, &user),
            key.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let body = Value::object()
            .with("user", user.as_str())
            .with("key", b64(&boxed));
        let path = format!("/api/vault/files/{}/keys", f.id);
        self.vault_request(host, "POST", &path, Some(body), VaultReq::Changed("Shared"));
        Ok(())
    }

    pub(crate) fn vault_remove(&mut self, host: &mut dyn Host, id: &str, user: Option<&str>) {
        let Some(f) = self.vault.file(id) else { return };
        let path = match user {
            Some(u) => format!(
                "/api/vault/files/{}/keys/{}",
                f.id,
                bl_net::http::percent_encode(u)
            ),
            None => format!("/api/vault/files/{}", f.id),
        };
        let what = if user.is_some() {
            "Access removed"
        } else {
            "Deleted"
        };
        self.vault_request(host, "DELETE", &path, None, VaultReq::Changed(what));
    }

    // ---- results ---------------------------------------------------------

    pub(crate) fn poll_vault(&mut self, host: &mut dyn Host) {
        for done in self.vault.jobs.take() {
            match done {
                Done::Keys { user, keys } => {
                    let body = Value::object()
                        .with("user", user.as_str())
                        .with("auth", b64(keys.auth.as_bytes()));
                    self.vault.pending_keys = Some(keys);
                    self.vault.user = user;
                    self.vault_request(
                        host,
                        "POST",
                        "/api/vault/login",
                        Some(body),
                        VaultReq::Login,
                    );
                }
                Done::Registered {
                    invite,
                    account,
                    reg,
                    code,
                } => {
                    let body = Value::object()
                        .with("invite", invite)
                        .with("user", account.user.as_str())
                        .with("auth", b64(reg.auth.as_bytes()))
                        .with("recoveryAuth", b64(reg.recovery_auth.as_bytes()))
                        .with("bundle", bundle_json(&reg.bundle));
                    self.vault.bundle = Some(reg.bundle);
                    self.vault.user = account.user.clone();
                    self.vault.pending_account = Some(account);
                    self.vault.pending_code = Some(code);
                    self.vault_request(
                        host,
                        "POST",
                        "/api/vault/register",
                        Some(body),
                        VaultReq::Register,
                    );
                }
                Done::Change {
                    account,
                    proof,
                    password,
                    recovery,
                } => {
                    let mut body = Value::object().with("proof", proof);
                    if let (Some(p), Some(b)) = (&password, self.vault.bundle.as_mut()) {
                        body.set("password", password_json(p));
                        b.salt = p.salt;
                        b.params = p.params;
                        b.master_by_password = p.master_by_password.clone();
                    }
                    if let Some((r, code)) = recovery {
                        body.set("recovery", recovery_json(&r));
                        if let Some(b) = self.vault.bundle.as_mut() {
                            b.master_by_recovery = r.master_by_recovery.clone();
                        }
                        self.vault.pending_code = Some(code);
                    }
                    self.vault.pending_account = Some(account);
                    self.vault_request(
                        host,
                        "PUT",
                        "/api/vault/account/keys",
                        Some(body),
                        VaultReq::Keys,
                    );
                }
                Done::Encrypted {
                    serial,
                    header,
                    data,
                    meta,
                    key,
                } => {
                    let Some(account) = self.vault.account.as_ref() else {
                        self.vault_upload_failed(serial, self.tr("The account is locked").into());
                        continue;
                    };
                    let sig = file::sign(&account.identity, &header, &meta, &key);
                    let mine = sealed::seal(
                        &account.identity.public().dh,
                        &sealed::file_key_aad(&header.file_id, &account.user),
                        key.as_bytes(),
                    );
                    let Ok(mine) = mine else { continue };
                    let body = Value::object()
                        .with("header", b64(&header.to_bytes()))
                        .with("meta", b64(&meta))
                        .with("signature", b64(&sig))
                        .with("key", b64(&mine));
                    if let Some(u) = self.vault.uploads.iter_mut().find(|u| u.serial == serial) {
                        u.file_id = hex(&header.file_id);
                        u.sent = file::HEADER_LEN;
                        u.data = data;
                    }
                    self.vault_request(
                        host,
                        "POST",
                        "/api/vault/files",
                        Some(body),
                        VaultReq::Create(serial),
                    );
                }
                Done::UploadFailed { serial, msg } => self.vault_upload_failed(serial, msg),
                Done::Failed { account, msg } => {
                    if let Some(a) = account {
                        self.vault.account = Some(a);
                    }
                    let msg = if msg == "wrong password" {
                        self.tr("Wrong password").to_string()
                    } else {
                        msg
                    };
                    self.vault_fail(msg);
                }
            }
        }
    }

    pub(crate) fn vault_http(
        &mut self,
        host: &mut dyn Host,
        req: VaultReq,
        token: &str,
        res: Result<HttpResponse, String>,
    ) {
        let status = res.as_ref().map_or(0, |r| r.status);
        let json = res
            .as_ref()
            .ok()
            .filter(|r| (200..300).contains(&r.status))
            .and_then(|r| bl_json::parse_bytes(&r.body).ok())
            .unwrap_or_default();
        let server_error = || match &res {
            Err(e) => e.clone(),
            Ok(r) => bl_json::parse_bytes(&r.body)
                .ok()
                .and_then(|v| v.get("error").as_str().map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {}", r.status)),
        };
        let auth_request = matches!(
            req,
            VaultReq::Prelogin { .. } | VaultReq::Login | VaultReq::Register | VaultReq::Recover
        );
        // Answers to a session that has since ended or been replaced
        // (logout, password change) are dropped.
        let stale = token != self.vault.token;
        if stale
            && matches!(
                req,
                VaultReq::Users
                    | VaultReq::Files
                    | VaultReq::Changed(_)
                    | VaultReq::Download { .. }
            )
        {
            return;
        }
        let replaced = stale || self.vault.pending_account.is_some();
        if status == 401 && replaced && !auth_request && !matches!(req, VaultReq::Keys) {
            return;
        }
        if status == 401 && !auth_request && !matches!(req, VaultReq::Logout) {
            self.vault_lock(host);
            self.vault.error = self.tr("The session ended: unlock again").into();
            return;
        }
        match req {
            VaultReq::Prelogin { user } => {
                let salt: Option<[u8; 16]> =
                    unb64(json.get("salt")).and_then(|s| s.try_into().ok());
                let (Some(salt), Some(params), Some(password)) = (
                    salt,
                    params_from(json.get("params")),
                    self.vault.pending_password.take(),
                ) else {
                    return self.vault_fail(server_error());
                };
                self.vault_job(host, move || {
                    match PasswordKeys::derive(&password, &salt, params) {
                        Ok(keys) => Done::Keys { user, keys },
                        // Weaker parameters than ours: possibly a malicious server.
                        Err(e) => Done::Failed {
                            account: None,
                            msg: e.to_string(),
                        },
                    }
                });
            }
            VaultReq::Login => {
                let keys = self.vault.pending_keys.take();
                match status {
                    200 => {
                        let unlocked = keys.as_ref().zip(bundle_from(json.get("bundle"))).and_then(
                            |(k, b)| {
                                Account::unlock(&self.vault.user, k, &b)
                                    .ok()
                                    .map(|a| (a, b))
                            },
                        );
                        match unlocked {
                            Some((a, b)) => {
                                let token = json.str_or("token", "").to_string();
                                self.vault_logged_in(host, token, a, Some(b));
                            }
                            // The server sent keys that are not ours.
                            None => {
                                self.vault_fail(self.tr("The server sent invalid account keys"))
                            }
                        }
                    }
                    401 => self.vault_fail(self.tr("Wrong user name or password")),
                    429 => self.vault_fail(self.tr("Too many attempts; try again later")),
                    _ => self.vault_fail(server_error()),
                }
            }
            VaultReq::Register => {
                let account = self.vault.pending_account.take();
                let code = self.vault.pending_code.take();
                match (status, account) {
                    (201, Some(a)) => {
                        self.vault.recovery_code = code.map(|c| c.display());
                        let token = json.str_or("token", "").to_string();
                        self.vault_logged_in(host, token, a, None);
                    }
                    (403, _) if server_error().contains("disabled") => {
                        self.vault_fail(self.tr("Registration is disabled on this server"))
                    }
                    (403, _) => {
                        self.vault_fail(self.tr("The invitation is not valid or has expired"))
                    }
                    (409, _) => self.vault_fail(self.tr("The user name is taken")),
                    _ => self.vault_fail(server_error()),
                }
            }
            VaultReq::Recover => {
                let code = self.vault.pending_code.take();
                let password = self.vault.pending_password.take();
                let bundle = bundle_from(json.get("bundle"));
                let user = self.vault.user.clone();
                let account = code
                    .as_ref()
                    .zip(bundle.as_ref())
                    .and_then(|(c, b)| Account::recover(&user, c, b).ok());
                match (status, account, password) {
                    (200, Some(account), Some(password)) => {
                        self.vault.token = json.str_or("token", "").to_string();
                        self.vault.bundle = bundle;
                        let proof = self.vault.pending_proof.take().unwrap_or_default();
                        self.vault_job(host, move || {
                            let password = match account.change_password(&password) {
                                Ok(p) => p,
                                Err(e) => {
                                    return Done::Failed {
                                        account: None,
                                        msg: e.to_string(),
                                    }
                                }
                            };
                            match account.new_recovery_code() {
                                Ok(r) => Done::Change {
                                    account,
                                    proof,
                                    password: Some(password),
                                    recovery: Some(r),
                                },
                                Err(e) => Done::Failed {
                                    account: None,
                                    msg: e.to_string(),
                                },
                            }
                        });
                    }
                    (200, None, _) => {
                        self.vault_fail(self.tr("The server sent invalid account keys"))
                    }
                    (401, ..) => self.vault_fail(self.tr("Wrong user name or recovery code")),
                    (429, ..) => self.vault_fail(self.tr("Too many attempts; try again later")),
                    _ => self.vault_fail(server_error()),
                }
            }
            VaultReq::Keys => {
                let account = self.vault.pending_account.take();
                let code = self.vault.pending_code.take();
                match (status, account) {
                    (200, Some(a)) => {
                        if let Some(c) = code {
                            self.vault.recovery_code = Some(c.display());
                        }
                        let token = json.str_or("token", "").to_string();
                        if token.is_empty() {
                            self.vault.account = Some(a);
                            self.vault.busy = None;
                            self.vault.rev += 1;
                        } else {
                            self.vault_logged_in(host, token, a, None);
                        }
                        self.toast(self.tr("Account keys changed"), false);
                    }
                    (_, a) => {
                        self.vault.account = a;
                        let msg = if status == 403 {
                            self.tr("Wrong password").to_string()
                        } else {
                            server_error()
                        };
                        self.vault_fail(msg);
                    }
                }
            }
            VaultReq::Users => {
                let me = self.vault.user.clone();
                let mut contacts = Vec::new();
                let mut pins_changed = false;
                for u in json.as_array().into_iter().flatten() {
                    let name = u.str_or("name", "").to_string();
                    let Some(public) =
                        unb64(u.get("public")).and_then(|p| PublicIdentity::from_bytes(&p).ok())
                    else {
                        continue;
                    };
                    if name == me {
                        continue;
                    }
                    let key = hex(&public.sign);
                    let status = match self.vault.pins.get(&name) {
                        Some((k, v)) if *k == key => {
                            if *v {
                                KeyStatus::Verified
                            } else {
                                KeyStatus::Pinned
                            }
                        }
                        Some(_) => KeyStatus::Changed,
                        None => {
                            // Trust on first use; the safety number confirms it.
                            self.vault.pins.insert(name.clone(), (key, false));
                            pins_changed = true;
                            KeyStatus::Pinned
                        }
                    };
                    contacts.push(Contact {
                        name,
                        public,
                        status,
                    });
                }
                if pins_changed {
                    self.save_pins(host);
                }
                self.vault.contacts = contacts;
                self.vault.rev += 1;
                self.vault_request(host, "GET", "/api/vault/files", None, VaultReq::Files);
            }
            VaultReq::Files => {
                if status != 200 {
                    return;
                }
                let files: Vec<VaultFile> = json
                    .get("files")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| self.decode_file(e))
                    .collect();
                self.vault.files = files;
                self.vault.used = json.get("used").as_u64().unwrap_or(0);
                self.vault.quota = json.get("quota").as_u64().unwrap_or(0);
                self.vault.rev += 1;
                self.revs.queue += 1;
            }
            VaultReq::Create(serial) => {
                if status == 201 {
                    self.vault_send_part(host, serial);
                } else {
                    let msg = if status == 507 {
                        self.tr("Storage quota exceeded").to_string()
                    } else {
                        server_error()
                    };
                    self.vault_upload_failed(serial, msg);
                }
            }
            VaultReq::Data(serial) => {
                if status != 200 {
                    return self.vault_upload_failed(serial, server_error());
                }
                let received = json.get("received").as_u64().unwrap_or(0) as usize;
                let Some(u) = self.vault.uploads.iter_mut().find(|u| u.serial == serial) else {
                    return;
                };
                u.sent = received;
                if json.bool_or("complete", false) {
                    let name = u.name.clone();
                    self.vault.uploads.retain(|u| u.serial != serial);
                    self.toast(format!("{} {name}", self.tr("Uploaded")), false);
                    self.vault_refresh(host);
                } else {
                    self.vault_send_part(host, serial);
                }
                self.vault.rev += 1;
            }
            VaultReq::Changed(what) => {
                if (200..300).contains(&status) {
                    self.toast(self.tr(what), false);
                } else {
                    self.toast(server_error(), true);
                }
                self.vault_refresh(host);
            }
            VaultReq::Logout => {}
            VaultReq::Download {
                id,
                autoplay,
                generation,
            } => {
                self.download_progress = None;
                if generation != self.play_generation {
                    return;
                }
                let plain = self.vault.file(&id).and_then(|f| {
                    let key = f.key.clone()?;
                    let body = res.as_ref().ok().filter(|r| r.status == 200)?;
                    file::decrypt(&key, &body.body).ok()
                });
                match plain {
                    Some(data) => {
                        self.vault.remember_plain(&id, Arc::new(data));
                        let track_id = format!("{ID_PREFIX}{id}");
                        if let Some(opener) = self.vault_opener(host, &track_id) {
                            self.player.load(host, opener, autoplay);
                            self.request_analysis_for_current();
                        }
                    }
                    None => self.toast(
                        format!("{}: {}", self.tr("Could not load track"), server_error()),
                        true,
                    ),
                }
            }
        }
    }

    // ---- user interface ----------------------------------------------------

    pub(crate) fn vault_json(&self, host: &dyn Host) -> Value {
        let state = if self.vault.busy.is_some() {
            "busy"
        } else if self.vault.account.is_some() {
            "ready"
        } else {
            "locked"
        };
        let uploads: Vec<Value> = self
            .vault
            .uploads
            .iter()
            .map(|u| {
                let total = u.data.len().max(1);
                Value::object()
                    .with("name", u.name.as_str())
                    .with(
                        "progress",
                        if u.data.is_empty() {
                            0.0
                        } else {
                            u.sent as f64 / total as f64
                        },
                    )
                    .with("error", u.error.clone())
            })
            .collect();
        Value::object()
            .with("state", state)
            .with("busy", self.vault.busy.clone())
            .with("error", self.vault.error.as_str())
            .with(
                "user",
                if self.vault.user.is_empty() {
                    self.settings.vault_user.as_str()
                } else {
                    self.vault.user.as_str()
                },
            )
            .with("recoveryCode", self.vault.recovery_code.clone())
            .with("rev", self.vault.rev)
            .with("uploads", Value::Array(uploads))
            .with("used", self.vault.used)
            .with("quota", self.vault.quota)
            .with(
                "fingerprint",
                self.vault
                    .account
                    .as_ref()
                    .map(|a| a.identity.public().fingerprint()),
            )
            .with("web", host.platform() == "web")
    }

    pub(crate) fn vault_files_json(&self) -> Value {
        let me = self.vault.user.as_str();
        Value::Array(
            self.vault
                .files
                .iter()
                .map(|f| {
                    let t = self.vault_track(&f.id).unwrap_or_default();
                    let audio = f.mime.starts_with("audio/")
                        || bl_media::is_supported_extension(
                            &extension(&f.name).unwrap_or_default(),
                        );
                    self.track_json(&t)
                        .with("name", f.name.as_str())
                        .with("owner", f.owner.as_str())
                        .with("mine", f.owner == me)
                        .with("size", f.size)
                        .with("complete", f.complete)
                        .with("audio", audio && f.problem.is_none() && f.complete)
                        .with("problem", f.problem.clone())
                        .with(
                            "recipients",
                            Value::Array(
                                f.recipients
                                    .iter()
                                    .map(|r| Value::from(r.as_str()))
                                    .collect(),
                            ),
                        )
                })
                .collect(),
        )
    }

    pub(crate) fn vault_contacts_json(&self) -> Value {
        let mine = self.vault.account.as_ref().map(|a| a.identity.public());
        Value::Array(
            self.vault
                .contacts
                .iter()
                .map(|c| {
                    let status = match c.status {
                        KeyStatus::Pinned => "pinned",
                        KeyStatus::Verified => "verified",
                        KeyStatus::Changed => "changed",
                    };
                    Value::object()
                        .with("name", c.name.as_str())
                        .with("status", status)
                        .with("fingerprint", c.public.fingerprint())
                        .with(
                            "safetyNumber",
                            mine.as_ref().map(|m| safety_number(m, &c.public)),
                        )
                })
                .collect(),
        )
    }

    /// `vault.*` commands of the UI.
    pub(crate) fn vault_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let s = |k: &str| c.get(k).as_str().unwrap_or("").to_string();
        let ok = Value::object().with("ok", true);
        match name {
            "vault.login" => self.vault_login(host, &s("user"), &s("password")),
            "vault.register" => self.vault_register(host, &s("invite"), &s("user"), &s("password")),
            "vault.recover" => self.vault_recover(host, &s("user"), &s("code"), &s("password")),
            "vault.logout" => self.vault_logout(host),
            "vault.changePassword" => {
                self.vault_change_password(host, &s("current"), &s("password"))
            }
            "vault.newRecoveryCode" => self.vault_new_recovery_code(host, &s("password")),
            "vault.dismissCode" => {
                self.vault.recovery_code = None;
                self.vault.rev += 1;
            }
            "vault.clearError" => {
                self.vault.error.clear();
                self.vault.rev += 1;
            }
            "vault.refresh" => self.vault_refresh(host),
            "vault.files" => return self.vault_files_json(),
            "vault.contacts" => return self.vault_contacts_json(),
            "vault.upload" => {
                let paths = c
                    .get("paths")
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                self.vault_upload_paths(host, paths);
            }
            "vault.expectUpload" => self.vault.expect_upload = true,
            "vault.share" => {
                if let Err(e) = self.vault_share(host, &s("id"), &s("user")) {
                    return Value::object().with("error", e);
                }
            }
            "vault.revoke" => self.vault_remove(host, &s("id"), Some(&s("user"))),
            "vault.leave" => {
                let me = self.vault.user.clone();
                self.vault_remove(host, &s("id"), Some(&me));
            }
            "vault.delete" => self.vault_remove(host, &s("id"), None),
            "vault.verify" => self.vault_trust(host, &s("user"), true),
            "vault.acceptKey" => self.vault_trust(host, &s("user"), false),
            _ => return Value::object().with("error", format!("unknown command {name}")),
        }
        ok
    }
}

/// Encrypts a file for upload (background thread).
fn encrypt_job(serial: u64, name: &str, data: Vec<u8>) -> Done {
    let ext = extension(name);
    let mut meta = Value::object()
        .with("name", name)
        .with("size", data.len() as u64)
        .with("kind", "music");
    let info =
        bl_media::read_info(&mut std::io::Cursor::new(&data[..]), ext.as_deref(), false).ok();
    if let Some(i) = &info {
        let t = &i.tags;
        meta = meta
            .with("title", t.title.clone())
            .with("artist", t.artist.clone())
            .with("album", t.album.clone())
            .with("durationMs", i.duration_ms)
            .with("codec", i.codec.to_string())
            .with("type", format!("audio/{}", i.codec));
    } else if let Some(e) = &ext {
        meta = meta.with("type", format!("application/{e}"));
    }
    let key = match Secret::random() {
        Ok(k) => k,
        Err(e) => {
            return Done::UploadFailed {
                serial,
                msg: e.to_string(),
            }
        }
    };
    match file::encrypt(&key, &data).and_then(|(h, enc)| {
        let m = file::seal_meta(&key, &h, meta.to_json().as_bytes())?;
        Ok((h, enc, m))
    }) {
        Ok((header, data, meta)) => Done::Encrypted {
            serial,
            header,
            data,
            meta,
            key,
        },
        Err(e) => Done::UploadFailed {
            serial,
            msg: e.to_string(),
        },
    }
}

/// Shared decrypted bytes as a cursor source.
struct ArcBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for ArcBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn open_stream(
    url: &str,
    headers: &[(String, String)],
    key: Secret,
    header: Header,
) -> Result<Box<dyn bl_media::Source>, String> {
    let s = bl_net::client::HttpStream::open(url, headers)?;
    let r = file::Reader::new(
        std::io::BufReader::with_capacity(256 * 1024, s),
        key,
        header,
    )
    .map_err(|e| e.to_string())?;
    Ok(Box::new(std::io::BufReader::with_capacity(64 * 1024, r)))
}

#[cfg(target_arch = "wasm32")]
fn open_stream(
    _url: &str,
    _headers: &[(String, String)],
    _key: Secret,
    _header: Header,
) -> Result<Box<dyn bl_media::Source>, String> {
    Err("streaming is not available in the browser".into())
}
