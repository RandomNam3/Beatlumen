//! Shared playlists (track and background image playlists), stored in
//! `data/playlists.json` and rewritten atomically on every change.

use crate::util;
use oc_json::Value;
use oc_net::api::{Playlist, PlaylistKind};
use std::path::PathBuf;
use std::sync::RwLock;

const MAX_PLAYLISTS: usize = 10_000;
const MAX_ITEMS: usize = 100_000;
const MAX_NAME: usize = 200;

pub type Failure = (u16, String);

pub struct Playlists {
    path: PathBuf,
    list: RwLock<Vec<Playlist>>,
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
            a.iter().map(|x| x.as_str().filter(|s| !s.is_empty() && s.len() <= 64).map(str::to_string).ok_or_else(|| bad("items must be id strings"))).collect::<Result<Vec<_>, _>>().map(Some)
        }
        _ => Err(bad("items must be an array")),
    }
}

impl Playlists {
    pub fn load(path: PathBuf) -> Playlists {
        let mut list = Vec::new();
        if let Ok(data) = std::fs::read(&path) {
            match oc_json::parse_bytes(&data) {
                Ok(v) => {
                    let arr = v.as_array().or_else(|| v.get("playlists").as_array());
                    for p in arr.into_iter().flatten() {
                        let p = Playlist::from_json(p);
                        if util::is_id(&p.id) && !list.iter().any(|q: &Playlist| q.id == p.id) {
                            list.push(p);
                        }
                    }
                }
                Err(e) => crate::warn!("cannot parse {}: {e}; starting with no playlists", path.display()),
            }
        }
        Playlists { path, list: RwLock::new(list) }
    }

    fn save(&self, list: &[Playlist]) -> Result<(), Failure> {
        let v = Value::object().with("playlists", Value::Array(list.iter().map(Playlist::to_json).collect()));
        util::write_atomic(&self.path, v.to_json_pretty().as_bytes()).map_err(|e| {
            crate::error!("cannot write {}: {e}", self.path.display());
            (500, "cannot save playlists".to_string())
        })
    }

    pub fn list_json(&self) -> Value {
        Value::Array(self.list.read().unwrap_or_else(|e| e.into_inner()).iter().map(Playlist::to_json).collect())
    }

    pub fn get(&self, id: &str) -> Option<Playlist> {
        self.list.read().unwrap_or_else(|e| e.into_inner()).iter().find(|p| p.id == id).cloned()
    }

    /// `POST /api/playlists` body `{name, kind, items?}`.
    pub fn create(&self, body: &[u8]) -> Result<Playlist, Failure> {
        let v = oc_json::parse_bytes(body).map_err(|e| bad(&format!("invalid JSON: {e}")))?;
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
        let p = Playlist { id, name, kind, items, modified: util::now_secs() };
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        if list.len() >= MAX_PLAYLISTS {
            return Err((409, "too many playlists".into()));
        }
        list.push(p.clone());
        if let Err(e) = self.save(&list) {
            list.pop();
            return Err(e);
        }
        Ok(p)
    }

    /// `PUT /api/playlists/{id}`: partial update of `name` and/or `items`.
    pub fn update(&self, id: &str, body: &[u8]) -> Result<Playlist, Failure> {
        let v = oc_json::parse_bytes(body).map_err(|e| bad(&format!("invalid JSON: {e}")))?;
        if v.as_object().is_none() {
            return Err(bad("expected a JSON object"));
        }
        let name = parse_name(v.get("name"))?;
        let items = parse_items(v.get("items"))?;
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        let i = list.iter().position(|p| p.id == id).ok_or((404, "playlist not found".to_string()))?;
        let old = list[i].clone();
        if let Some(n) = name {
            list[i].name = n;
        }
        if let Some(it) = items {
            list[i].items = it;
        }
        list[i].modified = util::now_secs();
        if let Err(e) = self.save(&list) {
            list[i] = old;
            return Err(e);
        }
        Ok(list[i].clone())
    }

    pub fn delete(&self, id: &str) -> Result<(), Failure> {
        let mut list = self.list.write().unwrap_or_else(|e| e.into_inner());
        let i = list.iter().position(|p| p.id == id).ok_or((404, "playlist not found".to_string()))?;
        let old = list.remove(i);
        if let Err(e) = self.save(&list) {
            list.insert(i, old);
            return Err(e);
        }
        Ok(())
    }
}
