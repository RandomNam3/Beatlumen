//! Listening together: a host plays songs of this server and the others in the
//! room follow. The server keeps the room and answers when it should sound:
//! a song starts once everybody has it (or after a moment, the slow ones catch
//! up), at a server time a little ahead. Nothing else travels here: what the
//! apps need goes over the event channel of `devices.rs`, and songs come from
//! the library like any other.
//!
//! Rooms live in memory and end with the host, or with the server. A guest
//! (somebody without a login) gets a token that opens this room and its songs
//! and nothing else, and that ends with the room. The host can turn guests off
//! for a room, the admin for the server (`--no-room-guests`).
//!
//! * `POST /api/rooms`: a new room, for a login.
//! * `POST /api/rooms/join` `{code, name, device}`: into a room; no login needed.
//! * `GET /api/rooms/<id>`: the room, for a member.
//! * `POST /api/rooms/<id>/state` `{track, upcoming, position, playing}`: from the host.
//! * `POST /api/rooms/<id>/ready` `{gen}`: this member has the song.
//! * `POST /api/rooms/<id>/heartbeat` `{driftMs}`: answers with the members.
//! * `POST /api/rooms/<id>/settings` `{guests}`: from the host.
//! * `DELETE /api/rooms/<id>`: leave; the host ends the room.
//!
//! Requests of a member carry the token from `POST /api/rooms` or `join`.

use crate::devices::{now_ms, Devices};
use crate::State;
use bl_json::Value;
use bl_net::server::{Request, Response};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Head start of a common start: the event has to arrive first.
const LEAD_MS: u64 = 1500;
/// How long everybody gets to have a new song.
const READY_WAIT: Duration = Duration::from_secs(8);
/// Members send a heartbeat every few seconds.
const MEMBER_GONE: Duration = Duration::from_secs(45);
const HOST_GONE: Duration = Duration::from_secs(120);
const MAX_ROOMS: usize = 100;
const MAX_MEMBERS: usize = 50;
const MAX_UPCOMING: usize = 20;
/// Without the ones easy to confuse.
const CODE_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;
/// Wrong codes from one address: guessing has to stay hopeless.
const MAX_WRONG: u32 = 20;
const WRONG_WINDOW: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    /// Waiting for everybody to have the song.
    Preparing,
    Playing,
    Paused,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Preparing => "preparing",
            Phase::Playing => "playing",
            Phase::Paused => "paused",
        }
    }
}

struct Member {
    name: String,
    token: String,
    guest: bool,
    device: String,
    /// The login's scope on the event channel (empty: a guest's own token).
    scope: String,
    ready_gen: u64,
    drift_ms: Option<f64>,
    seen: Instant,
}

struct Room {
    code: String,
    host: String,
    guests: bool,
    members: HashMap<String, Member>,
    /// Goes up with every new song.
    gen: u64,
    phase: Phase,
    /// The song, as the library describes it.
    track: Value,
    upcoming: Vec<Value>,
    /// Where the song is at server time `at`.
    position: f64,
    at: u64,
    /// After the wait: play, or stay paused?
    wants_play: bool,
    deadline: Instant,
}

impl Room {
    fn track_id(&self) -> &str {
        self.track.str_or("id", "")
    }

    fn start(&mut self, now: u64) {
        if self.wants_play {
            self.phase = Phase::Playing;
            self.at = now + LEAD_MS;
        } else {
            self.phase = Phase::Paused;
            self.at = now;
        }
    }

    fn start_if_ready(&mut self, now: u64) -> bool {
        let all = self.members.values().all(|m| m.ready_gen == self.gen);
        if self.phase == Phase::Preparing && all {
            self.start(now);
            return true;
        }
        false
    }

    fn members_json(&self) -> Value {
        let mut list: Vec<(&String, &Member)> = self.members.iter().collect();
        list.sort_by_key(|(id, m)| (**id != self.host, m.name.to_lowercase()));
        Value::Array(
            list.into_iter()
                .map(|(id, m)| {
                    let mut v = Value::object()
                        .with("id", id.as_str())
                        .with("name", m.name.as_str())
                        .with("host", *id == self.host)
                        .with("guest", m.guest)
                        .with("ready", m.ready_gen == self.gen);
                    if let Some(d) = m.drift_ms {
                        v = v.with("driftMs", d);
                    }
                    v
                })
                .collect(),
        )
    }

