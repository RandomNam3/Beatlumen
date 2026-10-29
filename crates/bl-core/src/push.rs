//! Push notifications through UnifiedPush: the platform registers with the
//! user's distributor (ntfy, NextPush, …) and reports the endpoint; the
//! core registers it with the server for the unlocked account. The server
//! then sends encrypted messages (RFC 8291) when files are shared; the
//! platform decrypts and shows them (also while the app is closed) and
//! hands them to the core while it runs.

use crate::core::Core;
use crate::host::Host;
use crate::vault::VaultReq;
use bl_json::Value;

const STORAGE_KEY: &str = "push.json";

#[derive(Default)]
pub(crate) struct PushState {
    /// off, unsupported, no-distributor, registering, active, failed
    pub status: &'static str,
    pub detail: String,
    instance: String,
    /// Endpoint reported by the platform, waiting to be registered.
    pending: Option<Value>,
}

impl Core {
    fn push_scope(&self) -> String {
        format!("{}|{}", self.server.base, self.vault.user)
    }

    fn push_stored(&self, host: &dyn Host) -> Value {
        host.storage_read(STORAGE_KEY)
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .filter(|v| v.as_object().is_some())
            .unwrap_or_else(Value::object)
    }

    fn push_store(&self, host: &mut dyn Host, entry: Value) {
        let mut all = self.push_stored(host);
        all.set(self.push_scope(), entry);
        host.storage_write(STORAGE_KEY, all.to_json().as_bytes());
    }

    /// After unlocking: asks the server for its VAPID key, then the
    /// platform for an endpoint.
    pub(crate) fn push_start(&mut self, host: &mut dyn Host) {
        if !self.settings.notifications {
            self.push.status = "off";
            return;
        }
        self.vault_get(host, "/api/vault/push", VaultReq::PushInfo);
    }

    pub(crate) fn push_info(&mut self, host: &mut dyn Host, json: &Value) {
        let vapid = json.str_or("vapidKey", "").to_string();
        let stored = self.push_stored(host).get(&self.push_scope()).clone();
        let mut instance = stored.str_or("instance", "").to_string();
        if instance.len() != 32 {
            let mut b = [0u8; 16];
            if bl_vault::random(&mut b).is_err() {
                return;
            }
            instance = b.iter().map(|x| format!("{x:02x}")).collect();
            self.push_store(host, Value::object().with("instance", instance.as_str()));
        }
        // Subscriptions the server forgot (e.g. a removed device) are
        // registered again when the endpoint arrives.
        self.push.instance = instance.clone();
        self.push.status = match host.push_register(&instance, &vapid) {
            1 => "registering",
            -1 => "no-distributor",
            _ => "unsupported",
        };
        if let Some(p) = self.push.pending.take() {
            self.push_endpoint(host, &p);
        }
    }

    /// The platform reports the endpoint of an instance.
    pub(crate) fn push_endpoint(&mut self, host: &mut dyn Host, c: &Value) {
        if self.push.instance.is_empty() || !self.vault_unlocked() {
            self.push.pending = Some(c.clone());
            return;
        }
        if c.str_or("instance", "") != self.push.instance {
            return;
        }
        let name = match host.platform() {
            "android" => "Android",
            "linux" => "Linux",
            "windows" => "Windows",
            p => p,
        };
        let body = Value::object()
            .with("endpoint", c.str_or("endpoint", ""))
            .with("p256dh", c.str_or("p256dh", ""))
            .with("auth", c.str_or("auth", ""))
            .with("name", name);
        self.vault_post(host, "/api/vault/push", body, VaultReq::PushSubscribed);
    }

    pub(crate) fn push_subscribed(
        &mut self,
        host: &mut dyn Host,
        ok: bool,
        json: &Value,
        error: String,
    ) {
        if ok {
            self.push.status = "active";
            self.push.detail.clear();
            let entry = Value::object()
                .with("instance", self.push.instance.as_str())
                .with("id", json.str_or("id", ""));
            self.push_store(host, entry);
        } else {
            self.push.status = "failed";
            self.push.detail = error;
        }
        self.vault.rev += 1;
    }

    /// Turns notifications off: the server forgets the device, the
    /// distributor the registration.
    pub(crate) fn push_stop(&mut self, host: &mut dyn Host) {
        let stored = self.push_stored(host).get(&self.push_scope()).clone();
        if let Some(id) = stored.get("id").as_str().filter(|i| !i.is_empty()) {
            if self.vault_unlocked() {
                self.vault_delete(host, &format!("/api/vault/push/{id}"));
            }
        }
        if let Some(i) = stored.get("instance").as_str() {
            host.push_unregister(i);
        }
        self.push_store(host, Value::object());
        self.push = PushState {
            status: "off",
            ..Default::default()
        };
        self.vault.rev += 1;
    }

    /// A decrypted message from the platform.
    pub(crate) fn push_message(&mut self, host: &mut dyn Host, msg: &Value) {
        if msg.str_or("type", "") == "share" {
            let from = msg.str_or("from", "").to_string();
            self.toast(
                format!("{from} {}", self.tr("shared a file with you")),
                false,
            );
            self.vault_refresh(host);
        }
    }

    pub(crate) fn push_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        match name {
            "push.endpoint" => self.push_endpoint(host, c),
            "push.message" => {
                if let Ok(m) = bl_json::parse(c.str_or("json", "")) {
                    self.push_message(host, &m);
                }
            }
            "push.failed" => {
                if c.str_or("instance", "") == self.push.instance {
                    self.push.status = "failed";
                    self.push.detail = c.str_or("reason", "").to_string();
                    self.vault.rev += 1;
                }
            }
            _ => return Value::object().with("error", format!("unknown command {name}")),
        }
        Value::object().with("ok", true)
    }

    pub(crate) fn push_json(&self) -> Value {
        Value::object()
            .with("enabled", self.settings.notifications)
            .with(
                "status",
                if self.push.status.is_empty() {
                    "off"
                } else {
                    self.push.status
                },
            )
            .with("detail", self.push.detail.as_str())
    }
}
