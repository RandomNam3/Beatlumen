//! Devices of the same server login: this app announces itself (name,
//! platform, what it plays), listens on the server's event channel (one
//! long-polling request) and runs the commands other devices send: remote
//! control, taking over playback ("transfer") and starting together at a
//! common time (sync groups). The server only relays; nothing listens on
//! this device.
//!
//! Remote commands are limited to playback: songs to play must be server
//! songs (never paths of this device), and the user can turn remote
//! control off (`remoteControl`).

use crate::core::{Core, Req};
use crate::host::{Host, HttpRequest};
use crate::player::PlayState;
use crate::server::ServerState;
use bl_json::Value;

/// Seconds between announcements while nothing changes.
const HEARTBEAT: f64 = 25.0;
/// Seconds before listening again after an error.
const RETRY: f64 = 5.0;
/// Head start for playing together (milliseconds).
const GROUP_LEAD_MS: f64 = 2500.0;
/// Queue entries a device reports around the current one.
const QUEUE_REPORT: usize = 50;

#[derive(Default)]
pub(crate) struct DevicesState {
    /// The devices of the login (from `GET /api/devices`).
    pub list: Vec<Value>,
    pub listening: bool,
    pub since: Option<u64>,
    pub retry_at: f64,
    /// What was last announced (without the position) and when.
    pub announced: String,
    pub announced_at: f64,
    pub announcing: bool,
    pub refreshing: bool,
    /// Server clock minus this device's clock (milliseconds).
    pub clock_offset_ms: Option<f64>,
    /// Start of a group playback: (server time ms, position s).
    pub play_at: Option<(f64, f64)>,
    /// The position announced last and the local time it was taken.
    pub reported_position: (f64, f64),
    /// The login the device requests use (an unlocked account's session
    /// keeps its devices among themselves); a change starts over.
    pub login: String,
    /// Answers to listening requests of an earlier login are ignored.
    pub generation: u64,
}

