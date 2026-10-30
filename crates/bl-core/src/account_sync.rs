//! Settings that follow an account: appearance, styles and effects,
//! playback options and the like are kept on the server with the account,
//! encrypted on the device (AES-GCM with a key derived from the account's
//! master key; the server stores what it cannot read). Each setting carries
//! the time it was changed; when devices disagree, the newest wins.
//! Device-specific settings (audio, receivers, folders, the server
//! connection) stay on the device.

use crate::core::{Core, Req};
use crate::host::Host;
use bl_json::Value;

/// Settings keys (as in the settings file) that follow the account.
pub(crate) const SYNCED: &[&str] = &[
    "language",
    "preset",
    "intensity",
    "adaptive",
    "normalize",
    "crossfade",
    "skipSilence",
    "visualizer",
    "spectrumMinFreq",
    "spectrumMaxFreq",
    "spectrumSmoothing",
    "barWidth",
    "barGap",
    "waveformThickness",
    "coverFallback",
    "backgroundMode",
    "extensionTarget",
    "notifications",
    "ui",
];

/// Seconds after a change before it is sent (several changes at once).
const DELAY: f64 = 2.0;
const PATH: &str = "/api/vault/settings";

#[derive(Default)]
pub(crate) struct SyncState {
    /// The account's document was read since unlocking (or since another
    /// device changed it).
    pub fetched: bool,
    pub fetching: bool,
    pub saving: bool,
    /// Local changes wait to be sent since then.
    pub dirty_since: Option<f64>,
    /// Settings revision last compared.
    seen_rev: Option<u64>,
}

fn now_ms(host: &dyn Host) -> u64 {
    (host.unix_time() * 1000.0) as u64
}

fn aad(user: &str) -> Vec<u8> {
    let mut a = b"beatlumen settings v1\0".to_vec();
    a.extend_from_slice(user.as_bytes());
    a
}

impl Core {
    fn sync_items(&self) -> &bl_json::Map {
        self.settings.sync_items.as_object().expect("an object")
    }

    /// Notes settings changed on this device (with the time of the change).
    fn note_local_changes(&mut self, host: &dyn Host) {
        if self.account_sync.seen_rev == Some(self.revs.settings) {
            return;
        }
        self.account_sync.seen_rev = Some(self.revs.settings);
        let current = self.settings.to_json();
        let mut changed = false;
        for key in SYNCED {
            let value = current.get(key).clone();
            let item = self.sync_items().get(key).cloned();
            match item {
                Some(i) if *i.get("value") == value => {}
                // Known before: a change now. Unknown (first sync with this
                // account): as old as can be, so the account's value wins.
                other => {
                    let t = if other.is_some() { now_ms(host) } else { 0 };
                    self.settings
                        .sync_items
                        .set(*key, Value::object().with("value", value).with("t", t));
                    changed |= t > 0;
                }
            }
        }
        if changed && self.account_sync.dirty_since.is_none() {
            self.account_sync.dirty_since = Some(host.now());
        }
    }

    /// Reads, merges and writes the account's settings (every tick).
    pub(crate) fn poll_account_sync(&mut self, host: &mut dyn Host) {
        let user = self.vault.user.clone();
        let unlocked =
            !self.vault.session_token().is_empty() && self.vault.settings_key().is_some();
        if !unlocked || !matches!(self.server.state, crate::server::ServerState::Connected(_)) {
            self.account_sync.fetched = false;
            return;
        }
        // Another account: its settings, not this one's.
        if self.settings.sync_user != user {
            self.settings.sync_user = user.clone();
            self.settings.sync_items = Value::object();
            self.account_sync = Default::default();
            self.settings_changed();
        }
        self.note_local_changes(host);
        let s = &self.account_sync;
        if !s.fetched && !s.fetching && !s.saving {
            let rid = self.device_request(host, "GET", PATH, Vec::new(), None);
            self.requests.insert(rid, Req::AccountSettingsGet(user));
            self.account_sync.fetching = true;
        } else if s.fetched
            && !s.saving
            && !s.fetching
            && s.dirty_since.is_some_and(|t| host.now() - t >= DELAY)
        {
            self.save_account_settings(host);
        }
    }

