//! What a login listened to: one database entry per play
//! (`<scope>history/<start, 16 hex digits>/<song id>`: `{ms, skip}`).
//! Only there when the user agreed to it in the app (the apps send nothing
//! otherwise), and only songs of this server's library; the user can delete
//! it at any time.

use crate::store::Part;
use crate::util;
use bl_json::Value;
use std::collections::HashSet;
use std::sync::Mutex;

/// Plays kept per login; the oldest go first.
const MAX_EVENTS: usize = 200_000;
/// Plays in one request.
const MAX_BATCH: usize = 1000;
/// Longest play taken as true (a day).
const MAX_MS: u64 = 86_400_000;

pub type Failure = (u16, String);

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    /// When it started (seconds since the Unix epoch).
    pub t: u64,
    /// How long it was heard.
    pub ms: u64,
    /// Left early.
    pub skip: bool,
}

impl Event {
    fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("t", self.t)
            .with("ms", self.ms)
            .with("skip", self.skip)
    }

    /// Its key in the database: in the order of the start times.
    fn key(&self) -> String {
        format!("{:016x}/{}", self.t, self.id)
    }

    fn from_stored(key: &str, v: &Value, now: u64) -> Option<Event> {
        let (t, id) = key.split_once('/')?;
        let t = u64::from_str_radix(t, 16).ok()?;
        Event::from_json(&v.clone().with("id", id).with("t", t), now)
    }

    fn stored(&self) -> Value {
        Value::object().with("ms", self.ms).with("skip", self.skip)
    }

    fn from_json(v: &Value, now: u64) -> Option<Event> {
        let id = v.get("id").as_str().filter(|s| util::is_id(s))?;
        let t = v.get("t").as_u64().filter(|t| *t <= now + 86_400)?;
        Some(Event {
            id: id.to_string(),
            t,
            ms: v.get("ms").as_u64().unwrap_or(0).min(MAX_MS),
            skip: v.bool_or("skip", false),
        })
    }
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    events: Vec<Event>,
    /// A device repeats plays when the answer got lost: (song, start).
    seen: HashSet<(String, u64)>,
}

pub struct History {
    part: Part,
    inner: Mutex<Inner>,
}

fn failed(e: crate::store::Error) -> Failure {
    crate::error!("cannot save the listening history: {e}");
    (500, "cannot save the listening history".to_string())
}

impl History {
    pub fn load(part: Part) -> History {
        let mut inner = Inner::default();
        let now = util::now_secs();
        for (key, v) in part.list_json() {
            if let Some(e) = Event::from_stored(&key, &v, now) {
                if inner.seen.insert((e.id.clone(), e.t)) {
                    inner.events.push(e);
                }
            }
        }
        inner.events.sort_by_key(|e| e.t);
        History {
            part,
            inner: Mutex::new(inner),
        }
    }

    /// `POST /api/history` body `{events: [{id, t, ms, skip}]}`; the number
    /// of new plays. Entries that make no sense are left out.
    pub fn add(&self, body: &[u8]) -> Result<usize, Failure> {
        let v = bl_json::parse_bytes(body).map_err(|e| (400, format!("invalid JSON: {e}")))?;
        let list = v
            .get("events")
            .as_array()
            .ok_or((400, "events must be an array".to_string()))?;
        if list.len() > MAX_BATCH {
            return Err((413, "too many events".into()));
        }
        let now = util::now_secs();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut fresh = Vec::new();
        for e in list.iter().filter_map(|e| Event::from_json(e, now)) {
            if inner.seen.insert((e.id.clone(), e.t)) {
                fresh.push(e);
            }
        }
        if fresh.is_empty() {
            return Ok(0);
        }
        // The oldest go when there are too many (together with the new ones).
        let mut all: Vec<&Event> = inner.events.iter().chain(&fresh).collect();
        all.sort_by_key(|e| e.t);
        let drop = if all.len() > MAX_EVENTS {
            all.len() - MAX_EVENTS * 9 / 10
        } else {
            0
        };
        let gone: Vec<String> = all[..drop].iter().map(|e| e.key()).collect();
        let written = self.part.store().write(|tx| {
            for e in &fresh {
                tx.put_json(&self.part.key(&e.key()), &e.stored())?;
            }
            for k in &gone {
                tx.delete(&self.part.key(k))?;
            }
            Ok(())
        });
        if let Err(e) = written {
            for e in &fresh {
                inner.seen.remove(&(e.id.clone(), e.t));
            }
            return Err(failed(e));
        }
        let n = fresh.len();
        inner.events.extend(fresh);
        inner.events.sort_by_key(|e| e.t);
        if drop > 0 {
            let gone: Vec<Event> = inner.events.drain(..drop).collect();
            for e in gone {
                inner.seen.remove(&(e.id, e.t));
            }
        }
        Ok(n)
    }