/// A server song id (16 hex digits): the only songs remote commands play.
fn server_song(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn ok() -> Value {
    Value::object().with("ok", true)
}

fn err(msg: &str) -> Value {
    Value::object().with("error", msg)
}

impl Core {
    fn connected(&self) -> bool {
        matches!(self.server.state, ServerState::Connected(_))
    }

    /// The login of device requests: the unlocked account, else the server
    /// login.
    fn device_login(&self) -> String {
        match self.vault.session_token() {
            "" => self.server.token.clone(),
            t => t.to_string(),
        }
    }

    /// A request to the server as this device's login.
    pub(crate) fn device_request(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> u64 {
        let login = self.device_login();
        self.device_request_as(host, &login, method, path, body, content_type)
    }

    fn device_request_as(
        &mut self,
        host: &mut dyn Host,
        login: &str,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> u64 {
        let mut headers = Vec::new();
        if !login.is_empty() {
            headers.push(("Authorization".to_string(), format!("Bearer {login}")));
        }
        if let Some(t) = content_type {
            headers.push(("Content-Type".to_string(), t.to_string()));
        }
        host.http_request(HttpRequest {
            method: method.into(),
            url: self.server.url(path),
            headers,
            body,
        })
    }

    /// This device's id (made on first use).
    fn device_id(&mut self) -> String {
        if self.settings.device_id.len() != 32 {
            self.settings.device_id = (0..16)
                .map(|_| format!("{:02x}", self.rng.next_u64() & 0xff))
                .collect();
            self.settings_changed();
        }
        self.settings.device_id.clone()
    }

    pub(crate) fn device_name(&self, host: &dyn Host) -> String {
        let n = self.settings.device_name.trim();
        if !n.is_empty() {
            return n.to_string();
        }
        let p = match host.platform() {
            "android" => "Android",
            "windows" => "Windows",
            "linux" => "Linux",
            "macos" => "Mac",
            "web" => "Web",
            other => other,
        };
        format!("Beatlumen ({p})")
    }

    fn device_kind(host: &dyn Host) -> &'static str {
        match host.platform() {
            "android" | "ios" => "phone",
            "web" => "browser",
            _ => "computer",
        }
    }

    /// The server's clock now (milliseconds), from this device's clock.
    fn server_now_ms(&self, host: &dyn Host) -> f64 {
        host.unix_time() * 1000.0 + self.devices.clock_offset_ms.unwrap_or(0.0)
    }

    /// What other devices see of this one's playback.
    fn device_playback(&self, host: &dyn Host) -> (Value, String) {
        let play = match self.player.state() {
            PlayState::Stopped => "stopped",
            PlayState::Loading => "loading",
            PlayState::Playing => "playing",
            PlayState::Paused => "paused",
        };
        let track = self.current.as_ref().map(|t| {
            Value::object()
                .with("id", t.id.as_str())
                .with("title", t.title.as_str())
                .with("artist", t.artists.join(", "))
                .with("album", t.album.as_str())
                .with("duration", t.duration_ms as f64 / 1000.0)
                .with("server", server_song(&t.id))
        });
        let cur = self.queue.position().unwrap_or(0);
        let first = cur.saturating_sub(QUEUE_REPORT / 5);
        let queue: Vec<Value> = self
            .queue
            .ordered()
            .enumerate()
            .skip(first)
            .take(QUEUE_REPORT)
            .map(|(pos, (_, id))| {
                let t = self.track_for(id);
                Value::object()
                    .with("pos", pos)
                    .with("id", if server_song(id) { id.as_str() } else { "" })
                    .with("title", t.title.as_str())
                    .with("artist", t.artists.join(", "))
            })
            .collect();
        let state = Value::object()
            .with("play", play)
            .with("track", track.unwrap_or(Value::Null))
            .with("volume", self.settings.volume)
            .with("muted", self.settings.muted)
            .with("shuffle", self.queue.shuffle)
            .with("repeat", self.settings.repeat.name())
            .with("queueIndex", self.queue.position())
            .with("queueLength", self.queue.len())
            .with("queue", Value::Array(queue));
        // The position is left out of the change check (it moves by itself).
        let signature = state.to_json();
        let position = self.player.position(host);
        let state = state
            .with("position", position)
            .with("positionAt", self.server_now_ms(host));
        (state, signature)
    }

    /// Announcing, listening and group starts (every tick).
    pub(crate) fn poll_devices(&mut self, host: &mut dyn Host) {
        if !self.connected() {
            if self.devices.listening || !self.devices.list.is_empty() {
                self.devices = Default::default();
                self.revs.devices += 1;
            }
            return;
        }
        let now = host.now();
        // Another login (an account unlocked or locked): start over.
        let login = self.device_login();
        if login != self.devices.login {
            // Leave the devices of the previous login.
            if self.devices.announced_at > 0.0 {
                let old = self.devices.login.clone();
                let id = self.device_id();
                self.device_request_as(
                    host,
                    &old,
                    "DELETE",
                    &format!("/api/devices/{id}"),
                    Vec::new(),
                    None,
                );
            }
            let generation = self.devices.generation + 1;
            self.devices = DevicesState {
                login,
                generation,
                ..Default::default()
            };
            self.revs.devices += 1;
        }
        // A group start that is due.
        if let Some((at, pos)) = self.devices.play_at {
            let late = self.server_now_ms(host) - at;
            if late >= 0.0 {
                self.devices.play_at = None;
                self.player.seek(host, pos + late / 1000.0);
                self.player.play(host);
            }
        }
        let id = self.device_id();
        if !self.devices.announcing {
            let (state, signature) = self.device_playback(host);
            let position = state.get("position").as_f64().unwrap_or(0.0);
            // A seek also counts as a change.
            let (p0, t0) = self.devices.reported_position;
            let expected = if self.player.state() == PlayState::Playing {
                p0 + (now - t0)
            } else {
                p0
            };
            let jumped = (position - expected).abs() > 2.0;
            let signature = format!(
                "{}|{}|{}|{signature}",
                self.device_name(host),
                self.settings.remote_control,
                host.platform()
            );
            if signature != self.devices.announced
                || jumped
                || now - self.devices.announced_at >= HEARTBEAT
            {
                let body = Value::object()
                    .with("name", self.device_name(host))
                    .with("platform", host.platform())
                    .with("kind", Self::device_kind(host))
                    .with("remote", self.settings.remote_control)
                    .with("state", state);
                let rid = self.device_request(
                    host,
                    "PUT",
                    &format!("/api/devices/{id}"),
                    body.to_json().into_bytes(),
                    Some("application/json"),
                );
                self.requests
                    .insert(rid, Req::DeviceAnnounce(self.devices.generation));
                self.devices.announcing = true;
                self.devices.announced = signature;
                self.devices.announced_at = now;
                self.devices.reported_position = (position, now);
            }
        }
        if !self.devices.listening && now >= self.devices.retry_at {
            let path = match self.devices.since {
                Some(s) => format!("/api/events?device={id}&since={s}"),
                None => format!("/api/events?device={id}"),
            };
            let rid = self.device_request(host, "GET", &path, Vec::new(), None);
            self.requests
                .insert(rid, Req::DeviceEvents(self.devices.generation));
            self.devices.listening = true;
            if self.devices.since.is_none() {
                self.refresh_devices(host);
            }
        }
    }

    fn refresh_devices(&mut self, host: &mut dyn Host) {
        if self.devices.refreshing || !self.connected() {
            return;
        }
        let rid = self.device_request(host, "GET", "/api/devices", Vec::new(), None);
        self.requests
            .insert(rid, Req::DeviceList(self.devices.generation));
        self.devices.refreshing = true;
    }

    fn take_clock(&mut self, host: &dyn Host, v: &Value) {
        if let Some(server) = v.get("now").as_f64() {
            let offset = server - host.unix_time() * 1000.0;
            // Smoothed; the first answer sets it.
            self.devices.clock_offset_ms = Some(match self.devices.clock_offset_ms {
                Some(o) => o * 0.8 + offset * 0.2,
                None => offset,
            });
        }
    }

    /// Answers to the device requests.
    pub(crate) fn devices_response(
        &mut self,
        host: &mut dyn Host,
        req: Req,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        let json = match res {
            Ok(r) if (200..300).contains(&r.status) => bl_json::parse_bytes(&r.body).ok(),
            _ => None,
        };
        // Answers for an earlier login.
        if let Req::DeviceAnnounce(g) | Req::DeviceList(g) | Req::DeviceEvents(g) = req {
            if g != self.devices.generation {
                return;
            }
        }
        match req {
            Req::DeviceAnnounce(_) => {
                self.devices.announcing = false;
                if let Some(v) = &json {
                    self.take_clock(host, v);
                }
            }
            Req::DeviceList(_) => {
                self.devices.refreshing = false;
                if let Some(Value::Array(list)) = json {
                    self.devices.list = list;
                    self.revs.devices += 1;
                }
            }
            Req::DeviceEvents(_) => {
                self.devices.listening = false;
                let Some(v) = json else {
                    self.devices.retry_at = host.now() + RETRY;
                    return;
                };
                self.take_clock(host, &v);
                let first = self.devices.since.is_none();
                self.devices.since = v.get("seq").as_u64();
                let mut refresh = first || v.bool_or("missed", false);
                for e in v.get("events").as_array().into_iter().flatten() {
                    match e.str_or("type", "") {
                        "devices" => refresh = true,
                        "command" => self.run_remote_command(host, e),
                        "settings" if e.str_or("from", "") != self.settings.device_id => {
                            self.account_sync.fetched = false;
                        }
                        _ => {}
                    }
                }
                if refresh {
                    self.refresh_devices(host);
                }
            }
            Req::DeviceCommand => {
                if !matches!(res, Ok(r) if (200..300).contains(&r.status)) {
                    let msg = match res {
                        Ok(r) if r.status == 403 => {
                            self.tr("This device does not allow remote control")
                        }
                        Ok(r) if r.status == 409 => self.tr("The device is offline"),
                        _ => self.tr("The command did not reach the device"),
                    };
                    self.toast(msg, true);
                }
            }
            _ => {}
        }
    }

    /// Runs a command another device sent.
    fn run_remote_command(&mut self, host: &mut dyn Host, e: &Value) {
        if !self.settings.remote_control {
            return;
        }
        let c = e.get("command");
        let from = e.str_or("fromName", "").to_string();
        let server_ids = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
                .filter(|id| server_song(id))
                .take(5000)
                .map(str::to_string)
                .collect()
        };
        let name = c.str_or("cmd", "");
        match name {
            "play" | "pause" | "toggle" | "stop" | "next" | "prev" | "seek" | "seekBy"
            | "volume" | "mute" | "shuffle" | "repeat" | "queue.play" | "queue.remove"
            | "queue.move" | "queue.clear" | "enqueue" => {
                let mut cmd = c.clone();
                if matches!(name, "play" | "enqueue") {
                    cmd.set(
                        "ids",
                        Value::Array(
                            server_ids(c.get("ids"))
                                .into_iter()
                                .map(Value::from)
                                .collect(),
                        ),
                    );
                    if name == "enqueue" && cmd.get("ids").as_array().is_some_and(|a| a.is_empty())
                    {
                        return;
                    }
                }
                self.command(host, &cmd);
            }
            // Another device hands over its queue: play it from its position.
            "take" => {
                let ids = server_ids(c.get("ids"));
                if ids.is_empty() {
                    return;
                }
                let index = c.get("index").as_u64().unwrap_or(0) as usize;
                self.play_tracks(host, ids, index);
                self.player.seek(host, c.f64_or("position", 0.0).max(0.0));
                if !c.bool_or("playing", true) {
                    self.player.pause(host);
                }
                if !from.is_empty() {
                    let msg = format!("{} {from}", self.tr("Playback taken over from"));
                    self.toast(msg, false);
                }
            }
            // Hand this device's playback to another one.
            "transferTo" => {
                let target = c.str_or("target", "").to_string();
                self.devices_transfer(host, &target);
            }
            // Start together with other devices at a server time.
            "playAt" => {
                let ids = server_ids(c.get("ids"));
                let at = c.f64_or("at", 0.0);
                if ids.is_empty() || at <= 0.0 {
                    return;
                }
                let index = c.get("index").as_u64().unwrap_or(0) as usize;
                self.play_tracks(host, ids, index);
                self.player.pause(host);
                self.devices.play_at = Some((at, c.f64_or("position", 0.0).max(0.0)));
            }
            _ => {}
        }
    }

    /// The server songs of the queue, the current index and position.
    fn queue_for_transfer(&self, host: &dyn Host) -> Option<(Vec<String>, usize, f64)> {
        let cur = self.queue.position()?;
        let mut ids = Vec::new();
        let mut index = 0;
        for (pos, (_, id)) in self.queue.ordered().enumerate() {
            if !server_song(id) {
                continue;
            }
            if pos == cur {
                index = ids.len();
            }
            ids.push(id.clone());
        }
        let current_on_server = self.queue.current().is_some_and(|id| server_song(id));
        if ids.is_empty() || !current_on_server {
            return None;
        }
        Some((ids, index, self.player.position(host)))
    }

    fn send_device_command(&mut self, host: &mut dyn Host, target: &str, command: Value) -> Value {
        if !self.connected() {
            return err(&self.tr("Not connected to the server"));
        }
        if target.is_empty() || target.len() > 64 {
            return err("invalid device");
        }
        let from = self.device_id();
        let body = Value::object().with("from", from).with("command", command);
        let rid = self.device_request(
            host,
            "POST",
            &format!("/api/devices/{target}/command"),
            body.to_json().into_bytes(),
            Some("application/json"),
        );
        self.requests.insert(rid, Req::DeviceCommand);
        ok()
    }

    /// Moves this device's playback to `target`.
    pub(crate) fn devices_transfer(&mut self, host: &mut dyn Host, target: &str) -> Value {
        let Some((ids, index, position)) = self.queue_for_transfer(host) else {
            let msg = self
                .tr("Only songs from the server can move to another device")
                .to_string();
            self.toast(msg.clone(), true);
            return err(&msg);
        };
        let playing = self.player.state() == PlayState::Playing;
        let cmd = Value::object()
            .with("cmd", "take")
            .with(
                "ids",
                Value::Array(ids.into_iter().map(Value::from).collect()),
            )
            .with("index", index as u64)
            .with("position", position)
            .with("playing", playing);
        let r = self.send_device_command(host, target, cmd);
        if r.get("ok").as_bool() == Some(true) {
            self.player.pause(host);
        }
        r
    }

    /// `devices.*` commands from the user interface.
    pub(crate) fn devices_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let id = c.str_or("id", "").to_string();
        match name {
            "devices.list" => {
                let me = self.settings.device_id.clone();
                let now = self.server_now_ms(host);
                Value::Array(
                    self.devices
                        .list
                        .iter()
                        .map(|d| {
                            // Where a playing device is now.
                            let st = d.get("state");
                            let mut pos = st.f64_or("position", 0.0);
                            if st.str_or("play", "") == "playing" {
                                pos += (now - st.f64_or("positionAt", now)).max(0.0) / 1000.0;
                            }
                            let dur = st.get("track").f64_or("duration", 0.0);
                            if dur > 0.0 {
                                pos = pos.min(dur);
                            }
                            d.clone()
                                .with("self", d.str_or("id", "") == me)
                                .with("positionNow", pos)
                        })
                        .collect(),
                )
            }
            "devices.refresh" => {
                self.refresh_devices(host);
                ok()
            }
            // Control another device: {"id", "command": {"cmd": ...}}.
            "devices.command" => self.send_device_command(host, &id, c.get("command").clone()),
            "devices.transfer" => self.devices_transfer(host, &id),
            // Takes the playback of another device over.
            "devices.pull" => {
                let me = self.device_id();
                self.send_device_command(
                    host,
                    &id,
                    Value::object().with("cmd", "transferTo").with("target", me),
                )
            }
            // Plays songs on another device: {"id", "ids", "start"}.
            "devices.playOn" => {
                let ids: Vec<Value> = c
                    .get("ids")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v.as_str().is_some_and(server_song))
                    .cloned()
                    .collect();
                if ids.is_empty() {
                    return err(&self.tr("Only songs from the server can play on another device"));
                }
                let cmd = Value::object()
                    .with("cmd", "play")
                    .with("ids", Value::Array(ids))
                    .with("start", c.get("start").as_u64().unwrap_or(0));
                self.send_device_command(host, &id, cmd)
            }
            // Plays this device's queue on it and on `ids` together.
            "devices.group" => {
                let Some((ids, index, position)) = self.queue_for_transfer(host) else {
                    return err(&self.tr("Only songs from the server can play on another device"));
                };
                let at = self.server_now_ms(host) + GROUP_LEAD_MS;
                let cmd = Value::object()
                    .with("cmd", "playAt")
                    .with(
                        "ids",
                        Value::Array(ids.iter().cloned().map(Value::from).collect()),
                    )
                    .with("index", index as u64)
                    .with("position", position)
                    .with("at", at);
                for target in c
                    .get("ids")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    self.send_device_command(host, target, cmd.clone());
                }
                self.player.pause(host);
                self.player.seek(host, position);
                self.devices.play_at = Some((at, position));
                ok()
            }
            _ => err("unknown command"),
        }
    }
}