    fn save_account_settings(&mut self, host: &mut dyn Host) {
        let Some(key) = self.vault.settings_key() else {
            return;
        };
        let user = self.vault.user.clone();
        // Values never changed here become the account's first ones
        // (newer than those of a device that joins later).
        let keys: Vec<String> = self.sync_items().keys().map(str::to_string).collect();
        for k in keys {
            let item = self.sync_items().get(&k).cloned().unwrap_or_default();
            if item.get("t").as_u64().unwrap_or(0) == 0 {
                self.settings.sync_items.set(k, item.with("t", 1u64));
            }
        }
        let doc = Value::object()
            .with("version", 1u64)
            .with("items", self.settings.sync_items.clone());
        let Ok(sealed) = bl_vault::aead::seal(&key, &aad(&user), doc.to_json().as_bytes()) else {
            return;
        };
        let device = self.settings.device_id.clone();
        let rid = self.device_request(
            host,
            "PUT",
            &format!("{PATH}?device={device}"),
            sealed,
            Some("application/octet-stream"),
        );
        self.requests.insert(rid, Req::AccountSettingsPut(user));
        self.account_sync.saving = true;
        self.account_sync.dirty_since = None;
    }

    pub(crate) fn account_settings_fetched(
        &mut self,
        host: &mut dyn Host,
        user: &str,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        self.account_sync.fetching = false;
        if user != self.vault.user {
            return;
        }
        let remote = match res {
            Ok(r) if r.status == 200 => {
                let Some(key) = self.vault.settings_key() else {
                    return;
                };
                match bl_vault::aead::open(&key, &aad(user), &r.body)
                    .ok()
                    .and_then(|plain| bl_json::parse_bytes(&plain).ok())
                {
                    Some(doc) => doc.get("items").clone(),
                    None => {
                        host.log("account settings could not be opened; keeping this device's");
                        Value::object()
                    }
                }
            }
            Ok(r) if r.status == 404 => Value::object(),
            _ => return,
        };
        self.account_sync.fetched = true;
        self.merge_account_settings(host, &remote);
    }

    /// Takes newer settings from the account and sends the ones that are
    /// newer here.
    fn merge_account_settings(&mut self, host: &mut dyn Host, remote: &Value) {
        let mut send = false;
        for key in SYNCED {
            let r = remote.get(key);
            let local_t = self
                .sync_items()
                .get(key)
                .map_or(0, |i| i.get("t").as_u64().unwrap_or(0));
            let has_local = self.sync_items().contains_key(key);
            if r.is_null() {
                send |= has_local;
                continue;
            }
            let remote_t = r.get("t").as_u64().unwrap_or(0);
            if remote_t > local_t || (!has_local && remote_t == local_t) {
                let value = r.get("value").clone();
                if *key == "ui" && !matches!(value, Value::Object(_)) {
                    continue;
                }
                self.settings.sync_items.set(*key, r.clone());
                if self.settings.to_json().get(key) != &value {
                    self.set_setting(host, key, value);
                }
            } else if local_t > remote_t {
                send = true;
            }
        }
        // What was just taken is not a local change.
        self.account_sync.seen_rev = None;
        self.note_local_changes(host);
        self.settings_changed();
        if send && self.account_sync.dirty_since.is_none() {
            self.account_sync.dirty_since = Some(host.now() - DELAY);
        }
    }

    pub(crate) fn account_settings_saved(
        &mut self,
        host: &mut dyn Host,
        user: &str,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        self.account_sync.saving = false;
        if user != self.vault.user {
            return;
        }
        if !matches!(res, Ok(r) if (200..300).contains(&r.status)) {
            // Try again later.
            self.account_sync.dirty_since = Some(host.now() + 10.0);
        }
    }
}