    /// Songs a guest may fetch, and their albums (for the covers).
    fn holds(&self, id: &str) -> bool {
        let ids = |t: &Value| t.str_or("id", "") == id || t.str_or("albumId", "") == id;
        ids(&self.track) || self.upcoming.iter().any(ids)
    }
}

#[derive(Default)]
struct Inner {
    rooms: HashMap<String, Room>,
    /// A member's token: its room and id.
    tokens: HashMap<String, (String, String)>,
    wrong: HashMap<IpAddr, (u32, Instant)>,
}

#[derive(Default)]
pub struct Rooms {
    inner: Mutex<Inner>,
}

/// What a request with a guest's token may do besides its own room.
pub struct Guest {
    pub room: String,
}

fn code_of(random: &[u8]) -> String {
    random
        .iter()
        .take(CODE_LEN)
        .map(|b| CODE_CHARS[*b as usize % CODE_CHARS.len()] as char)
        .collect()
}

/// As people type a code: small letters, spaces and dashes count for nothing.
fn clean_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

fn scope_room(id: &str) -> String {
    format!("room:{id}")
}

fn random_hex(n: usize) -> Result<String, Response> {
    crate::random::hex(n).map_err(|_| Response::error(500, "no randomness"))
}

fn body(req: &Request) -> Result<Value, Response> {
    bl_json::parse_bytes(&req.body).map_err(|e| Response::error(400, &format!("invalid JSON: {e}")))
}

fn room_json(id: &str, r: &Room) -> Value {
    Value::object()
        .with("id", id)
        .with("code", r.code.as_str())
        .with("host", r.host.as_str())
        .with("guests", r.guests)
        .with("gen", r.gen)
        .with("phase", r.phase.name())
        .with("track", r.track.clone())
        .with("upcoming", Value::Array(r.upcoming.clone()))
        .with("position", r.position)
        .with("at", r.at)
        .with("members", r.members_json())
        .with("now", now_ms())
}

impl Rooms {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(devices: &Devices, id: &str, r: &Room) {
        devices.notify(
            &scope_room(id),
            Value::object()
                .with("type", "room")
                .with("room", room_json(id, r)),
        );
    }

    fn end(inner: &mut Inner, devices: &Devices, id: &str) {
        if let Some(r) = inner.rooms.remove(id) {
            for m in r.members.values() {
                inner.tokens.remove(&m.token);
            }
            devices.notify(
                &scope_room(id),
                Value::object().with("type", "room.ended").with("room", id),
            );
        }
    }

    /// A guest's token, and what it opens.
    pub fn guest(&self, req: &Request) -> Option<Guest> {
        let token = crate::auth::Auth::request_token(req)?;
        let inner = self.lock();
        let (room, member) = inner.tokens.get(&token)?;
        let m = inner.rooms.get(room)?.members.get(member)?;
        m.guest.then(|| Guest { room: room.clone() })
    }

    /// Only what listening in this room takes, and that only reads.
    pub fn guest_may(&self, g: &Guest, method: &str, segs: &[&str]) -> bool {
        if !matches!(method, "GET" | "HEAD") {
            return false;
        }
        match segs {
            ["time"] | ["events"] => true,
            ["tracks", id, "stream" | "cover" | "loudness" | "lyrics" | "analysis"]
            | ["albums", id, "cover"] => {
                let inner = self.lock();
                inner.rooms.get(&g.room).is_some_and(|r| r.holds(id))
            }
            _ => false,
        }
    }

    /// The rooms this device of a login listens to.
    pub fn scopes_of(&self, device: &str, scope: &str) -> Vec<String> {
        if device.is_empty() {
            return Vec::new();
        }
        let inner = self.lock();
        inner
            .rooms
            .iter()
            .filter(|(_, r)| {
                r.members
                    .values()
                    .any(|m| m.device == device && m.scope == scope)
            })
            .map(|(id, _)| scope_room(id))
            .collect()
    }

