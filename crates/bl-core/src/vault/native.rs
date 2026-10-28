//! Passkeys and saved passwords through the platform's credential manager
//! (Android: Credential Manager with Google Password Manager, Bitwarden,
//! …). The host shows the provider's sheet and hands back the WebAuthn
//! JSON; everything is checked here and on the server as on the sign-in
//! page (`web/auth/auth.js`). Where the platform has none, the sign-in
//! falls back to the browser.

use super::{b64, bundle_from, Done, VaultReq};
use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use bl_vault::keys::{self, Account, PasswordKeys};
use bl_vault::webpush::base64url;

/// The credential request waiting for the platform's sheet.
pub(crate) enum Native {
    SignIn {
        challenge: String,
    },
    AddPasskey {
        name: String,
        proof: Value,
        challenge: String,
        /// The new credential, waiting for its PRF output.
        created: Option<Created>,
    },
}

pub(crate) struct Created {
    cred: Vec<u8>,
    client_data: String,
    attestation: String,
}

#[derive(Debug, Clone)]
pub(crate) enum NativeReq {
    SignInOptions,
    SignIn { cred: Vec<u8>, prf: Vec<u8> },
    CreateOptions,
    PrfOptions,
    Added,
}

fn unb64url(s: &str) -> Option<Vec<u8>> {
    bl_net::base64_decode(s).filter(|b| !b.is_empty())
}

/// The PRF output of a credential response (`clientExtensionResults`).
fn prf_of(r: &Value) -> Option<Vec<u8>> {
    let first = r
        .get("clientExtensionResults")
        .get("prf")
        .get("results")
        .get("first");
    first.as_str().and_then(unb64url)
}

impl Core {
    /// Signs in with a passkey or a saved password from the platform's
    /// credential manager; the browser where there is none.
    pub(crate) fn vault_native_sign_in(&mut self, host: &mut dyn Host) {
        if !self.vault_ready() {
            return self.vault_fail(self.tr("Not connected to a server"));
        }
        if !self.platform_credentials(host) {
            return self.vault_browser_sign_in(host);
        }
        self.vault_busy("Waiting for the password manager…");
        if self.server_passkeys() {
            let body = Value::object().with("purpose", "login");
            self.vault_post(
                host,
                "/api/vault/passkeys/options",
                body,
                VaultReq::Native(NativeReq::SignInOptions),
            );
        } else {
            self.native_get(host, String::new(), String::new(), true);
        }
    }

    pub(crate) fn platform_credentials(&self, host: &dyn Host) -> bool {
        host.platform() == "android"
    }

    fn server_passkeys(&self) -> bool {
        matches!(&self.server.state, crate::server::ServerState::Connected(i) | crate::server::ServerState::NeedsLogin(i) if i.passkeys)
    }

    fn native_get(
        &mut self,
        host: &mut dyn Host,
        challenge: String,
        public_key: String,
        password: bool,
    ) {
        let req = Value::object()
            .with("publicKey", public_key)
            .with("password", password);
        if !host.credential_request("get", &req.to_json()) {
            self.vault.native = None;
            return self.vault_browser_sign_in(host);
        }
        if !matches!(self.vault.native, Some(Native::AddPasskey { .. })) {
            self.vault.native = Some(Native::SignIn { challenge });
        }
    }

    /// Creates a passkey for this account in the platform's credential
    /// manager. `password` proves the account (empty: the session does,
    /// right after a passkey sign-in).
    pub(crate) fn vault_add_passkey(&mut self, host: &mut dyn Host, password: &str, name: &str) {
        let (Some(user), Some(bundle)) = (
            self.vault.account.as_ref().map(|a| a.user.clone()),
            self.vault.bundle.clone(),
        ) else {
            return self.vault_fail(self.tr("The account is locked"));
        };
        if !self.platform_credentials(host) {
            return self.vault_manage_in_browser(host);
        }
        self.vault_busy("Adding a passkey…");
        let name = if name.trim().is_empty() {
            self.tr("Passkey on this device").to_string()
        } else {
            name.trim().to_string()
        };
        if password.is_empty() {
            return self.native_create_options(host, name, Value::object().with("session", true));
        }
        let password = password.to_string();
        self.vault_job(host, move || {
            let fail = |msg: String| Done::Failed { account: None, msg };
            let keys = match PasswordKeys::derive(&password, &bundle.salt, bundle.params) {
                Ok(k) => k,
                Err(e) => return fail(e.to_string()),
            };
            if Account::unlock(&user, &keys, &bundle).is_err() {
                return fail("wrong password".into());
            }
            Done::Proof {
                name,
                proof: Value::object().with("auth", b64(keys.auth.as_bytes())),
            }
        });
    }

