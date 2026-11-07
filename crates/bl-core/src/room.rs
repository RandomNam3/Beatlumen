//! Listening together: one device hosts a room, the others follow it. The server
//! keeps the room (`apps/beatlumen-server/src/rooms.rs`); its changes arrive over
//! the event channel of `devices.rs`. Only songs of the server can be in a room.
//!
//! A song starts when everybody has it, at a server time a little ahead. From
//! there on every device, the host too, compares where it is with where it should
//! be and corrects itself (`clock::correction`).

use crate::clock::{correction, Correction};
use crate::core::{Core, Req};
use crate::host::Host;
use crate::player::PlayState;
use crate::server::ServerState;
use bl_json::Value;
use bl_net::api::Track;

/// Seconds between heartbeats (the server drops members it no longer hears).
const BEAT: f64 = 5.0;
/// Seconds between looks at the timing.
const CHECK: f64 = 1.0;
/// After a start or a seek the position needs a moment to mean anything.
const SETTLE: f64 = 2.0;
/// Seconds of a song to have before saying ready.
const READY_AUDIO: f64 = 0.5;
/// Seconds before trying again to load a song that did not come.
const RELOAD: f64 = 5.0;
/// What a member of a room leaves to the host.
const HOST_ONLY: &[&str] = &[
    "play",
    "pause",
    "toggle",
    "stop",
    "next",
    "prev",
    "seek",
    "seekBy",
    "open",
    "enqueue",
    "queue.play",
    "queue.remove",
    "queue.move",
    "queue.clear",
    "shuffle",
    "repeat",
    "songs.radio",
];

#[derive(Debug, Clone)]
pub(crate) enum RoomReq {
    /// Created or joined.
    Entered,
    /// The host's state, with the room as the answer.
    State,
    Quiet,
    Beat,
    Settings,
}

#[derive(Default)]
pub(crate) struct RoomState {
    pub id: String,
    pub code: String,
    /// This member, and the token of its room.
    pub me: String,
    pub token: String,
    pub host_member: String,
    pub guest: bool,
    pub guests: bool,
    pub gen: u64,
    pub phase: String,
    pub track: String,
    pub position: f64,
    /// Server time (ms) `position` is meant for.
    pub at: f64,
    pub members: Value,
    pub upcoming: Vec<Value>,
    pub error: String,
    /// Busy creating or joining.
    pub entering: bool,
    /// A code to join once the server answers.
    pub pending: Option<String>,
    /// The app's login is this guest's token (a server with a password).
    pub guest_login: bool,
    /// The change of the room last acted on.
    applied: (u64, String, u64),
    /// A common start still to come: (server time ms, position).
    start_at: Option<(f64, f64)>,
    ready_sent: u64,
    load_at: f64,
    beat_at: f64,
    check_at: f64,
    drift_ms: Option<f64>,
    /// A change is on its way to the server.
    posting: bool,
    /// What the host last chose, until the room says so (position, playing, at
    /// which server time).
    chosen: Option<(f64, bool, f64)>,
    /// The next change, to go once the server answered the last.
    waiting: Option<(String, f64, bool)>,
    /// The songs after this one, as last sent.
    sent_upcoming: Vec<String>,
}

/// A code or a link with one: (the server's address, the code).
pub(crate) fn parse_invitation(s: &str) -> (Option<String>, String) {
    let s = s.trim();
    let Some(at) = s.find("room=") else {
        return (None, s.to_string());
    };
    let code = s[at + 5..]
        .split(['&', '#'])
        .next()
        .unwrap_or("")
        .to_string();
    let base = s[..at]
        .trim_end_matches(['?', '&'])
        .trim_end_matches('/')
        .to_string();
    (Some(base).filter(|b| b.contains("://")), code)
}

fn ok() -> Value {
    Value::object().with("ok", true)
}

fn server_song(id: &str) -> bool {
    !id.is_empty() && !id.contains(':')
}

impl Core {
    pub(crate) fn in_room(&self) -> bool {
        !self.room.id.is_empty()
    }

    pub(crate) fn room_host(&self) -> bool {
        self.in_room() && self.room.me == self.room.host_member
    }

    /// Playing by a common clock: ask it often enough.
    pub(crate) fn clock_wanted(&self) -> bool {
        self.in_room() || self.devices.group_anchor.is_some()
    }