    /// `POST /api/rooms`: for a login; the scope is its own on the event channel.
    pub fn create(&self, state: &State, req: &Request, scope: &str) -> Response {
        let v = match body(req) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let mut random = [0u8; CODE_LEN];
        let (id, member, token) = match (random_hex(8), random_hex(8), random_hex(16)) {
            (Ok(a), Ok(b), Ok(c)) if crate::random::fill(&mut random).is_ok() => (a, b, c),
            _ => return Response::error(500, "no randomness"),
        };
        let mut inner = self.lock();
        if inner.rooms.len() >= MAX_ROOMS {
            return Response::error(507, "too many rooms");
        }
        let mut code = code_of(&random);
        while inner.rooms.values().any(|r| r.code == code) {
            if crate::random::fill(&mut random).is_err() {
                return Response::error(500, "no randomness");
            }
            code = code_of(&random);
        }
        let device = clip(v.str_or("device", ""), 64);
        let mut members = HashMap::new();
        members.insert(
            member.clone(),
            Member {
                name: clip(v.str_or("name", ""), 64),
                token: token.clone(),
                guest: false,
                device: device.clone(),
                scope: scope.to_string(),
                ready_gen: 0,
                drift_ms: None,
                seen: Instant::now(),
            },
        );
        let room = Room {
            code,
            host: member.clone(),
            guests: v.bool_or("guests", true) && state.config().room_guests,
            members,
            gen: 0,
            phase: Phase::Idle,
            track: Value::Null,
            upcoming: Vec::new(),
            position: 0.0,
            at: 0,
            wants_play: false,
            deadline: Instant::now(),
        };
        let json = room_json(&id, &room);
        inner.rooms.insert(id.clone(), room);
        inner.tokens.insert(token.clone(), (id, member.clone()));
        drop(inner);
        // As on joining: the host's request already waiting on the event channel
        // looks again, now with the room. (It used to come back only by chance,
        // when the host's own device announcement made an event.)
        if !scope.is_empty() {
            state.devices.notify_device(
                scope,
                &device,
                Value::object().with("type", "room.joined"),
            );
        }
        Response::json(
            201,
            &Value::object()
                .with("room", json)
                .with("me", member)
                .with("token", token)
                .with("guest", false),
        )
    }

    /// `POST /api/rooms/join`: anyone with the code. Without a login it is as a
    /// guest, if this room and this server allow that.
    pub fn join(&self, state: &State, req: &Request, ip: Option<IpAddr>) -> Response {
        let v = match body(req) {
            Ok(v) => v,
            Err(r) => return r,
        };
        let user = state.vault.session_user(req);
        let password = state.auth.required() && state.auth.authorized(req);
        let guest = user.is_none() && !password;
        // The scope is the one `routes.rs` gives to this login's event channel.
        let scope = match &user {
            Some(u) => format!("user:{u}"),
            None if guest && state.auth.required() => String::new(),
            None => "shared".to_string(),
        };
        let (member, token) = match (random_hex(8), random_hex(16)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return Response::error(500, "no randomness"),
        };
        let code = clean_code(v.str_or("code", ""));
        let now = Instant::now();
        let mut guard = self.lock();
        let inner = &mut *guard;
        if let Some(ip) = ip {
            let (n, since) = *inner.wrong.get(&ip).unwrap_or(&(0, now));
            if n >= MAX_WRONG && now.duration_since(since) < WRONG_WINDOW {
                return Response::error(429, "too many attempts");
            }
        }
        let found = inner
            .rooms
            .iter()
            .find(|(_, r)| r.code == code)
            .map(|(id, _)| id.clone());
        let Some(id) = found else {
            if let Some(ip) = ip {
                inner
                    .wrong
                    .retain(|_, (_, s)| now.duration_since(*s) < WRONG_WINDOW);
                let e = inner.wrong.entry(ip).or_insert((0, now));
                e.0 += 1;
            }
            return Response::error(404, "no such room");
        };
        let room = inner.rooms.get_mut(&id).expect("just found");
        if guest && !(room.guests && state.config().room_guests) {
            return Response::error(403, "this room takes no guests");
        }
        if room.members.len() >= MAX_MEMBERS {
            return Response::error(507, "the room is full");
        }
        room.members.insert(
            member.clone(),
            Member {
                name: clip(v.str_or("name", ""), 64),
                token: token.clone(),
                guest,
                device: clip(v.str_or("device", ""), 64),
                scope: scope.clone(),
                ready_gen: 0,
                drift_ms: None,
                seen: now,
            },
        );
        let device = clip(v.str_or("device", ""), 64);
        let json = room_json(&id, room);
        Self::publish(&state.devices, &id, room);
        inner.tokens.insert(token.clone(), (id, member.clone()));
        drop(guard);
        // A request already waiting on the event channel should look again.
        if !scope.is_empty() {
            state.devices.notify_device(
                &scope,
                &device,
                Value::object().with("type", "room.joined"),
            );
        }
        Response::json(
            200,
            &Value::object()
                .with("room", json)
                .with("me", member)
                .with("token", token)
                .with("guest", guest),
        )
    }

