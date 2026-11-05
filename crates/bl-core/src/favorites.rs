//! Favorite songs. Kept on the device (`favorites.json`); favorites among
//! the server's songs also live with the login on the server, so all its
//! devices share them. Marks set while the server is away wait in `pending`.
//! Songs of other kinds (local files, encrypted files) never leave the
//! device.

use crate::core::{Core, Req};
use crate::host::{Host, HttpResponse};
use crate::library;
use bl_json::Value;
use bl_net::api::{paths, Playlist, PlaylistKind};
use std::collections::HashSet;

const STORAGE: &str = "favorites.json";
/// Seconds between looks at what other devices marked.
const REFRESH: f64 = 300.0;

/// Id of the playlist the favorites show up as.
pub(crate) const PLAYLIST_ID: &str = "favorites";

pub(crate) struct FavoritesState {
    /// Newest first: (song, when it was marked).
    items: Vec<(String, u64)>,
    set: HashSet<String>,
    /// Server songs not yet told to the server: (song, marked, when).
    pending: Vec<(String, bool, u64)>,
    /// The favorites as a playlist (`items` without the times).
    playlist: Playlist,
    /// A change is on its way to the server.
    sending: bool,
    fetching: bool,
    fetched_at: Option<f64>,
    /// The server's name, to fetch again after connecting.
    server_seen: bool,
}

impl Default for FavoritesState {
    fn default() -> Self {
        FavoritesState {
            items: Vec::new(),
            set: HashSet::new(),
            pending: Vec::new(),
            playlist: Playlist {
                id: PLAYLIST_ID.to_string(),
                name: "Favorites".to_string(),
                kind: PlaylistKind::Track,
                items: Vec::new(),
                modified: 0,
            },
            sending: false,
            fetching: false,
            fetched_at: None,
            server_seen: false,
        }
    }
}

impl FavoritesState {
    pub fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.items.iter().map(|(id, _)| id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn rebuilt(&mut self) {
        self.items.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        self.set = self.items.iter().map(|(id, _)| id.clone()).collect();
        self.playlist.items = self.items.iter().map(|(id, _)| id.clone()).collect();
    }

    pub(crate) fn load(data: &[u8]) -> FavoritesState {
        let mut s = FavoritesState::default();
        let Ok(v) = bl_json::parse_bytes(data) else {
            return s;
        };
        let mut seen = HashSet::new();
        for i in v.get("items").as_array().into_iter().flatten() {
            let id = i.str_or("id", "");
            if !id.is_empty() && seen.insert(id.to_string()) {
                s.items
                    .push((id.to_string(), i.get("t").as_u64().unwrap_or(0)));
            }
        }
        for p in v.get("pending").as_array().into_iter().flatten() {
            let id = p.str_or("id", "");
            if library::is_server_id(id) {
                s.pending.push((
                    id.to_string(),
                    p.bool_or("on", true),
                    p.get("t").as_u64().unwrap_or(0),
                ));
            }
        }
        s.rebuilt();
        s
    }

    fn to_json(&self) -> Value {
        Value::object()
            .with(
                "items",
                Value::Array(
                    self.items
                        .iter()
                        .map(|(id, t)| Value::object().with("id", id.as_str()).with("t", *t))
                        .collect(),
                ),
            )
            .with(
                "pending",
                Value::Array(
                    self.pending
                        .iter()
                        .map(|(id, on, t)| {
                            Value::object()
                                .with("id", id.as_str())
                                .with("on", *on)
                                .with("t", *t)
                        })
                        .collect(),
                ),
            )
    }

    /// Takes the server's list for its songs, with what is still to be sent
    /// on top; everything else stays as it is.
    fn merge_server(&mut self, server: Vec<(String, u64)>) {
        let mut items: Vec<(String, u64)> = self
            .items
            .drain(..)
            .filter(|(id, _)| !library::is_server_id(id))
            .collect();
        items.extend(server);
        for (id, on, t) in &self.pending {
            items.retain(|(i, _)| i != id);
            if *on {
                items.push((id.clone(), *t));
            }
        }
        self.items = items;
        self.rebuilt();
    }
}

impl Core {
    pub(crate) fn is_favorite(&self, id: &str) -> bool {
        self.favorites.contains(id)
    }

    fn save_favorites(&mut self, host: &mut dyn Host) {
        host.storage_write(STORAGE, self.favorites.to_json().to_json().as_bytes());
        self.revs.favorites += 1;
        self.revs.playlists += 1;
    }

    pub(crate) fn load_favorites(&mut self, host: &dyn Host) {
        if let Some(d) = host.storage_read(STORAGE) {
            self.favorites = FavoritesState::load(&d);
        }
    }

    pub(crate) fn set_favorite(&mut self, host: &mut dyn Host, id: &str, on: bool) {
        if id.is_empty() || self.favorites.contains(id) == on {
            return;
        }
        let now = host.unix_time() as u64;
        if on {
            self.favorites.items.push((id.to_string(), now));
        } else {
            self.favorites.items.retain(|(i, _)| i != id);
        }
        self.favorites.rebuilt();
        if library::is_server_id(id) {
            self.favorites.pending.retain(|(i, _, _)| i != id);
            self.favorites.pending.push((id.to_string(), on, now));
        }
        self.save_favorites(host);
        self.send_favorites(host);
    }