    pub(super) fn native_create_options(
        &mut self,
        host: &mut dyn Host,
        name: String,
        proof: Value,
    ) {
        self.vault.native = Some(Native::AddPasskey {
            name,
            proof,
            challenge: String::new(),
            created: None,
        });
        let body = Value::object().with("purpose", "register");
        self.vault_post(
            host,
            "/api/vault/passkeys/options",
            body,
            VaultReq::Native(NativeReq::CreateOptions),
        );
    }

    /// A result from the platform's sheet (`vault.credentialResult`).
    pub(crate) fn vault_credential_result(
        &mut self,
        host: &mut dyn Host,
        kind: &str,
        ok: bool,
        data: &str,
    ) {
        let Some(op) = self.vault.native.take() else {
            return;
        };
        let v = bl_json::parse(data).unwrap_or_default();
        if !ok {
            let t = v.str_or("type", "");
            if t.contains("CANCEL") || t.contains("INTERRUPTED") {
                self.vault.busy = None;
                self.vault.rev += 1;
            } else if t == "unsupported" && matches!(op, Native::SignIn { .. }) {
                self.vault.busy = None;
                self.vault_browser_sign_in(host);
            } else if t.contains("NO_CREDENTIAL") {
                self.vault_fail(self.tr("No passkey or saved password for this server was found. Sign in with your password once, or use the browser."));
            } else if t == "unsupported" {
                self.vault_fail(self.tr("Android 14 or newer is needed"));
            } else {
                let msg = v.str_or("error", "");
                self.vault_fail(if msg.is_empty() {
                    t.to_string()
                } else {
                    msg.to_string()
                });
            }
            return;
        }
        let r = v.get("response");
        match (kind, op) {
            ("get", Native::SignIn { challenge }) => {
                if v.str_or("type", "").contains("PASSWORD") {
                    let (user, password) = (v.str_or("user", ""), v.str_or("password", ""));
                    self.vault_login(host, user, password);
                    // Came from the manager: nothing to save.
                    self.vault.save_password = None;
                    return;
                }
                let Some(cred) = unb64url(r.str_or("rawId", r.str_or("id", ""))) else {
                    return self.vault_fail(self.tr("The password manager sent an invalid answer"));
                };
                let Some(prf) = prf_of(r) else {
                    return self.vault_fail(self.tr("This passkey cannot unlock encrypted files (its provider does not support the PRF extension). Use another passkey or your password."));
                };
                let a = r.get("response");
                let body = Value::object()
                    .with("challengeId", challenge)
                    .with("credentialId", base64url(&cred))
                    .with("clientDataJSON", a.str_or("clientDataJSON", ""))
                    .with("authenticatorData", a.str_or("authenticatorData", ""))
                    .with("signature", a.str_or("signature", ""))
                    .with("userHandle", a.str_or("userHandle", ""));
                self.vault_busy("Unlocking…");
                self.vault_post(
                    host,
                    "/api/vault/passkeys/login",
                    body,
                    VaultReq::Native(NativeReq::SignIn { cred, prf }),
                );
            }
            ("create", Native::AddPasskey { name, proof, challenge, .. }) => {
                let a = r.get("response");
                let Some(cred) = unb64url(r.str_or("rawId", r.str_or("id", ""))) else {
                    return self.vault_fail(self.tr("The password manager sent an invalid answer"));
                };
                let created = Created {
                    cred,
                    client_data: a.str_or("clientDataJSON", "").to_string(),
                    attestation: a.str_or("attestationObject", "").to_string(),
                };
                match prf_of(r) {
                    Some(prf) => self.native_register(host, name, proof, challenge, created, &prf),
                    None => {
                        // Most providers give the PRF output only when
                        // signing in: ask once.
                        self.vault.native = Some(Native::AddPasskey {
                            name,
                            proof,
                            challenge,
                            created: Some(created),
                        });
                        let body = Value::object().with("purpose", "login");
                        self.vault_post(
                            host,
                            "/api/vault/passkeys/options",
                            body,
                            VaultReq::Native(NativeReq::PrfOptions),
                        );
                    }
                }
            }
            ("get", Native::AddPasskey { name, proof, challenge, created: Some(created) }) => {
                match prf_of(r) {
                    Some(prf) => self.native_register(host, name, proof, challenge, created, &prf),
                    None => self.vault_fail(self.tr("This passkey cannot unlock encrypted files (its provider does not support the PRF extension). Use another passkey or your password.")),
                }
            }
            (_, op) => self.vault.native = Some(op),
        }
    }

