//! What a login listened to, one JSON line per play in `history.jsonl`.
//! Only there when the user agreed to it in the app (the apps send nothing
//! otherwise), and only songs of this server's library; the user can delete
//! it at any time.

use crate::util;
use bl_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
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
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl History {
    pub fn load(path: PathBuf) -> History {
        let mut inner = Inner::default();
        if let Ok(data) = std::fs::read(&path) {
            let now = util::now_secs();
            for line in data.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                if let Some(e) = bl_json::parse_bytes(line)
                    .ok()
                    .and_then(|v| Event::from_json(&v, now))
                {
                    if inner.seen.insert((e.id.clone(), e.t)) {
                        inner.events.push(e);
                    }
                }
            }
            inner.events.sort_by_key(|e| e.t);
        }
        History {
            path,
            inner: Mutex::new(inner),
        }
    }

    fn rewrite(&self, inner: &Inner) -> Result<(), Failure> {
        let mut out = String::new();
        for e in &inner.events {
            out.push_str(&e.to_json().to_json());
            out.push('\n');
        }
        util::write_atomic(&self.path, out.as_bytes()).map_err(|e| {
            crate::error!("cannot write {}: {e}", self.path.display());
            (500, "cannot save the listening history".to_string())
        })
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
        let mut lines = String::new();
        for e in &fresh {
            lines.push_str(&e.to_json().to_json());
            lines.push('\n');
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(lines.as_bytes()));
        if let Err(e) = written {
            crate::error!("cannot write {}: {e}", self.path.display());
            for e in &fresh {
                inner.seen.remove(&(e.id.clone(), e.t));
            }
            return Err((500, "cannot save the listening history".to_string()));
        }
        let n = fresh.len();
        inner.events.extend(fresh);
        inner.events.sort_by_key(|e| e.t);
        if inner.events.len() > MAX_EVENTS {
            let drop = inner.events.len() - MAX_EVENTS * 9 / 10;
            let gone: Vec<Event> = inner.events.drain(..drop).collect();
            for e in gone {
                inner.seen.remove(&(e.id, e.t));
            }
            self.rewrite(&inner)?;
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
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                crate::error!("cannot delete {}: {e}", self.path.display());
                return Err((500, "cannot delete the listening history".to_string()));
            }
        }
        *inner = Inner::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bl-hist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
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
        assert!(!p.exists());
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