    /// `/api/rooms/<id>...` with a member's token.
    pub fn member_request(
        &self,
        state: &State,
        req: &Request,
        id: &str,
        rest: &[&str],
    ) -> Response {
        let Some(token) = crate::auth::Auth::request_token(req) else {
            return Response::error(401, "authentication required");
        };
        let mut guard = self.lock();
        let inner = &mut *guard;
        let member = match inner.tokens.get(&token) {
            Some((room, member)) if room == id => member.clone(),
            _ => return Response::error(401, "not in this room"),
        };
        let Some(room) = inner.rooms.get_mut(id) else {
            return Response::error(404, "no such room");
        };
        let is_host = room.host == member;
        if let Some(m) = room.members.get_mut(&member) {
            m.seen = Instant::now();
        }
        let now = now_ms();
        match (req.method.as_str(), rest) {
            ("GET", []) => Response::json(200, &room_json(id, room)),
            ("POST", ["heartbeat"]) => {
                let v = match body(req) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                if let Some(m) = room.members.get_mut(&member) {
                    m.drift_ms = v.get("driftMs").as_f64();
                }
                Response::json(
                    200,
                    &Value::object()
                        .with("members", room.members_json())
                        .with("now", now),
                )
            }
            ("POST", ["ready"]) => {
                let v = match body(req) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                let gen = v.get("gen").as_u64().unwrap_or(0);
                if gen != room.gen {
                    return Response::new(204);
                }
                if let Some(m) = room.members.get_mut(&member) {
                    m.ready_gen = gen;
                }
                if room.start_if_ready(now) {
                    Self::publish(&state.devices, id, room);
                }
                Response::new(204)
            }
            ("POST", ["state"]) => {
                if !is_host {
                    return Response::error(403, "only the host decides");
                }
                let v = match body(req) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                let idx = state.library();
                let song = |id: &str| {
                    crate::util::is_id(id)
                        .then(|| idx.track(id))
                        .flatten()
                        .map(|e| e.track.to_json())
                };
                let upcoming: Vec<Value> = v
                    .get("upcoming")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_str())
                    .filter_map(song)
                    .take(MAX_UPCOMING)
                    .collect();
                // Only what comes next changed: this goes on as it is.
                let Some(track) = v.get("track").as_str() else {
                    room.upcoming = upcoming;
                    Self::publish(&state.devices, id, room);
                    return Response::new(204);
                };
                let info = if track.is_empty() {
                    Value::Null
                } else {
                    // Only songs of this server.
                    match song(track) {
                        Some(t) => t,
                        None => return Response::error(404, "not a song of this server"),
                    }
                };
                let position = v.f64_or("position", 0.0).max(0.0);
                let playing = v.bool_or("playing", true);
                room.upcoming = upcoming;
                set_song(room, track, info, position, playing, now, Instant::now());
                room.start_if_ready(now);
                Self::publish(&state.devices, id, room);
                Response::json(200, &room_json(id, room))
            }
            ("POST", ["settings"]) => {
                if !is_host {
                    return Response::error(403, "only the host decides");
                }
                let v = match body(req) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                let on = v.bool_or("guests", room.guests) && state.config().room_guests;
                if !on {
                    drop_guests(room, id, &mut inner.tokens);
                }
                room.guests = on;
                Self::publish(&state.devices, id, room);
                Response::json(200, &room_json(id, room))
            }
            ("DELETE", []) => {
                if is_host {
                    Self::end(inner, &state.devices, id);
                } else {
                    room.members.remove(&member);
                    room.start_if_ready(now);
                    Self::publish(&state.devices, id, room);
                    inner.tokens.remove(&token);
                }
                Response::new(204)
            }
            _ => Response::error(405, "method not allowed"),
        }
    }

    /// Goes on by itself: the wait for a song ends, those gone leave, and rooms
    /// without their host end.
    pub fn tick(&self, devices: &Devices, guests_allowed: bool, now: Instant) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let ids: Vec<String> = inner.rooms.keys().cloned().collect();
        for id in ids {
            let room = inner.rooms.get_mut(&id).expect("just listed");
            let host_gone = room
                .members
                .get(&room.host)
                .is_none_or(|m| now.saturating_duration_since(m.seen) > HOST_GONE);
            if host_gone {
                Self::end(inner, devices, &id);
                continue;
            }
            let host = room.host.clone();
            let before = room.members.len();
            room.members
                .retain(|m, v| *m == host || now.saturating_duration_since(v.seen) <= MEMBER_GONE);
            let mut changed = room.members.len() != before;
            if changed {
                keep_tokens(room, &id, &mut inner.tokens);
            }
            if !guests_allowed && room.guests {
                drop_guests(room, &id, &mut inner.tokens);
                room.guests = false;
                changed = true;
            }
            let ms = now_ms();
            if room.phase == Phase::Preparing {
                if now >= room.deadline {
                    room.start(ms);
                    changed = true;
                } else if room.start_if_ready(ms) {
                    changed = true;
                }
            }
            if changed {
                Self::publish(devices, &id, room);
            }
        }
    }
}