    /// All plays, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .events
            .clone()
    }

    /// `GET /api/history?since=&limit=`: the newest plays, newest first.
    pub fn list_json(&self, since: u64, limit: usize) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Value::object().with(
            "events",
            Value::Array(
                inner
                    .events
                    .iter()
                    .rev()
                    .take_while(|e| e.t >= since)
                    .take(limit)
                    .map(Event::to_json)
                    .collect(),
            ),
        )
    }

    /// `DELETE /api/history`.
    pub fn clear(&self) -> Result<(), Failure> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = self.part.clear() {
            crate::error!("cannot delete the listening history: {e}");
            return Err((500, "cannot delete the listening history".to_string()));
        }
        *inner = Inner::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Part {
        Part::new(
            &crate::store::temp_store(&format!("hist-{name}")),
            "shared/history/",
        )
    }

    const A: &str = "00000000000000aa";
    const B: &str = "00000000000000bb";

    fn body(events: &[(&str, u64, u64, bool)]) -> Vec<u8> {
        Value::object()
            .with(
                "events",
                Value::Array(
                    events
                        .iter()
                        .map(|(id, t, ms, skip)| {
                            Value::object()
                                .with("id", *id)
                                .with("t", *t)
                                .with("ms", *ms)
                                .with("skip", *skip)
                        })
                        .collect(),
                ),
            )
            .to_json()
            .into_bytes()
    }

    #[test]
    fn plays_are_kept_once_and_over_restart() {
        let p = temp("keep");
        let h = History::load(p.clone());
        assert_eq!(
            h.add(&body(&[(A, 100, 1000, false), (B, 50, 2000, true)]))
                .unwrap(),
            2
        );
        // The answer got lost and the device sends them again.
        assert_eq!(h.add(&body(&[(A, 100, 1000, false)])).unwrap(), 0);
        let again = History::load(p);
        let ev = again.events();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].id, B);
        assert!(ev[0].skip);
    }

    #[test]
    fn nonsense_is_left_out() {
        let h = History::load(temp("bad"));
        let n = h
            .add(&body(&[
                ("file:/home/me/secret.mp3", 100, 1, false),
                ("vault:abc", 100, 1, false),
                (A, 90_000_000_000, 1, false),
                (B, 100, 900_000_000_000, false),
            ]))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(h.events()[0].ms, MAX_MS);
        assert_eq!(h.add(b"{\"events\":3}").unwrap_err().0, 400);
    }

    #[test]
    fn clear_removes_everything() {
        let p = temp("clear");
        let h = History::load(p.clone());
        h.add(&body(&[(A, 100, 1, false)])).unwrap();
        h.clear().unwrap();
        assert!(h.events().is_empty());
        assert!(p.list().is_empty());
        assert!(History::load(p).events().is_empty());
        // Deleted plays may come back from a device that sends new ones.
        assert_eq!(h.add(&body(&[(A, 100, 1, false)])).unwrap(), 1);
    }

    #[test]
    fn newest_first_with_limit() {
        let h = History::load(temp("list"));
        h.add(&body(&[
            (A, 1, 1, false),
            (A, 2, 1, false),
            (B, 3, 1, false),
        ]))
        .unwrap();
        let v = h.list_json(2, 1);
        let ev = v.get("events").as_array().unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].str_or("id", ""), B);
        assert_eq!(
            h.list_json(2, 10).get("events").as_array().unwrap().len(),
            2
        );
    }
}
