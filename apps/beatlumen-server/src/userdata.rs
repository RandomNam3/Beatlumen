//! Playlists and presets of user accounts. Requests signed in with a user
//! account use that user's own; the server password signs in to the shared
//! ones.

use crate::playlists::Playlists;
use crate::presets::Presets;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct UserStore {
    pub playlists: Playlists,
    pub presets: Presets,
}

#[derive(Default)]
pub struct UserData {
    stores: Mutex<HashMap<PathBuf, Arc<UserStore>>>,
}

impl UserData {
    /// The store in `dir` (see `Vault::user_data_dir`), opened on first use.
    pub fn get(&self, dir: PathBuf) -> Arc<UserStore> {
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = stores.get(&dir) {
            if dir.exists() {
                return s.clone();
            }
        }
        let presets = dir.join("presets");
        if let Err(e) = std::fs::create_dir_all(&presets) {
            crate::warn!("cannot create {}: {e}", presets.display());
        }
        let s = Arc::new(UserStore {
            playlists: Playlists::load(dir.join("playlists.json")),
            presets: Presets::new(presets),
        });
        stores.insert(dir, s.clone());
        s
    }
}