/// A token is good for as long as its member is in the room.
fn keep_tokens(room: &Room, id: &str, tokens: &mut HashMap<String, (String, String)>) {
    let left: HashSet<&String> = room.members.values().map(|m| &m.token).collect();
    tokens.retain(|t, (r, _)| r != id || left.contains(t));
}

fn drop_guests(room: &mut Room, id: &str, tokens: &mut HashMap<String, (String, String)>) {
    room.members.retain(|_, m| !m.guest);
    keep_tokens(room, id, tokens);
}

/// The host chose: another song waits for everybody, the same one goes on from
/// the given position at a common time.
fn set_song(
    room: &mut Room,
    track: &str,
    info: Value,
    position: f64,
    playing: bool,
    now: u64,
    instant: Instant,
) {
    room.position = position;
    room.wants_play = playing;
    if track != room.track_id() {
        room.track = info;
        room.gen += 1;
        room.phase = if track.is_empty() {
            Phase::Idle
        } else {
            Phase::Preparing
        };
        room.at = now;
        room.deadline = instant + READY_WAIT;
    } else if !track.is_empty() && room.phase != Phase::Preparing {
        room.start(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str) -> Member {
        Member {
            name: name.into(),
            token: format!("token-{name}"),
            guest: false,
            device: format!("device-{name}"),
            scope: "shared".into(),
            ready_gen: 0,
            drift_ms: None,
            seen: Instant::now(),
        }
    }

    fn room() -> Room {
        let mut members = HashMap::new();
        members.insert("a".to_string(), member("a"));
        members.insert("b".to_string(), member("b"));
        Room {
            code: "ABCDEFGH".into(),
            host: "a".into(),
            guests: true,
            members,
            gen: 0,
            phase: Phase::Idle,
            track: Value::Null,
            upcoming: Vec::new(),
            position: 0.0,
            at: 0,
            wants_play: false,
            deadline: Instant::now(),
        }
    }

    fn song(id: &str) -> Value {
        Value::object().with("id", id).with("albumId", "album")
    }

    #[test]
    fn a_code_reads_however_it_was_typed() {
        assert_eq!(clean_code(" abcd-efgh "), "ABCDEFGH");
        let code = code_of(&[0, 1, 2, 3, 4, 5, 6, 255]);
        assert_eq!(code.len(), CODE_LEN);
        assert!(code.bytes().all(|c| CODE_CHARS.contains(&c)));
        // Nothing to mix up.
        assert!(!CODE_CHARS.iter().any(|c| b"IL01O".contains(c)));
    }

    #[test]
    fn a_new_song_waits_until_everybody_has_it() {
        let mut r = room();
        let now = Instant::now();
        set_song(&mut r, "s1", song("s1"), 0.0, true, 1000, now);
        assert_eq!(r.phase, Phase::Preparing);
        assert_eq!(r.gen, 1);
        assert!(!r.start_if_ready(1000));
        r.members.get_mut("a").unwrap().ready_gen = 1;
        assert!(!r.start_if_ready(1000), "one is not there yet");
        r.members.get_mut("b").unwrap().ready_gen = 1;
        assert!(r.start_if_ready(2000));
        assert_eq!(r.phase, Phase::Playing);
        assert_eq!(r.at, 2000 + LEAD_MS, "with a head start");
    }

    #[test]
    fn a_new_song_stays_paused_if_the_host_paused() {
        let mut r = room();
        set_song(&mut r, "s1", song("s1"), 12.0, false, 1000, Instant::now());
        r.members.get_mut("a").unwrap().ready_gen = 1;
        r.members.get_mut("b").unwrap().ready_gen = 1;
        assert!(r.start_if_ready(2000));
        assert_eq!(r.phase, Phase::Paused);
        assert_eq!(r.position, 12.0);
    }

    #[test]
    fn the_same_song_goes_on_without_a_new_wait() {
        let mut r = room();
        set_song(&mut r, "s1", song("s1"), 0.0, true, 1000, Instant::now());
        r.start(1000);
        // Pause, then on from another place.
        set_song(&mut r, "s1", Value::Null, 30.0, false, 5000, Instant::now());
        assert_eq!((r.phase, r.gen), (Phase::Paused, 1));
        assert_eq!(r.position, 30.0);
        set_song(&mut r, "s1", Value::Null, 40.0, true, 6000, Instant::now());
        assert_eq!((r.phase, r.gen, r.at), (Phase::Playing, 1, 6000 + LEAD_MS));
        // Nothing left to play.
        set_song(&mut r, "", Value::Null, 0.0, false, 7000, Instant::now());
        assert_eq!((r.phase, r.gen), (Phase::Idle, 2));
    }

    #[test]
    fn the_wait_for_a_song_ends_and_the_host_ending_the_room() {
        let rooms = Rooms::default();
        let devices = Devices::default();
        let mut r = room();
        let now = Instant::now();
        set_song(&mut r, "s1", song("s1"), 0.0, true, 1000, now);
        r.members.get_mut("a").unwrap().ready_gen = 1;
        rooms.lock().rooms.insert("r".into(), r);
        rooms.tick(&devices, true, now);
        assert_eq!(rooms.lock().rooms["r"].phase, Phase::Preparing);
        // The slow one catches up by itself.
        rooms.tick(&devices, true, now + READY_WAIT + Duration::from_secs(1));
        assert_eq!(rooms.lock().rooms["r"].phase, Phase::Playing);
        // Those not heard from leave; without the host the room ends.
        rooms
            .lock()
            .rooms
            .get_mut("r")
            .unwrap()
            .members
            .get_mut("a")
            .unwrap()
            .seen = now + Duration::from_secs(100);
        rooms.tick(&devices, true, now + Duration::from_secs(100));
        assert_eq!(rooms.lock().rooms["r"].members.len(), 1);
        rooms.tick(&devices, true, now + Duration::from_secs(500));
        assert!(rooms.lock().rooms.is_empty());
    }

    #[test]
    fn a_guest_opens_this_room_and_its_songs_only() {
        let rooms = Rooms::default();
        let mut r = room();
        r.track = song("s1");
        r.upcoming = vec![song("s2")];
        rooms.lock().rooms.insert("r".into(), r);
        let g = Guest { room: "r".into() };
        assert!(rooms.guest_may(&g, "GET", &["tracks", "s1", "stream"]));
        assert!(rooms.guest_may(&g, "GET", &["tracks", "s2", "cover"]));
        assert!(rooms.guest_may(&g, "GET", &["albums", "album", "cover"]));
        assert!(rooms.guest_may(&g, "GET", &["time"]));
        assert!(rooms.guest_may(&g, "GET", &["events"]));
        assert!(!rooms.guest_may(&g, "GET", &["tracks", "s3", "stream"]));
        assert!(!rooms.guest_may(&g, "GET", &["library"]));
        assert!(!rooms.guest_may(&g, "GET", &["devices"]));
        assert!(!rooms.guest_may(&g, "GET", &["rescan"]));
        let gone = Guest { room: "x".into() };
        assert!(!rooms.guest_may(&gone, "GET", &["tracks", "s1", "stream"]));
        // Nothing to change: the song's text, for one.
        assert!(!rooms.guest_may(&g, "PUT", &["tracks", "s1", "lyrics"]));
        assert!(!rooms.guest_may(&g, "POST", &["tracks", "s1", "stream"]));
    }

    #[test]
    fn guests_go_when_the_admin_turns_them_off() {
        let rooms = Rooms::default();
        let devices = Devices::default();
        let mut r = room();
        r.members.get_mut("b").unwrap().guest = true;
        let t = r.members["b"].token.clone();
        rooms.lock().rooms.insert("r".into(), r);
        rooms
            .lock()
            .tokens
            .insert(t.clone(), ("r".into(), "b".into()));
        rooms.tick(&devices, true, Instant::now());
        assert_eq!(rooms.lock().rooms["r"].members.len(), 2);
        rooms.tick(&devices, false, Instant::now());
        let inner = rooms.lock();
        assert_eq!(inner.rooms["r"].members.len(), 1);
        assert!(!inner.rooms["r"].guests);
        assert!(!inner.tokens.contains_key(&t), "the token ends with it");
    }
}
