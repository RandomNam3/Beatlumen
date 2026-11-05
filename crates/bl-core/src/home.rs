//! What the start page shows.

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use bl_net::api::Track;
use std::collections::HashSet;

/// Items per row.
const SHELF: usize = 24;

/// The name scanners give songs without an album.
const NO_ALBUM: &str = "Unknown Album";

impl Core {
    /// `home.overview`: rows of the start page. Rows without anything in
    /// them are left to the page to hide.
    pub(crate) fn home_overview(&mut self, host: &dyn Host) -> Value {
        let (shelves, mixes) = self.suggestions_json(host);
        Value::object()
            .with("shelves", shelves)
            .with("mixes", mixes)
            .with("recent", self.history_recent(SHELF))
            .with("new", self.newest_items())
            .with(
                "favorites",
                Value::Array(
                    self.favorites
                        .ids()
                        .map(|id| self.track_json(&self.track_for(id)))
                        .take(SHELF)
                        .collect(),
                ),
            )
            .with("favoritesCount", self.favorites.playlist().items.len())
    }

    /// The newest of the library: albums, and songs without one.
    fn newest_items(&self) -> Value {
        let mut newest: Vec<&Track> = self.library.tracks.iter().collect();
        newest.sort_by_key(|t| std::cmp::Reverse(t.modified));
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for t in newest {
            if out.len() >= SHELF {
                break;
            }
            if t.album.is_empty() || t.album == NO_ALBUM {
                out.push(self.track_json(t).with("type", "track"));
            } else if seen.insert(t.album_id.as_str()) {
                if let Some(a) = self.library.album(&t.album_id) {
                    out.push(self.album_json(a).with("type", "album"));
                }
            }
        }
        Value::Array(out)
    }
}