    fn room_send(
        &mut self,
        host: &mut dyn Host,
        method: &str,
        path: &str,
        body: Value,
        r: RoomReq,
    ) {
        let token = self.room.token.clone();
        let path = format!("/api/rooms/{}{path}", self.room.id);
        let rid = self.device_request_as(
            host,
            &token,
            method,
            &path,
            body.to_json().into_bytes(),
            Some("application/json"),
        );
        self.requests.insert(rid, Req::Room(r));
    }

    pub(crate) fn room_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        match name {
            "room.create" => {
                if self.in_room() || self.room.entering {
                    return ok();
                }
                let body = Value::object()
                    .with("name", self.device_name(host))
                    .with("device", self.device_id())
                    .with("guests", c.bool_or("guests", true));
                self.room.error.clear();
                self.room.entering = true;
                let rid = self.device_request(
                    host,
                    "POST",
                    "/api/rooms",
                    body.to_json().into_bytes(),
                    Some("application/json"),
                );
                self.requests.insert(rid, Req::Room(RoomReq::Entered));
            }
            "room.join" => self.room_join(host, c.str_or("code", "")),
            "room.leave" => {
                if self.in_room() {
                    self.room_send(host, "DELETE", "", Value::object(), RoomReq::Quiet);
                    self.room_left(host, None);
                }
            }
            "room.guests" => {
                if self.room_host() {
                    let body = Value::object().with("guests", c.bool_or("on", true));
                    self.room_send(host, "POST", "/settings", body, RoomReq::Settings);
                }
            }
            _ => return Value::object().with("error", "unknown command"),
        }
        ok()
    }

    pub(crate) fn room_join(&mut self, host: &mut dyn Host, invitation: &str) {
        if self.in_room() || self.room.entering {
            return;
        }
        let (base, code) = parse_invitation(invitation);
        if code.is_empty() {
            return;
        }
        self.room.error.clear();
        if let Some(base) = base.filter(|b| *b != self.server.base) {
            // Another server: there first.
            self.room.pending = Some(code);
            self.command(
                host,
                &Value::object()
                    .with("cmd", "server.connect")
                    .with("url", base),
            );
            return;
        }
        let reachable = matches!(
            self.server.state,
            ServerState::Connected(_) | ServerState::NeedsLogin(_)
        );
        if !reachable {
            if matches!(self.server.state, ServerState::Connecting) {
                self.room.pending = Some(code);
            } else {
                self.room.error = self
                    .tr("Enter the whole link: this device is not connected to a server")
                    .to_string();
            }
            return;
        }
        let body = Value::object()
            .with("code", code)
            .with("name", self.device_name(host))
            .with("device", self.device_id());
        self.room.entering = true;
        let rid = self.device_request(
            host,
            "POST",
            "/api/rooms/join",
            body.to_json().into_bytes(),
            Some("application/json"),
        );
        self.requests.insert(rid, Req::Room(RoomReq::Entered));
    }

    /// Takes the host's controls; `None`: the command goes on as usual.
    pub(crate) fn room_transport(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Option<Value> {
        if !self.in_room() {
            return None;
        }
        if !self.room_host() {
            if HOST_ONLY.contains(&name) {
                let msg = self.tr("Only the host chooses what plays").to_string();
                self.toast(msg, false);
                return Some(ok());
            }
            return None;
        }
        // Another song: as usual, `poll_room` notices.
        let track = self.room.track.clone();
        if track.is_empty() {
            return None;
        }
        let playing = match self.room.chosen {
            Some((_, playing, _)) => playing,
            None => matches!(self.room.phase.as_str(), "playing" | "preparing"),
        };
        let position = self.room_position(host);
        let (position, play) = match name {
            "pause" => (position, false),
            "toggle" => (position, !playing),
            "play" if c.get("ids").as_array().is_none_or(|a| a.is_empty()) => (position, true),
            "seek" => (c.f64_or("position", 0.0).max(0.0), playing),
            "seekBy" => ((position + c.f64_or("delta", 0.0)).max(0.0), playing),
            _ => return None,
        };
        self.room_post(host, &track, position, play);
        Some(ok())
    }

    /// Where the room's song is by now.
    fn room_position(&self, host: &dyn Host) -> f64 {
        if let Some((position, playing, at)) = self.room.chosen {
            let since = (self.server_now_ms(host) - at).max(0.0) / 1000.0;
            return position + if playing { since } else { 0.0 };
        }
        if self.room.phase == "playing" {
            self.room.position + ((self.server_now_ms(host) - self.room.at) / 1000.0).max(0.0)
        } else {
            self.room.position
        }
    }

    fn room_upcoming(&self) -> Vec<String> {
        let current = self.queue.position().unwrap_or(0);
        self.queue
            .ordered()
            .skip(current + 1)
            .map(|(_, id)| id)
            .filter(|id| server_song(id))
            .take(20)
            .cloned()
            .collect()
    }

    fn room_post(&mut self, host: &mut dyn Host, track: &str, position: f64, playing: bool) {
        self.room.chosen = Some((position, playing, self.server_now_ms(host)));
        // One at a time: they must not overtake each other on the way.
        if self.room.posting {
            self.room.waiting = Some((track.to_string(), position, playing));
            return;
        }
        let upcoming = self.room_upcoming();
        let body = Value::object()
            .with("track", track)
            .with("upcoming", upcoming.clone())
            .with("position", position)
            .with("playing", playing);
        self.room.sent_upcoming = upcoming;
        self.room.posting = true;
        self.room_send(host, "POST", "/state", body, RoomReq::State);
    }

    /// The host's player chose another song (a click, or the last one ended).
    fn room_host_step(&mut self, host: &mut dyn Host) {
        if self.room.posting {
            return;
        }
        let current = self
            .queue
            .current()
            .filter(|id| server_song(id))
            .cloned()
            .unwrap_or_default();
        if current != self.room.track {
            // Everybody starts it together, this device too.
            if !current.is_empty() && self.player.state() == PlayState::Playing {
                self.player.pause(host);
            }
            self.room_post(host, &current, 0.0, true);
            return;
        }
        // The same song once more (on repeat): for everybody.
        let over = self.player.duration().is_some_and(|d| {
            self.room.phase == "playing"
                && self.room.start_at.is_none()
                && self.room_position(host) > d + 0.5
        });
        if over && !current.is_empty() {
            let position = self.player.position(host);
            self.room_post(host, &current, position, true);
            return;
        }
        let upcoming = self.room_upcoming();
        if upcoming != self.room.sent_upcoming {
            self.room.sent_upcoming = upcoming.clone();
            let body = Value::object().with("upcoming", upcoming);
            self.room_send(host, "POST", "/state", body, RoomReq::Quiet);
        }
    }

    /// Is the room's song loaded here?
    fn room_has_song(&self) -> bool {
        self.queue.current() == Some(&self.room.track)
            && matches!(self.player.state(), PlayState::Paused | PlayState::Playing)
    }

    /// With some of it ready to sound?
    fn room_song_ready(&self) -> bool {
        let rate = self.player.device_rate().max(1) as f64;
        self.room_has_song()
            && (!self.player.can_nudge()
                || self.player.queued_frames() as f64 >= READY_AUDIO * rate)
    }

    /// Every device: do what the room says.
    fn room_follow(&mut self, host: &mut dyn Host) {
        let change = (self.room.gen, self.room.phase.clone(), self.room.at as u64);
        if self.room.track.is_empty() {
            // Nothing to hear together (the host's own song stays the host's).
            if self.room.applied != change {
                self.room.applied = change;
                self.room.start_at = None;
                self.room.drift_ms = None;
                if !self.room_host() {
                    self.player.stop(host);
                }
            }
            return;
        }
        // A member's queue is just the room's song. If it cannot be loaded, it
        // gets another chance in a moment.
        if !self.room_host()
            && self.queue.current() != Some(&self.room.track)
            && host.now() >= self.room.load_at
        {
            self.room.load_at = host.now() + RELOAD;
            let track = self.room.track.clone();
            let shuffle = self.queue.shuffle;
            self.queue.shuffle = false;
            self.queue.replace(vec![track], 0, &mut self.rng);
            self.queue.shuffle = shuffle;
            self.play_current(host, false);
        }
        if self.room.phase == "preparing"
            && self.room.ready_sent != self.room.gen
            && self.room_song_ready()
        {
            self.room.ready_sent = self.room.gen;
            let body = Value::object().with("gen", self.room.gen);
            self.room_send(host, "POST", "/ready", body, RoomReq::Quiet);
        }
        if self.room.applied != change {
            self.room.applied = change;
            self.room.start_at = None;
            self.room.drift_ms = None;
            match self.room.phase.as_str() {
                "paused" => {
                    self.player.pause(host);
                    self.player.seek(host, self.room.position);
                }
                "playing" => self.room.start_at = Some((self.room.at, self.room.position)),
                _ => {}
            }
        }
        // Nothing sounds before its time (a song that finished loading would).
        if (self.room.phase != "playing" || self.room.start_at.is_some())
            && self.player.state() == PlayState::Playing
        {
            self.player.pause(host);
        }
        // The common start: late ones go in where the others are by then.
        if let Some((at, position)) = self.room.start_at {
            let late = self.server_now_ms(host) - at;
            if late >= 0.0 && self.room_has_song() {
                self.room.start_at = None;
                self.player.seek(host, position + late / 1000.0);
                self.player.play(host);
                self.room.check_at = host.now() + SETTLE;
            }
            return;
        }
        if self.room.phase == "playing" && host.now() >= self.room.check_at {
            self.room.check_at = host.now() + CHECK;
            let expected = self.room_position(host);
            // At the end there is nothing to catch up with.
            if self.player.duration().is_none_or(|d| expected < d - 0.5) {
                self.room.drift_ms = self.keep_in_step(host, expected).or(self.room.drift_ms);
            }
        }
    }

    /// Corrects playback towards `expected` and says how far off it was (ms).
    pub(crate) fn keep_in_step(&mut self, host: &mut dyn Host, expected: f64) -> Option<f64> {
        if self.player.state() != PlayState::Playing {
            return None;
        }
        let ahead = self.player.position(host) - expected;
        match correction(ahead, self.player.can_nudge()) {
            Correction::Nothing => {}
            Correction::Nudge(step) => self.player.nudge(step),
            Correction::Seek => {
                self.player.seek(host, expected);
                self.room.check_at = host.now() + SETTLE;
                self.devices.group_check_at = host.now() + SETTLE;
            }
        }
        Some(ahead * 1000.0)
    }

    pub(crate) fn poll_room(&mut self, host: &mut dyn Host) {
        if !self.in_room() {
            let reachable = matches!(
                self.server.state,
                ServerState::Connected(_) | ServerState::NeedsLogin(_)
            );
            // A whole link says where to go; a code waits for the server.
            let goes = self.room.pending.as_ref().is_some_and(|p| {
                reachable || p.contains("://") || self.server.state == ServerState::Disabled
            });
            if goes && !self.room.entering {
                if let Some(code) = self.room.pending.take() {
                    self.room_join(host, &code);
                }
            }
            return;
        }
        let now = host.now();
        if now >= self.room.beat_at {
            self.room.beat_at = now + BEAT;
            let mut body = Value::object();
            if let Some(d) = self.room.drift_ms {
                body = body.with("driftMs", (d * 10.0).round() / 10.0);
            }
            self.room_send(host, "POST", "/heartbeat", body, RoomReq::Beat);
        }
        if self.room_host() {
            self.room_host_step(host);
        }
        self.room_follow(host);
    }

    /// The room as the server says it is now.
    pub(crate) fn room_apply(&mut self, host: &mut dyn Host, v: &Value) {
        if v.str_or("id", "") != self.room.id {
            return;
        }
        let is_member = v
            .get("members")
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x.str_or("id", "") == self.room.me));
        if !is_member {
            let msg = self.tr("You are no longer in the room").to_string();
            self.room_left(host, Some(msg));
            return;
        }
        let track = v.get("track");
        for t in std::iter::once(track).chain(v.get("upcoming").as_array().into_iter().flatten()) {
            let id = t.str_or("id", "");
            if !id.is_empty() && self.library.track(id).is_none() {
                self.extra_tracks
                    .insert(id.to_string(), Track::from_json(t));
            }
        }
        let r = &mut self.room;
        r.code = v.str_or("code", "").to_string();
        r.host_member = v.str_or("host", "").to_string();
        r.guests = v.bool_or("guests", false);
        r.gen = v.get("gen").as_u64().unwrap_or(0);
        r.phase = v.str_or("phase", "idle").to_string();
        r.track = track.str_or("id", "").to_string();
        r.position = v.f64_or("position", 0.0);
        r.at = v.f64_or("at", 0.0);
        r.members = v.get("members").clone();
        r.upcoming = v.get("upcoming").as_array().cloned().unwrap_or_default();
        self.revs.queue += 1;
    }

    /// Out of the room; `why` if it was not this device's choice.
    pub(crate) fn room_left(&mut self, host: &mut dyn Host, why: Option<String>) {
        let guest_login = self.room.guest_login;
        let guest = self.room.guest;
        self.room = Default::default();
        if let Some(msg) = why {
            self.toast(msg, false);
        }
        if guest {
            // A guest had this song through the room only.
            self.player.stop(host);
            self.queue.clear();
            self.current = None;
            self.revs.queue += 1;
        }
        if guest_login {
            self.server.token.clear();
            self.connect_server(host);
        }
    }

    pub(crate) fn room_event(&mut self, host: &mut dyn Host, e: &Value) {
        if !self.in_room() {
            return;
        }
        match e.str_or("type", "") {
            "room" => self.room_apply(host, e.get("room")),
            "room.ended" if e.str_or("room", "") == self.room.id => {
                let msg = self.tr("The room has ended").to_string();
                self.room_left(host, Some(msg));
            }
            _ => {}
        }
    }

    pub(crate) fn room_http(
        &mut self,
        host: &mut dyn Host,
        r: RoomReq,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        let status = res.as_ref().map(|r| r.status).unwrap_or(0);
        let json = match res {
            Ok(r) if (200..300).contains(&r.status) => bl_json::parse_bytes(&r.body).ok(),
            _ => None,
        };
        match r {
            RoomReq::Entered => {
                self.room.entering = false;
                let Some(v) = json else {
                    self.room.error = self
                        .tr(match status {
                            0 => "The server cannot be reached",
                            404 => "There is no room with this code",
                            403 => "This room takes no guests: log in first",
                            429 => "Too many attempts; try again later",
                            401 => "Log in to this server first",
                            _ => "The room could not be opened",
                        })
                        .to_string();
                    return;
                };
                let room = v.get("room");
                self.room = RoomState {
                    id: room.str_or("id", "").to_string(),
                    me: v.str_or("me", "").to_string(),
                    token: v.str_or("token", "").to_string(),
                    guest: v.bool_or("guest", false),
                    ..Default::default()
                };
                self.room_apply(host, room);
                self.devices.clock.next_at = 0.0;
                // Songs need a login: this room is all a guest has here.
                if self.room.guest && matches!(self.server.state, ServerState::NeedsLogin(_)) {
                    self.room.guest_login = true;
                    self.server.token = self.room.token.clone();
                    self.connect_server(host);
                }
            }
            RoomReq::State => {
                self.room.posting = false;
                if let Some((track, position, playing)) = self.room.waiting.take() {
                    self.room_post(host, &track, position, playing);
                    return;
                }
                self.room.chosen = None;
                match json {
                    Some(v) => self.room_apply(host, &v),
                    None if status == 404 => {
                        // Not a song of this server after all: not for the room.
                        let msg = self.tr("Only songs of the server can be in a room");
                        self.toast(msg.to_string(), true);
                    }
                    None => {}
                }
            }
            RoomReq::Beat => match json {
                Some(v) => {
                    self.room.members = v.get("members").clone();
                }
                None if status == 401 || status == 404 => {
                    let msg = self.tr("The room has ended").to_string();
                    self.room_left(host, Some(msg));
                }
                None => {}
            },
            RoomReq::Settings => {
                if let Some(v) = json {
                    self.room_apply(host, &v);
                }
            }
            RoomReq::Quiet => {}
        }
    }

    pub(crate) fn room_json(&self) -> Value {
        let r = &self.room;
        if !self.in_room() {
            return Value::object()
                .with("active", false)
                .with("entering", r.entering || r.pending.is_some())
                .with("error", r.error.as_str());
        }
        let base = match self.launch.origin.as_deref() {
            Some(o) if self.server.base.is_empty() => o,
            _ => self.server.base.as_str(),
        };
        Value::object()
            .with("active", true)
            .with("code", r.code.as_str())
            .with("link", format!("{base}/?room={}", r.code))
            .with("host", self.room_host())
            .with("guest", r.guest)
            .with("guests", r.guests)
            .with("phase", r.phase.as_str())
            .with("at", r.at)
            .with("members", r.members.clone())
            .with("me", r.me.as_str())
            .with("upcoming", Value::Array(r.upcoming.clone()))
            .with("driftMs", r.drift_ms)
            .with("roundTripMs", self.devices.clock.round_trip())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invitation_is_a_code_or_a_link() {
        assert_eq!(
            parse_invitation(" abcd-efgh "),
            (None, "abcd-efgh".to_string())
        );
        assert_eq!(
            parse_invitation("https://music.example.org/?room=ABCDEFGH"),
            (
                Some("https://music.example.org".to_string()),
                "ABCDEFGH".to_string()
            )
        );
        assert_eq!(
            parse_invitation("http://10.0.0.2:7878/beat/?x=1&room=ABCD&y=2#z"),
            (
                Some("http://10.0.0.2:7878/beat/?x=1".to_string()),
                "ABCD".to_string()
            )
        );
        // No address to go to.
        assert_eq!(parse_invitation("?room=ABCD"), (None, "ABCD".to_string()));
    }
}
