//! Staying unlocked on this device: the unlocked account (master key, key
//! bundle, session) is kept in the platform's protected store (Android
//! Keystore, Windows DPAPI, the Secret Service on Linux) and opened again at
//! the next start, so the password or passkey is needed once per device.
//! "Lock" forgets it; so does a session the server no longer accepts
//! (expired after 90 days without use, or ended by a password change).

use super::{b64, bundle_from, bundle_json};
use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use bl_vault::keys::Account;
use bl_vault::Secret;

/// How long the app waits for the store before showing the sign-in form.
const RESTORE_WAIT: f64 = 20.0;

impl Core {
    fn secret_key(&self) -> String {
        format!("vault {}", self.server.base.trim_end_matches('/'))
    }

    /// Whether this device can stay unlocked.
    pub(crate) fn vault_can_remember(&self, host: &dyn Host) -> bool {
        host.has_secret_store()
    }

    /// Keeps the unlocked account in the platform's store (if wanted).
    pub(super) fn vault_remember(&mut self, host: &mut dyn Host) {
        if !self.settings.vault_remember || !host.has_secret_store() || self.vault.token.is_empty()
        {
            return;
        }
        let (Some(a), Some(b)) = (self.vault.account.as_ref(), self.vault.bundle.as_ref()) else {
            return;
        };
        let v = Value::object()
            .with("v", 1u64)
            .with("server", self.server.base.trim_end_matches('/'))
            .with("user", a.user.as_str())
            .with("token", self.vault.token.as_str())
            .with("master", b64(a.master().as_bytes()))
            .with("bundle", bundle_json(b))
            .with("passwordOff", self.vault.password_off)
            .to_json();
        // Unchanged since the last time: no write (the keyring may ask).
        let digest = bl_vault::sha256(&[v.as_bytes()]);
        if self.vault.remembered == Some(digest) {
            return;
        }
        self.vault.remembered = Some(digest);
        host.secret_store(&self.secret_key(), v.as_bytes());
    }

    /// Forgets the account kept on this device.
    pub(super) fn vault_forget(&mut self, host: &mut dyn Host) {
        self.vault.remembered = None;
        if host.has_secret_store() {
            host.secret_store(&self.secret_key(), &[]);
        }
    }

    /// Asks the store for a kept account once the server is known.
    pub(super) fn vault_restore(&mut self, host: &mut dyn Host) {
        if let Some(t) = self.vault.restoring {
            if self.time - t > RESTORE_WAIT {
                self.vault.restoring = None;
                self.vault.rev += 1;
            }
            return;
        }
        let key = self.secret_key();
        if !self.settings.vault_remember
            || !host.has_secret_store()
            || self.vault.account.is_some()
            || self.vault.busy.is_some()
            || !self.vault_ready()
            || self.vault.restore_asked == key
        {
            return;
        }
        self.vault.restore_asked = key.clone();
        if host.secret_load(&key) {
            self.vault.restoring = Some(self.time);
            self.vault.rev += 1;
        }
    }

    /// The kept account from the store (`vault.secret`).
    pub(crate) fn vault_secret(&mut self, host: &mut dyn Host, key: &str, value: &str) {
        if key != self.secret_key() || self.vault.restoring.is_none() {
            return;
        }
        self.vault.restoring = None;
        self.vault.rev += 1;
        if self.vault.account.is_some() || self.vault.busy.is_some() {
            return;
        }
        let Some(v) = bl_net::base64_decode(value)
            .filter(|b| !b.is_empty())
            .and_then(|b| bl_json::parse_bytes(&b).ok())
        else {
            return;
        };
        if v.str_or("server", "") != self.server.base.trim_end_matches('/') {
            return;
        }
        let user = v.str_or("user", "").to_string();
        let account = bundle_from(v.get("bundle")).and_then(|bundle| {
            let master = bl_net::base64_decode(v.str_or("master", ""))?;
            let master = Secret::from_bytes(master.try_into().ok()?);
            Account::from_master(&user, master, &bundle)
                .ok()
                .map(|a| (a, bundle))
        });
        let Some((account, bundle)) = account else {
            // Damaged or from another version: sign in again.
            return self.vault_forget(host);
        };
        self.vault.password_off = v.bool_or("passwordOff", false);
        self.vault.remembered = Some(bl_vault::sha256(&[v.to_json().as_bytes()]));
        let token = v.str_or("token", "").to_string();
        self.vault_logged_in(host, token, account, Some(bundle));
    }

    /// The "stay unlocked on this device" switch.
    pub(crate) fn vault_set_remember(&mut self, host: &mut dyn Host, on: bool) {
        if self.settings.vault_remember != on {
            self.settings.vault_remember = on;
            self.settings_changed();
        }
        if on {
            self.vault.remembered = None;
            self.vault_remember(host);
        } else {
            self.vault_forget(host);
        }
        self.vault.rev += 1;
    }
}
