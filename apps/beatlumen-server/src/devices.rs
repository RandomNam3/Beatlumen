//! Devices: every logged-in app announces itself (name, platform, what it
//! plays) and keeps one long-polling request open on the event channel.
//! Devices of the same scope (the same account, or everyone logged in with
//! the server password) see each other and send each other commands
//! (remote control, transfer, play at a common time); the server only
//! relays them. Devices open no ports for this.
//!
//! * `PUT /api/devices/<id>`: announce or update a device (heartbeat).
//! * `DELETE /api/devices/<id>`: the device goes away.
//! * `GET /api/devices`: the devices of the scope.
//! * `POST /api/devices/<id>/command`: a command for a device (only if it
//!   allows remote control).
//! * `GET /api/events?device=<id>&since=<seq>`: waits up to 20 s for events
//!   (`devices` changed, `command` for this device) after `since`.
//! * `GET /api/time`: the server clock (milliseconds) for playing in sync.
//!
//! Limits: a command has at most [`MAX_COMMAND`] bytes, a device has at most
//! [`MAX_WAITING`] commands ([`MAX_WAITING_BYTES`]) it has not fetched yet,
//! a login at most [`MAX_SCOPE_DEVICES`] devices, and the event buffer of
//! all logins at most [`MAX_EVENTS`] events of [`MAX_EVENT_BYTES`].

use bl_json::Value;
use bl_net::server::{Request, Response};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A device not heard from for this long is offline.
const ONLINE: Duration = Duration::from_secs(50);
/// Devices not heard from for this long are forgotten.
const FORGET: Duration = Duration::from_secs(30 * 24 * 3600);
const MAX_DEVICES: usize = 200;
/// Devices of one login (an account, or the server password).
pub const MAX_SCOPE_DEVICES: usize = 50;
pub const MAX_EVENTS: usize = 1000;
/// The events in the buffer together (their JSON).
pub const MAX_EVENT_BYTES: usize = 32 << 20;
/// One command (its JSON).
pub const MAX_COMMAND: usize = 16 * 1024;
/// Commands for one device it has not fetched yet.
pub const MAX_WAITING: usize = 64;
pub const MAX_WAITING_BYTES: usize = 256 * 1024;
const EVENT_AGE: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(20);
/// Size limit of a device's reported state.
const MAX_STATE: usize = 64 * 1024;

struct Device {
    scope: String,
    name: String,
    platform: String,
    kind: String,
    remote: bool,
    state: Value,
    seen: Instant,
    /// The events up to here it fetched (the `since` of its last poll).
    fetched: u64,
}

struct Event {
    seq: u64,
    scope: String,
    /// Only for this device (None: every device of the scope).
    target: Option<String>,
    body: Value,
    at: Instant,
    /// Size of `body` as JSON.
    bytes: usize,
    command: bool,
}

#[derive(Default)]
struct Inner {
    devices: HashMap<String, Device>,
    events: VecDeque<Event>,
    /// `bytes` of all events.
    bytes: usize,
    seq: u64,
}

#[derive(Default)]
pub struct Devices {
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn valid_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn body(req: &Request) -> Result<Value, Response> {
    bl_json::parse_bytes(&req.body).map_err(|e| Response::error(400, &format!("invalid JSON: {e}")))
}

impl Devices {
    fn push(&self, inner: &mut Inner, scope: &str, target: Option<&str>, body: Value) {
        inner.seq += 1;
        let seq = inner.seq;
        let bytes = body.to_json().len();
        let command = body.str_or("type", "") == "command";
        inner.bytes += bytes;
        inner.events.push_back(Event {
            seq,
            scope: scope.to_string(),
            target: target.map(str::to_string),
            body,
            at: Instant::now(),
            bytes,
            command,
        });
        // Listeners that fall behind the buffer are told they missed some.
        while inner.events.len() > MAX_EVENTS
            || inner.bytes > MAX_EVENT_BYTES
            || inner
                .events
                .front()
                .is_some_and(|e| e.at.elapsed() > EVENT_AGE)
        {
            match inner.events.pop_front() {
                Some(e) => inner.bytes -= e.bytes,
                None => break,
            }
        }
        self.cv.notify_all();
    }

