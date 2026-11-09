//! What an account took out of its view of the shared library. The file stays
//! for the others (see docs/ENCRYPTION.md and `extensions.rs` for when a song of
//! an extension goes away); for this account it is in the trash for a while,
//! and hidden after that as well.

use crate::store::Part;
use std::collections::HashMap;
use std::sync::RwLock;

/// One database entry per track (`<scope>hidden/<id>`: when it was removed).
pub struct Hidden {
    part: Part,
    /// Track id and when it was removed.
    list: RwLock<HashMap<String, u64>>,
}

impl Hidden {
    pub fn load(part: Part) -> Hidden {
        let list = part
            .list()
            .into_iter()
            .filter_map(|(id, at)| Some((id, std::str::from_utf8(&at).ok()?.parse().ok()?)))
            .collect();
        Hidden {
            part,
            list: RwLock::new(list),
        }
    }

    pub fn add(&self, id: &str, now: u64) {
        let mut l = self.list.write().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = self.part.put(id, now.to_string().as_bytes()) {
            crate::warn!("cannot save a removed song: {e}");
        }
        l.insert(id.to_string(), now);
    }

    /// Whether it was hidden.
    pub fn remove(&self, id: &str) -> bool {
        let mut l = self.list.write().unwrap_or_else(|e| e.into_inner());
        let had = l.remove(id).is_some();
        if had {
            if let Err(e) = self.part.delete(id) {
                crate::warn!("cannot save a restored song: {e}");
            }
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
