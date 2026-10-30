//! What the admin interface sees of the accounts: names, sizes and
//! passkey counts (never keys or file contents), invitations and public
//! links, and the settings it changes while the server runs.

use super::{invites_dir, read_json, Failure, Vault};
use crate::util;
use bl_json::Value;
use std::sync::atomic::Ordering;

impl Vault {
    pub fn quota(&self) -> u64 {
        self.quota.load(Ordering::Relaxed)
    }

    pub fn set_quota(&self, bytes: u64) {
        self.quota.store(bytes, Ordering::Relaxed);
    }

    pub fn registration_open(&self) -> bool {
        self.open_registration.load(Ordering::Relaxed)
    }

    pub fn set_registration(&self, open: bool) {
        self.open_registration.store(open, Ordering::Relaxed);
    }

    /// The accounts: name, creation time, files they own, bytes used,
    /// passkeys, whether the password is turned off.
    pub fn admin_users(&self) -> Value {
        let files = self.files.read().unwrap_or_else(|e| e.into_inner());
        let users = self.users.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<&super::User> = users.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Value::Array(
            list.into_iter()
                .map(|u| {
                    let mine: Vec<_> = files.values().filter(|f| f.owner == u.name).collect();
                    Value::object()
                        .with("name", u.name.as_str())
                        .with("created", u.created)
                        .with("files", mine.len() as u64)
                        .with("bytes", mine.iter().map(|f| f.size).sum::<u64>())
                        .with("passkeys", u.passkeys.len() as u64)
                        .with("passwordOff", u.password_off)
                })
                .collect(),
        )
    }

    pub fn user_count(&self) -> usize {
        self.users.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Deletes an account with everything it owns.
    pub fn admin_delete_user(&self, name: &str) -> Result<(), Failure> {
        if !self
            .users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name)
        {
            return Err((404, "no such user".into()));
        }
        self.remove_account(name)?;
        crate::info!("vault: the admin deleted the account {name}");
        Ok(())
    }

    /// Open invitations: id (digest of the code) and expiry. The codes
    /// themselves are not kept.
    pub fn admin_invites(&self) -> Value {
        let now = util::now_secs();
        let mut list: Vec<(String, u64)> = std::fs::read_dir(&self.invites_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_suffix(".json")?.to_string();
                let expires = read_json(&e.path())?.get("expires").as_u64()?;
                Some((id, expires))
            })
            .filter(|(_, exp)| *exp > now)
            .collect();
        list.sort_by_key(|(_, e)| *e);
        Value::Array(
            list.into_iter()
                .map(|(id, exp)| Value::object().with("id", id).with("expires", exp))
                .collect(),
        )
    }

    pub fn admin_revoke_invite(&self, id: &str) -> Result<(), Failure> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err((400, "invalid invitation".into()));
        }
        std::fs::remove_file(self.invites_dir.join(format!("{id}.json")))
            .map_err(|_| (404, "no such invitation".to_string()))
    }

    /// Creates an invitation and returns its code.
    pub fn admin_create_invite(&self, days: u64) -> Result<String, Failure> {
        let data = self
            .invites_dir
            .parent()
            .and_then(|v| v.parent())
            .ok_or((500, "no data folder".to_string()))?;
        debug_assert_eq!(invites_dir(data), self.invites_dir);
        super::create_invite(data, days).map_err(|e| (500, e))
    }

    /// All public links: id, owner, file, creation, expiry, password.
    pub fn admin_links(&self) -> Value {
        let links = self.links.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<Value> = links.values().map(|l| l.admin_json()).collect();
        list.sort_by_key(|v| std::cmp::Reverse(v.get("created").as_u64().unwrap_or(0)));
        Value::Array(list)
    }

    pub fn link_count(&self) -> usize {
        self.links.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn admin_delete_link(&self, id: &str) -> Result<(), Failure> {
        let mut links = self.links.write().unwrap_or_else(|e| e.into_inner());
        if links.remove(id).is_none() {
            return Err((404, "no such link".into()));
        }
        let _ = std::fs::remove_file(self.link_path(id));
        Ok(())
    }
}
