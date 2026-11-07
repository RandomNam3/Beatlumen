//! What an account took out of its view of the shared library. The file stays
//! for the others (see docs/ENCRYPTION.md and `extensions.rs` for when a song of
//! an extension goes away); for this account it is in the trash for a while,
//! and hidden after that as well.

use bl_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

pub struct Hidden {
    path: PathBuf,
    /// Track id and when it was removed.
    list: RwLock<HashMap<String, u64>>,
}

impl Hidden {
    pub fn load(path: PathBuf) -> Hidden {
        let mut list = HashMap::new();
        if let Some(o) = std::fs::read(&path)
            .ok()
            .and_then(|d| bl_json::parse_bytes(&d).ok())
        {
            for (id, at) in o.as_object().into_iter().flat_map(|m| m.iter()) {
                if let Some(at) = at.as_u64() {
                    list.insert(id.to_string(), at);
                }
            }
        }
        Hidden {
            path,
            list: RwLock::new(list),
        }
    }

    fn save(&self, list: &HashMap<String, u64>) {
        let mut o = Value::object();
        for (id, at) in list {
            o = o.with(id, *at);
        }
        if let Err(e) = crate::util::write_atomic(&self.path, o.to_json().as_bytes()) {
            crate::warn!("cannot write {}: {e}", self.path.display());
        }
    }

    pub fn add(&self, id: &str, now: u64) {
        let mut l = self.list.write().unwrap_or_else(|e| e.into_inner());
        l.insert(id.to_string(), now);
        self.save(&l);
    }

    /// Whether it was hidden.
    pub fn remove(&self, id: &str) -> bool {
        let mut l = self.list.write().unwrap_or_else(|e| e.into_inner());
        let had = l.remove(id).is_some();
        if had {
            self.save(&l);
        }
        had
    }

    /// When it was removed, if it was.
    pub fn removed_at(&self, id: &str) -> Option<u64> {
        let l = self.list.read().unwrap_or_else(|e| e.into_inner());
        l.get(id).copied()
    }

    /// All of it: (track id, removed at).
    pub fn all(&self) -> Vec<(String, u64)> {
        let l = self.list.read().unwrap_or_else(|e| e.into_inner());
        l.iter().map(|(id, at)| (id.clone(), *at)).collect()
    }
}