    /// The favorites playlist was edited: makes the list match `items`
    /// (a new song on top, order otherwise by when they were marked).
    pub(crate) fn favorites_replace(&mut self, host: &mut dyn Host, items: &[String]) {
        let old: Vec<String> = self.favorites.ids().cloned().collect();
        for id in old.iter().filter(|id| !items.contains(id)) {
            self.set_favorite(host, id, false);
        }
        for id in items.iter().filter(|id| !old.contains(id)) {
            self.set_favorite(host, id, true);
        }
    }

    fn send_favorites(&mut self, host: &mut dyn Host) {
        if self.favorites.sending || !self.server.is_connected() {
            return;
        }
        let Some((id, on, t)) = self.favorites.pending.first().cloned() else {
            return;
        };
        self.favorites.sending = true;
        let rid = if on {
            self.server.send(
                host,
                "PUT",
                &paths::favorite(&id),
                Value::object().with("t", t).to_json().into_bytes(),
                "application/json",
            )
        } else {
            self.server.send(
                host,
                "DELETE",
                &paths::favorite(&id),
                Vec::new(),
                "application/json",
            )
        };
        self.requests.insert(rid, Req::FavoriteSaved);
    }

    pub(crate) fn favorite_saved(
        &mut self,
        host: &mut dyn Host,
        res: &Result<HttpResponse, String>,
    ) {
        self.favorites.sending = false;
        match res {
            Ok(r) if (200..300).contains(&r.status) || (400..500).contains(&r.status) => {
                // Done, or refused for good (then it is not tried again).
                if !self.favorites.pending.is_empty() {
                    self.favorites.pending.remove(0);
                }
                self.save_favorites(host);
                self.send_favorites(host);
                if self.favorites.pending.is_empty() {
                    self.favorites.fetched_at = None;
                }
            }
            // The server is away: tried again when it is back.
            _ => self.favorites.fetched_at = Some(self.time),
        }
    }

    pub(crate) fn favorites_fetched(
        &mut self,
        host: &mut dyn Host,
        res: &Result<HttpResponse, String>,
    ) {
        self.favorites.fetching = false;
        let Ok(r) = res else {
            return;
        };
        if r.status != 200 {
            return;
        }
        let Ok(v) = bl_json::parse_bytes(&r.body) else {
            return;
        };
        let server = v
            .get("items")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| {
                let id = i.get("id").as_str().filter(|s| library::is_server_id(s))?;
                Some((id.to_string(), i.get("t").as_u64().unwrap_or(0)))
            })
            .collect();
        let before = self.favorites.items.clone();
        self.favorites.merge_server(server);
        if self.favorites.items != before {
            self.save_favorites(host);
        }
    }

    pub(crate) fn poll_favorites(&mut self, host: &mut dyn Host) {
        if !self.server.is_connected() {
            self.favorites.server_seen = false;
            return;
        }
        if !self.favorites.server_seen {
            self.favorites.server_seen = true;
            self.favorites.fetched_at = None;
            self.favorites.sending = false;
            self.favorites.fetching = false;
            self.send_favorites(host);
        }
        if !self.favorites.pending.is_empty() {
            if !self.favorites.sending
                && self
                    .favorites
                    .fetched_at
                    .is_none_or(|t| self.time - t > REFRESH)
            {
                self.send_favorites(host);
            }
            return;
        }
        if self.favorites.fetching || self.favorites.sending {
            return;
        }
        if self
            .favorites
            .fetched_at
            .is_none_or(|t| self.time - t > REFRESH)
        {
            self.favorites.fetched_at = Some(self.time);
            self.favorites.fetching = true;
            let rid = self.server.get(host, paths::FAVORITES);
            self.requests.insert(rid, Req::Favorites);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_list_replaces_server_songs_only() {
        let mut s = FavoritesState::default();
        s.items = vec![
            ("file:/music/a.mp3".into(), 5),
            ("00000000000000aa".into(), 4),
            ("00000000000000bb".into(), 3),
        ];
        s.pending = vec![("00000000000000cc".into(), true, 9)];
        s.merge_server(vec![
            ("00000000000000bb".into(), 3),
            ("00000000000000dd".into(), 7),
        ]);
        let ids: Vec<&str> = s.ids().map(String::as_str).collect();
        assert_eq!(
            ids,
            [
                "00000000000000cc",
                "00000000000000dd",
                "file:/music/a.mp3",
                "00000000000000bb"
            ]
        );
    }

    #[test]
    fn a_removal_still_to_be_sent_wins_over_the_server() {
        let mut s = FavoritesState::default();
        s.pending = vec![("00000000000000aa".into(), false, 9)];
        s.merge_server(vec![("00000000000000aa".into(), 3)]);
        assert!(!s.contains("00000000000000aa"));
    }

    #[test]
    fn stored_and_loaded() {
        let mut s = FavoritesState::default();
        s.items = vec![("file:/music/a.mp3".into(), 5)];
        s.pending = vec![("00000000000000aa".into(), true, 6)];
        s.rebuilt();
        let again = FavoritesState::load(s.to_json().to_json().as_bytes());
        assert!(again.contains("file:/music/a.mp3"));
        assert_eq!(again.pending.len(), 1);
        assert_eq!(again.playlist().items, ["file:/music/a.mp3"]);
    }
}
