//! Playlists, presets, favorites, listening history, labels and removed songs
//! of a login. Requests signed in with a user account use that user's own
//! (`acct/<name>/` in the database); the server password signs in to the
//! shared ones (`shared/`).

use crate::favorites::Favorites;
use crate::hidden::Hidden;
use crate::history::History;
use crate::labels::SongLabels;
use crate::playlists::Playlists;
use crate::presets::Presets;
use crate::store::{Part, Store};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Present while a store is open: an account deleted meanwhile took it with
/// it, and its next use starts empty.
const OPEN_MARK: &str = "open";

pub struct UserStore {
    pub playlists: Playlists,
    pub presets: Presets,
    pub favorites: Favorites,
    pub history: History,
    pub labels: SongLabels,
    pub hidden: Hidden,
}

impl UserStore {
    /// The stores under `scope` (see [`crate::store::scope`]).
    pub fn open(store: &Arc<Store>, scope: &str) -> UserStore {
        let part = |name: &str| Part::new(store, format!("{scope}{name}/"));
        UserStore {
            playlists: Playlists::load(part("playlist")),
            presets: Presets::new(part("preset")),
            favorites: Favorites::load(part("favorite")),
            history: History::load(part("history")),
            labels: SongLabels::load(part("labels")),
            hidden: Hidden::load(part("hidden")),
        }
    }
}

pub struct UserData {
    store: Arc<Store>,
    stores: Mutex<HashMap<String, Arc<UserStore>>>,
}

impl UserData {
    pub fn new(store: Arc<Store>) -> UserData {
        UserData {
            store,
            stores: Mutex::new(HashMap::new()),
        }
    }

    /// The store of an account, opened on first use.
    pub fn get(&self, account: &str) -> Arc<UserStore> {
        let scope = crate::store::scope(account);
        let mark = format!("{scope}{OPEN_MARK}");
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = stores.get(&scope) {
            if self.store.contains(&mark) {
                return s.clone();
            }
        }
        if let Err(e) = self.store.put(&mark, b"") {
            crate::warn!("cannot open the data of {scope}: {e}");
        }
        let s = Arc::new(UserStore::open(&self.store, &scope));
        stores.insert(scope, s.clone());
        s
    }
}
