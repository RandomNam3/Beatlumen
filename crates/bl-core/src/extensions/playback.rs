//! Optional provider feedback, independent of personal history sync.
use super::{Core, ExtReq, Host, Req, ID_PREFIX};
use crate::library;
use crate::player::PlayState;
use bl_json::Value;
use std::collections::VecDeque;

struct Play {
    origin: String,
    track: String,
    session: String,
    seq: u64,
    position: f64,
    sent_position: f64,
    state: &'static str,
    sent: f64,
}
#[derive(Default)]
pub(super) struct Feedback {
    current: Option<Play>,
    queue: VecDeque<Value>,
    sending: bool,
    retry: f64,
    serial: u64,
}
impl Feedback {
    fn event(&mut self, state: &'static str, position: f64, observed_at: u64) {
        if let Some(p) = &mut self.current {
            let seq = p.seq;
            p.seq += 1;
            self.queue.push_back(
                Value::object()
                    .with("track", p.track.as_str())
                    .with("session", p.session.as_str())
                    .with("seq", seq)
                    .with("position", position.max(0.0))
                    .with("state", state)
                    .with("observed_at", observed_at),
            );
        }
        if self.queue.len() > 256 {
            self.queue.clear();
            self.current = None;
        }
    }
    pub(super) fn complete(&mut self, session: &str, seq: u64, ok: bool, now: f64) {
        if !self.queue.front().is_some_and(|v| {
            v.str_or("session", "") == session && v.get("seq").as_u64() == Some(seq)
        }) {
            return;
        }
        self.sending = false;
        if ok {
            self.queue.pop_front();
        } else {
            self.retry = now + 5.0;
        }
    }
}
impl Core {
    pub(crate) fn extensions_playback_tick(&mut self, host: &mut dyn Host) {
        if !self.server.is_connected() || !self.ext.list.iter().any(|e| e.playback) {
            return;
        }
        let observation = self
            .current
            .as_ref()
            .filter(|t| self.queue.current() == Some(&t.id))
            .filter(|t| library::is_server_id(&t.id) || t.id.starts_with(ID_PREFIX))
            .filter(|_| !self.playing_airplay() && !self.playing_dlna() && !self.playing_capture())
            .filter(|_| self.player.state() != PlayState::Stopped)
            .map(|t| {
                let origin = if let Some(job) = t.id.strip_prefix(ID_PREFIX) {
                    self.ext
                        .jobs
                        .iter()
                        .find(|j| j.job == job)
                        .map(|j| j.item.clone())
                } else {
                    self.ext
                        .in_library
                        .iter()
                        .find(|(_, id)| *id == &t.id)
                        .map(|(item, _)| item.clone())
                }
                .unwrap_or_else(|| t.id.clone());
                let state = if self.player.state() == PlayState::Playing {
                    "playing"
                } else {
                    "paused"
                };
                (origin, t.id.clone(), self.player.position(host), state)
            });
        let mut f = std::mem::take(&mut self.ext.feedback);
        let observed_at = (host.unix_time() * 1000.0) as u64;
        let same = match (&f.current, &observation) {
            (Some(p), Some((origin, ..))) => &p.origin == origin,
            (None, None) => true,
            _ => false,
        };
        let repeat = match (&f.current, &observation) {
            (Some(p), Some((_, _, pos, state))) => {
                *state == "playing" && *pos < 2.0 && *pos + 5.0 < p.position
            }
            _ => false,
        };
        if !same || repeat {
            if let Some(p) = &f.current {
                let pos = p.position;
                f.event("ended", pos, observed_at);
            }
            f.current = None;
        }
        if let Some((origin, track, pos, state)) = observation {
            if f.current.is_none() && state == "playing" {
                f.serial += 1;
                f.current = Some(Play {
                    origin,
                    track,
                    session: format!(
                        "{:016x}-{:016x}-{}",
                        host.random_seed(),
                        host.unix_time().to_bits(),
                        f.serial
                    ),
                    seq: 0,
                    position: pos,
                    sent_position: pos,
                    state,
                    sent: self.time,
                });
                f.event("start", pos, observed_at);
            } else if let Some(p) = &mut f.current {
                p.track = track;
                let elapsed = (self.time - p.sent).max(0.0);
                let seek = pos < p.position || pos - p.sent_position > elapsed * 1.5 + 0.5;
                let changed = state != p.state;
                if seek {
                    f.event("seeking", pos, observed_at);
                }
                if seek || changed || (state == "playing" && elapsed >= 5.0) {
                    f.event(state, pos, observed_at);
                    if let Some(p) = &mut f.current {
                        p.sent_position = pos;
                        p.state = state;
                        p.sent = self.time;
                    }
                }
                if let Some(p) = &mut f.current {
                    p.position = pos;
                }
            }
        }
        if !f.sending && self.time >= f.retry {
            if let Some(v) = f.queue.front() {
                let rid = self.server.send(
                    host,
                    "POST",
                    "/api/extensions/playback",
                    v.to_json().into_bytes(),
                    "application/json",
                );
                self.requests.insert(
                    rid,
                    Req::Ext(ExtReq::Playback(
                        v.str_or("session", "").to_string(),
                        v.get("seq").as_u64().unwrap_or(0),
                    )),
                );
                f.sending = true;
            }
        }
        self.ext.feedback = f;
    }
}
