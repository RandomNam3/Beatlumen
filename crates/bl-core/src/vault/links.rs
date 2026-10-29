//! Public links to encrypted files (`bl_vault::link`): the link's secret
//! stays in the URL fragment; the server gets the wrapped file key.

use super::{b64, params_json, Done, VaultReq};
use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use bl_vault::link::{self, NewLink};

/// Longest link lifetime the apps offer (days, 0 = no expiry).
const MAX_DAYS: u64 = 365;

impl Core {
    /// `vault.link.create`: a link to the file `id`; with `password` the
    /// receiver needs it as well; `days` until it expires (0 = never).
    pub(crate) fn vault_link_create(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        password: &str,
        days: u64,
    ) {
        let Some(f) = self.vault.file(id) else { return };
        let Some(key) = f.key.clone() else {
            return self.toast(self.tr("This file cannot be opened"), true);
        };
        if f.owner != self.vault.user {
            return self.toast(
                self.tr("Only the owner can share a link to this file"),
                true,
            );
        }
        // The server's file id (the apps' ids carry a "vault:" prefix).
        let id = f.id.clone();
        self.vault.link_url = None;
        self.vault.link_busy = true;
        let password = password.to_string();
        let days = days.min(MAX_DAYS);
        self.vault_job(host, move || {
            // Argon2id when there is a password (slow by design).
            match link::create(
                &id,
                &key,
                (!password.is_empty()).then_some(password.as_str()),
            ) {
                Ok(link) => Done::Link {
                    file: id,
                    link,
                    expires_in: days * 24 * 3600,
                },
                Err(e) => Done::Failed {
                    account: None,
                    msg: e.to_string(),
                },
            }
        });
    }

    /// Sends a link made by [`Core::vault_link_create`] to the server.
    pub(super) fn vault_link_upload(
        &mut self,
        host: &mut dyn Host,
        file: String,
        link: NewLink,
        expires_in: u64,
    ) {
        let mut body = Value::object()
            .with("wrapped", b64(&link.wrapped))
            .with("expiresIn", expires_in);
        if let (Some((salt, params)), Some(hash)) = (link.password, link.auth_hash) {
            body = body.with(
                "password",
                Value::object()
                    .with("salt", b64(&salt))
                    .with("params", params_json(params))
                    .with("authHash", b64(&hash)),
            );
        }
        let secret = link::secret_to_text(&link.secret);
        self.vault_request(
            host,
            "POST",
            &format!("/api/vault/files/{file}/links"),
            Some(body),
            VaultReq::LinkCreated { file, secret },
        );
    }

    /// The server's answer to a new link: the full URL for the user.
    pub(super) fn vault_link_created(
        &mut self,
        host: &mut dyn Host,
        status: u16,
        json: &Value,
        file: String,
        secret: String,
    ) {
        self.vault.link_busy = false;
        let id = json.str_or("id", "");
        if status != 201 || id.is_empty() {
            self.toast(self.tr("The link could not be created"), true);
            return;
        }
        let url = format!("{}#{secret}", self.server.url(&format!("/share/{id}")));
        self.vault_links_fetch(host, &format!("{}{file}", super::ID_PREFIX));
        self.vault.link_url = Some(url);
        self.vault.rev += 1;
    }

    /// `vault.links`: (re)loads the links of a file.
    pub(crate) fn vault_links_fetch(&mut self, host: &mut dyn Host, file: &str) {
        let Some(file) = self.vault.file(file).map(|f| f.id.clone()) else {
            return;
        };
        let file = file.as_str();
        if self.vault.links_file != file {
            self.vault.links_file = file.to_string();
            self.vault.links = Vec::new();
            self.vault.link_url = None;
        }
        self.vault_request(
            host,
            "GET",
            &format!("/api/vault/links?file={file}"),
            None,
            VaultReq::Links(file.to_string()),
        );
    }

    pub(super) fn vault_links_loaded(&mut self, json: &Value, file: String) {
        if file == self.vault.links_file {
            self.vault.links = json.as_array().cloned().unwrap_or_default();
            self.vault.rev += 1;
        }
    }

    /// `vault.link.delete`: revokes a link.
    pub(crate) fn vault_link_delete(&mut self, host: &mut dyn Host, link: &str) {
        let file = self.vault.links_file.clone();
        self.vault.links.retain(|l| l.str_or("id", "") != link);
        self.vault.rev += 1;
        self.vault_request(
            host,
            "DELETE",
            &format!("/api/vault/links/{link}"),
            None,
            VaultReq::Links(file),
        );
    }

    pub(super) fn vault_links_json(&self) -> Value {
        Value::object()
            .with("file", self.vault.links_file.as_str())
            .with("busy", self.vault.link_busy)
            .with("url", self.vault.link_url.clone())
            .with("list", Value::Array(self.vault.links.clone()))
    }
}