    fn native_register(
        &mut self,
        host: &mut dyn Host,
        name: String,
        proof: Value,
        challenge: String,
        created: Created,
        prf: &[u8],
    ) {
        let Some(account) = self.vault.account.as_ref() else {
            return self.vault_fail(self.tr("The account is locked"));
        };
        let wrapped = match keys::wrap_master_for_passkey(
            &account.user,
            account.master(),
            &created.cred,
            prf,
        ) {
            Ok(w) => w,
            Err(e) => return self.vault_fail(e.to_string()),
        };
        let body = Value::object()
            .with("challengeId", challenge)
            .with("clientDataJSON", created.client_data)
            .with("attestationObject", created.attestation)
            .with("name", name)
            .with("masterByPasskey", b64(&wrapped))
            .with("proof", proof);
        self.vault_post(
            host,
            "/api/vault/passkeys",
            body,
            VaultReq::Native(NativeReq::Added),
        );
    }

    pub(super) fn native_http(
        &mut self,
        host: &mut dyn Host,
        req: NativeReq,
        status: u16,
        json: &Value,
        server_error: String,
    ) {
        match req {
            NativeReq::SignInOptions => {
                if status == 200 {
                    let challenge = json.str_or("challengeId", "").to_string();
                    let pk = json.get("publicKey").to_json();
                    self.native_get(host, challenge, pk, true);
                } else {
                    // Passkeys not set up: saved passwords still work.
                    self.native_get(host, String::new(), String::new(), true);
                }
            }
            NativeReq::SignIn { cred, prf } => match status {
                200 => {
                    let user = json.str_or("user", "").to_string();
                    let account = bundle_from(json.get("bundle")).and_then(|bundle| {
                        let wrapped = bl_net::base64_decode(json.str_or("masterByPasskey", ""))?;
                        let master =
                            keys::unwrap_master_with_passkey(&user, &cred, &prf, &wrapped).ok()?;
                        Account::from_master(&user, master, &bundle)
                            .ok()
                            .map(|a| (a, bundle))
                    });
                    match account {
                        Some((a, bundle)) => {
                            self.vault.password_off =
                                json.get("bundle").bool_or("passwordOff", false);
                            let token = json.str_or("token", "").to_string();
                            self.vault_logged_in(host, token, a, Some(bundle));
                        }
                        None => self.vault_fail(self.tr("The server sent invalid account keys")),
                    }
                }
                401 if server_error.contains("unknown") => {
                    self.vault_fail(self.tr("This passkey is not registered on this server"))
                }
                401 => self.vault_fail(self.tr("The passkey sign-in could not be verified")),
                429 => self.vault_fail(self.tr("Too many attempts; try again later")),
                _ => self.vault_fail(server_error),
            },
            NativeReq::CreateOptions => {
                let Some(Native::AddPasskey { challenge, .. }) = self.vault.native.as_mut() else {
                    return;
                };
                if status != 200 {
                    self.vault.native = None;
                    return self.vault_fail(server_error);
                }
                *challenge = json.str_or("challengeId", "").to_string();
                let req = Value::object()
                    .with("publicKey", json.get("publicKey").to_json())
                    .with("user", self.vault.user.as_str());
                if !host.credential_request("create", &req.to_json()) {
                    self.vault.native = None;
                    self.vault_fail(self.tr("Android 14 or newer is needed"));
                }
            }
            NativeReq::PrfOptions => {
                let Some(Native::AddPasskey {
                    created: Some(c), ..
                }) = self.vault.native.as_ref()
                else {
                    return;
                };
                if status != 200 {
                    self.vault.native = None;
                    return self.vault_fail(server_error);
                }
                // Only the PRF output is used; the assertion is not sent.
                let mut pk = json.get("publicKey").clone();
                pk.set(
                    "allowCredentials",
                    Value::Array(vec![Value::object()
                        .with("type", "public-key")
                        .with("id", base64url(&c.cred))]),
                );
                let req = Value::object()
                    .with("publicKey", pk.to_json())
                    .with("password", false);
                if !host.credential_request("get", &req.to_json()) {
                    self.vault.native = None;
                    self.vault_fail(self.tr("Android 14 or newer is needed"));
                }
            }
            NativeReq::Added => match status {
                201 => {
                    self.vault.busy = None;
                    self.vault.rev += 1;
                    self.toast(self.tr("Passkey added"), false);
                }
                403 if server_error.contains("again") => {
                    self.vault_fail(self.tr("Lock and sign in with your passkey again to confirm"))
                }
                403 => self.vault_fail(self.tr("Wrong password")),
                409 => self.vault_fail(self.tr("This passkey is registered already")),
                _ => self.vault_fail(server_error),
            },
        }
    }

    /// Offers the password just used to the platform's password manager.
    pub(super) fn offer_password(&mut self, host: &mut dyn Host) {
        if let Some((user, password)) = self.vault.save_password.take() {
            let v = Value::object()
                .with("user", user)
                .with("password", password);
            host.credential_request("save", &v.to_json());
        }
    }
}
