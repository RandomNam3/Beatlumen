//! Smart playlists: rules kept in the settings (so they follow the account like
//! other settings do), worked out over all songs known here whenever somebody
//! looks.

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use bl_recommend::SmartList;

/// Marks the id of a smart playlist among the playlists.
pub(crate) const PREFIX: &str = "smart:";
const MOST_LISTS: usize = 200;

impl Core {
    pub(crate) fn smart_lists(&self) -> Vec<SmartList> {
        self.settings
            .smart_lists
            .as_array()
            .map(|a| a.iter().map(SmartList::from_json).collect())
            .unwrap_or_default()
    }

    fn smart_store(&mut self, host: &mut dyn Host, lists: Vec<SmartList>) {
        let value = Value::Array(lists.iter().map(SmartList::to_json).collect());
        self.set_setting(host, "smartLists", value);
        self.revs.playlists += 1;
    }

    /// The songs of a list right now.
    pub(crate) fn smart_songs(&mut self, host: &dyn Host, list: &SmartList) -> Vec<String> {
        let catalog = self.song_catalog();
        let now = host.unix_time() as u64;
        let taste = self.local_taste(&catalog, now);
        let favorites = self.favorites.with_times();
        let favorites: Vec<&str> = favorites.iter().map(|(id, _)| id.as_str()).collect();
        list.songs(&catalog, &taste, &favorites, host.random_seed())
    }

    /// A list by the id it has among the playlists.
    pub(crate) fn smart_find(&self, id: &str) -> Option<SmartList> {
        let id = id.strip_prefix(PREFIX)?;
        self.smart_lists().into_iter().find(|l| l.id == id)
    }

    /// Saves a list (a new one when its id is empty); returns its id.
    pub(crate) fn smart_save(&mut self, host: &mut dyn Host, v: &Value) -> Result<String, String> {
        let mut list = SmartList::from_json(v);
        if list.name.is_empty() {
            return Err(self.tr("A smart playlist needs a name").to_string());
        }
        if list.conditions.is_empty() {
            return Err(self
                .tr("A smart playlist needs at least one rule")
                .to_string());
        }
        let mut lists = self.smart_lists();
        if list.id.is_empty() {
            if lists.len() >= MOST_LISTS {
                return Err(self.tr("There are too many smart playlists").to_string());
            }
            list.id = format!("{:016x}", host.random_seed() ^ host.unix_time().to_bits());
        }
        let id = list.id.clone();
        match lists.iter_mut().find(|l| l.id == id) {
            Some(old) => *old = list,
            None => lists.push(list),
        }
        self.smart_store(host, lists);
        Ok(format!("{PREFIX}{id}"))
    }

    pub(crate) fn smart_delete(&mut self, host: &mut dyn Host, id: &str) {
        let id = id.strip_prefix(PREFIX).unwrap_or(id);
        let mut lists = self.smart_lists();
        lists.retain(|l| l.id != id);
        self.smart_store(host, lists);
    }

    /// What the list would hold, for the dialog.
    pub(crate) fn smart_preview(&mut self, host: &dyn Host, v: &Value) -> Value {
        let list = SmartList::from_json(v);
        let songs = self.smart_songs(host, &list);
        Value::object().with("count", songs.len())
    }
}
