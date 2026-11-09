//! Playlists of a login (track and background image playlists): one
//! database entry per playlist (`<scope>playlist/<id>`), with its place in the
//! list (`pos`).

use crate::store::Part;
use crate::util;
use bl_json::Value;
use bl_net::api::{Playlist, PlaylistKind};
use std::collections::HashMap;
use std::sync::RwLock;

const MAX_PLAYLISTS: usize = 10_000;
const MAX_ITEMS: usize = 100_000;
const MAX_NAME: usize = 200;

pub type Failure = (u16, String);

#[derive(Default)]
struct Inner {
    list: Vec<Playlist>,
    /// Place of each playlist in the list (kept in the database).
    pos: HashMap<String, u64>,
    next: u64,
}

pub struct Playlists {
    part: Part,
    list: RwLock<Inner>,
}

fn failed(e: crate::store::Error) -> Failure {
    crate::error!("cannot save playlists: {e}");
    (500, "cannot save playlists".to_string())
}

fn bad(msg: &str) -> Failure {
    (400, msg.to_string())
}

fn parse_name(v: &Value) -> Result<Option<String>, Failure> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(bad("name must not be empty"));
            }
            if s.chars().count() > MAX_NAME || s.chars().any(char::is_control) {
                return Err(bad("invalid name"));
            }
            Ok(Some(s.to_string()))
        }
        _ => Err(bad("name must be a string")),
    }
}

fn parse_items(v: &Value) -> Result<Option<Vec<String>>, Failure> {
    match v {
        Value::Null => Ok(None),
        Value::Array(a) => {
            if a.len() > MAX_ITEMS {
                return Err(bad("too many items"));
            }
            a.iter()
                .map(|x| {
                    x.as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 64)
                        .map(str::to_string)
                        .ok_or_else(|| bad("items must be id strings"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        _ => Err(bad("items must be an array")),
    }
}

impl Playlists {
    pub fn load(part: Part) -> Playlists {
        let mut found: Vec<(u64, Playlist)> = part
            .list_json()
            .into_iter()
            .map(|(id, v)| {
                (
                    v.get("pos").as_u64().unwrap_or(0),
                    Playlist::from_json(&v),
                    id,
                )
            })
            .filter(|(_, p, id)| util::is_id(&p.id) && p.id == *id)
            .map(|(pos, p, _)| (pos, p))
            .collect();
        found.sort_by_key(|(pos, _)| *pos);
        let next = found.last().map_or(0, |(pos, _)| pos + 1);
        let inner = Inner {
            pos: found.iter().map(|(pos, p)| (p.id.clone(), *pos)).collect(),
            list: found.into_iter().map(|(_, p)| p).collect(),
            next,
        };
        Playlists {
            part,
            list: RwLock::new(inner),
        }
    }

    fn save(&self, inner: &Inner, p: &Playlist) -> Result<(), Failure> {
        let pos = inner.pos.get(&p.id).copied().unwrap_or(inner.next);
        self.part
            .put_json(&p.id, &p.to_json().with("pos", pos))
            .map_err(failed)
    }

    pub fn list_json(&self) -> Value {
        Value::Array(
            self.list
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .list
                .iter()
                .map(Playlist::to_json)
                .collect(),
        )
    }

    /// The songs of all song playlists (for the recommendations).
    pub fn track_items(&self) -> Vec<Vec<String>> {
        self.list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .list
            .iter()
            .filter(|p| p.kind == PlaylistKind::Track)
            .map(|p| p.items.clone())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Playlist> {
        self.list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .list
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// `POST /api/playlists` body `{name, kind, items?}`.
    pub fn create(&self, body: &[u8]) -> Result<Playlist, Failure> {
        let v = bl_json::parse_bytes(body).map_err(|e| bad(&format!("invalid JSON: {e}")))?;
        if v.as_object().is_none() {
            return Err(bad("expected a JSON object"));
        }
        let name = parse_name(v.get("name"))?.unwrap_or_else(|| "New Playlist".into());
        let kind = match v.get("kind") {
            Value::Null => PlaylistKind::Track,
            Value::String(s) if s == "track" || s == "background" => PlaylistKind::parse(s),
            _ => return Err(bad("kind must be \"track\" or \"background\"")),
        };
        let items = parse_items(v.get("items"))?.unwrap_or_default();
        let id = crate::random::hex(8).map_err(|e| (500, format!("no random source: {e}")))?;
        let p = Playlist {
            id,
            name,
            kind,
            items,
            modified: util::now_secs(),
        };
        let mut inner = self.list.write().unwrap_or_else(|e| e.into_inner());
        if inner.list.len() >= MAX_PLAYLISTS {
            return Err((409, "too many playlists".into()));
        }
        self.save(&inner, &p)?;
        let pos = inner.next;
        inner.pos.insert(p.id.clone(), pos);
        inner.next += 1;
        inner.list.push(p.clone());
        Ok(p)
    }

    /// `PUT /api/playlists/{id}`: partial update of `name` and/or `items`.
    pub fn update(&self, id: &str, body: &[u8]) -> Result<Playlist, Failure> {
        let v = bl_json::parse_bytes(body).map_err(|e| bad(&format!("invalid JSON: {e}")))?;
        if v.as_object().is_none() {
            return Err(bad("expected a JSON object"));
        }
        let name = parse_name(v.get("name"))?;
        let items = parse_items(v.get("items"))?;
        let mut inner = self.list.write().unwrap_or_else(|e| e.into_inner());
        let i = inner
            .list
            .iter()
            .position(|p| p.id == id)
            .ok_or((404, "playlist not found".to_string()))?;
        let mut p = inner.list[i].clone();
        if let Some(n) = name {
            p.name = n;
        }
        if let Some(it) = items {
            p.items = it;
        }
        p.modified = util::now_secs();
        self.save(&inner, &p)?;
        inner.list[i] = p.clone();
        Ok(p)
    }

    pub fn delete(&self, id: &str) -> Result<(), Failure> {
        let mut inner = self.list.write().unwrap_or_else(|e| e.into_inner());
        let i = inner
            .list
            .iter()
            .position(|p| p.id == id)
            .ok_or((404, "playlist not found".to_string()))?;
        self.part.delete(id).map_err(failed)?;
        inner.list.remove(i);
        inner.pos.remove(id);
        Ok(())
    }
}