    fn device_json(id: &str, d: &Device) -> Value {
        Value::object()
            .with("id", id)
            .with("name", d.name.as_str())
            .with("platform", d.platform.as_str())
            .with("kind", d.kind.as_str())
            .with("remote", d.remote)
            .with("online", d.seen.elapsed() < ONLINE)
            .with("state", d.state.clone())
    }

    /// Tells the devices of a scope about a change (e.g. synced settings).
    pub fn notify(&self, scope: &str, body: Value) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.push(&mut inner, scope, None, body);
    }

    /// The scope of a known device.
    pub fn scope_of(&self, id: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.devices.get(id).map(|d| d.scope.clone())
    }

    /// Tells one device of a scope.
    pub fn notify_device(&self, scope: &str, device: &str, body: Value) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.push(&mut inner, scope, Some(device), body);
    }

    /// Devices in a scope (for the admin overview: all of them).
    pub fn count_online(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .devices
            .values()
            .filter(|d| d.seen.elapsed() < ONLINE)
            .count()
    }

    /// `/api/devices...`, `/api/events`, `/api/time` for a request whose
    /// login belongs to `scope`.
    /// `rooms`: the scopes of rooms this device listens to as well.
    pub fn handle(&self, req: &Request, segs: &[&str], scope: &str, rooms: &[String]) -> Response {
        let m = req.method.as_str();
        match (m, segs) {
            ("GET", ["time"]) => Response::json(200, &Value::object().with("now", now_ms())),
            ("GET", ["events"]) => self.events(req, scope, rooms),
            ("GET", ["devices"]) => {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let mut list: Vec<(&String, &Device)> = inner
                    .devices
                    .iter()
                    .filter(|(_, d)| d.scope == scope)
                    .collect();
                list.sort_by_key(|a| a.1.name.to_lowercase());
                Response::json(
                    200,
                    &Value::Array(
                        list.into_iter()
                            .map(|(id, d)| Self::device_json(id, d))
                            .collect(),
                    ),
                )
            }
            ("PUT", ["devices", id]) => self.announce(req, id, scope),
            ("DELETE", ["devices", id]) => {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.devices.get(*id).is_some_and(|d| d.scope == scope) {
                    inner.devices.remove(*id);
                    self.push(
                        &mut inner,
                        scope,
                        None,
                        Value::object().with("type", "devices"),
                    );
                }
                Response::new(204)
            }
            ("POST", ["devices", id, "command"]) => self.command(req, id, scope),
            _ => Response::error(405, "method not allowed"),
        }
    }

    fn announce(&self, req: &Request, id: &str, scope: &str) -> Response {
        if !valid_id(id) {
            return Response::error(400, "invalid device id");
        }
        if req.body.len() > MAX_STATE {
            return Response::error(413, "device state too large");
        }
        let v = match body(req) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Another scope's device with this id stays its own.
        if inner.devices.get(id).is_some_and(|d| d.scope != scope) {
            return Response::error(409, "device id in use");
        }
        inner.devices.retain(|_, d| d.seen.elapsed() < FORGET);
        if !inner.devices.contains_key(id) {
            // Full: the device offline the longest makes room (of this login,
            // or of all); one login cannot take every place.
            let in_scope = inner.devices.values().filter(|d| d.scope == scope).count();
            let full_scope = in_scope >= MAX_SCOPE_DEVICES;
            if full_scope || inner.devices.len() >= MAX_DEVICES {
                let oldest = inner
                    .devices
                    .iter()
                    .filter(|(_, d)| {
                        d.seen.elapsed() >= ONLINE && (!full_scope || d.scope == scope)
                    })
                    .min_by_key(|(_, d)| d.seen)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => {
                        inner.devices.remove(&k);
                    }
                    None => return Response::error(507, "too many devices"),
                }
            }
        }
        let name = clip(v.str_or("name", "").trim(), 64);
        let dev = Device {
            scope: scope.to_string(),
            name: if name.is_empty() {
                "Beatlumen".into()
            } else {
                name
            },
            platform: clip(v.str_or("platform", ""), 16),
            kind: clip(v.str_or("kind", ""), 16),
            remote: v.bool_or("remote", true),
            state: v.get("state").clone(),
            seen: Instant::now(),
            // A new device has nothing waiting.
            fetched: inner.devices.get(id).map_or(inner.seq, |d| d.fetched),
        };
        let changed = match inner.devices.get(id) {
            None => true,
            Some(old) => {
                old.seen.elapsed() >= ONLINE
                    || old.name != dev.name
                    || old.remote != dev.remote
                    || old.state != dev.state
            }
        };
        inner.devices.insert(id.to_string(), dev);
        if changed {
            self.push(
                &mut inner,
                scope,
                None,
                Value::object().with("type", "devices").with("device", id),
            );
        }
        Response::json(200, &Value::object().with("now", now_ms()))
    }

    fn command(&self, req: &Request, id: &str, scope: &str) -> Response {
        if req.body.len() > MAX_COMMAND + 1024 {
            return Response::error(413, "command too large");
        }
        let v = match body(req) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let cmd = v.get("command");
        if !cmd
            .get("cmd")
            .as_str()
            .is_some_and(|c| !c.is_empty() && c.len() < 32)
        {
            return Response::error(400, "expected {\"command\": {\"cmd\": ...}}");
        }
        let size = cmd.to_json().len();
        if size > MAX_COMMAND {
            return Response::error(413, "command too large");
        }
        let from = clip(v.str_or("from", ""), 64);
        let from = from.as_str();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(d) = inner.devices.get(id).filter(|d| d.scope == scope) else {
            return Response::error(404, "no such device");
        };
        if d.seen.elapsed() >= ONLINE {
            return Response::error(409, "the device is offline");
        }
        if !d.remote {
            return Response::error(403, "the device does not allow remote control");
        }
        // What waits for the device to fetch it.
        let (n, bytes) = inner
            .events
            .iter()
            .filter(|e| e.command && e.seq > d.fetched && e.target.as_deref() == Some(id))
            .fold((0, 0), |(n, b), e| (n + 1, b + e.bytes));
        if n >= MAX_WAITING || bytes + size > MAX_WAITING_BYTES {
            return Response::error(429, "too many commands are waiting for this device");
        }
        let from_name = inner
            .devices
            .get(from)
            .filter(|d| d.scope == scope)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        self.push(
            &mut inner,
            scope,
            Some(id),
            Value::object()
                .with("type", "command")
                .with("from", from)
                .with("fromName", from_name)
                .with("command", cmd.clone()),
        );
        Response::new(202)
    }

    /// Long polling: the events after `since` for this device, waiting up
    /// to 20 s for the first one. Counts as a heartbeat.
    fn events(&self, req: &Request, scope: &str, rooms: &[String]) -> Response {
        let device = req.query("device").unwrap_or("").to_string();
        let since: u64 = req
            .query("since")
            .and_then(|s| s.parse().ok())
            .unwrap_or(u64::MAX);
        let deadline = Instant::now() + WAIT;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // A new listener starts from now.
        let since = if since == u64::MAX { inner.seq } else { since };
        loop {
            let newest = inner.seq;
            if let Some(d) = inner.devices.get_mut(&device).filter(|d| d.scope == scope) {
                let back = d.seen.elapsed() >= ONLINE;
                d.seen = Instant::now();
                d.fetched = d.fetched.max(since.min(newest));
                if back {
                    self.push(
                        &mut inner,
                        scope,
                        None,
                        Value::object().with("type", "devices"),
                    );
                }
            }
            let list: Vec<Value> = inner
                .events
                .iter()
                .filter(|e| {
                    e.seq > since
                        && (e.scope == scope || rooms.contains(&e.scope))
                        && !matches!(e.target.as_deref(), Some(t) if t != device)
                })
                .map(|e| e.body.clone().with("seq", e.seq))
                .collect();
            // Events older than the buffer were missed: ask for a refresh.
            let missed =
                since < inner.seq && inner.events.front().is_some_and(|e| e.seq > since + 1);
            let now = Instant::now();
            if !list.is_empty() || missed || now >= deadline {
                return Response::json(
                    200,
                    &Value::object()
                        .with("seq", inner.seq)
                        .with("now", now_ms())
                        .with("missed", missed)
                        .with("events", Value::Array(list)),
                );
            }
            inner = self
                .cv
                .wait_timeout(inner, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
