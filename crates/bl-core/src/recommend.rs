//! Recommendations, radio, endless playback and labels in the app.
//!
//! Connected to a server, the server decides (it knows how songs sound and
//! what the login listens to across devices). Without one, or for local files,
//! the same algorithm runs here with the tags and this device's history.

use crate::core::{Core, Req};
use crate::host::{Host, HttpResponse};
use crate::library;
use crate::settings::Repeat;
use bl_json::Value;
use bl_net::api::paths;
use bl_recommend::{Catalog, Play, Reason, Seed, Signals, Song, Taste};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const STORAGE: &str = "labels.json";
/// Seconds between looks at the server's suggestions.
const REFRESH: f64 = 600.0;
/// Labels change rarely (seconds).
const LABELS_REFRESH: f64 = 900.0;
/// Songs fetched at once for a radio.
const RADIO_BATCH: usize = 25;
/// More songs are fetched when so few are left.
const QUEUE_LOW: usize = 3;
/// Seconds to wait after no more songs came.
const RETRY: f64 = 60.0;
/// Songs of the queue told to the server as already played.
const MAX_RECENT: usize = 500;

/// A row of suggestions as shown: ids with why.
pub(crate) struct ShelfData {
    pub key: String,
    pub arg: String,
    pub items: Vec<(String, String, String)>,
}

pub(crate) struct MixData {
    pub id: String,
    pub key: String,
    pub arg: String,
    pub items: Vec<String>,
}

struct RadioState {
    seed: Value,
    name: String,
}

#[derive(Default)]
pub(crate) struct RecommendState {
    shelves: Vec<ShelfData>,
    mixes: Vec<MixData>,
    /// From the server (else worked out here).
    from_server: bool,
    fetching: bool,
    fetched_at: Option<f64>,
    /// `favorites` and `history` revisions the shelves were worked out for.
    made_for: Option<(u64, u64, u64)>,
    /// Labels of the server's songs (automatic and given).
    server_labels: HashMap<String, Vec<String>>,
    labels_fetching: bool,
    labels_fetched_at: Option<f64>,
    /// Labels given to songs that are not the server's.
    local_labels: HashMap<String, Vec<String>>,
    catalog: Option<((u64, u64, u64), Arc<Catalog>)>,
    labels_rev: u64,
    radio: Option<RadioState>,
    /// A request for more songs is on its way.
    extending: bool,
    wait_until: f64,
    /// Bumped when `shelves` and `mixes` change.
    pub rev: u64,
    random: u64,
}

/// The seed a command names; `name` tells the user what plays.
fn parse_seed(seed: &Value) -> Option<Seed> {
    if let Some(a) = seed.get("tracks").as_array() {
        let ids: Vec<String> = a
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect();
        return (!ids.is_empty()).then_some(Seed::Songs(ids));
    }
    if let Some(a) = seed.get("artist").as_str() {
        return Some(Seed::Artist(a.to_string()));
    }
    if let Some(a) = seed.get("album").as_str() {
        return Some(Seed::Album(a.to_string()));
    }
    if let Some(l) = seed.get("label").as_str() {
        return Some(Seed::Label(l.to_string()));
    }
    seed.bool_or("taste", false).then_some(Seed::Taste)
}

fn reason_parts(r: &Reason) -> (String, String) {
    match r {
        Reason::Artist(a) => ("artist".to_string(), a.clone()),
        Reason::Genre(g) => ("genre".to_string(), g.clone()),
        Reason::PlayedWith(s) => ("played-with".to_string(), s.clone()),
        Reason::New => ("new".to_string(), String::new()),
        Reason::Forgotten => ("forgotten".to_string(), String::new()),
        Reason::Discover => ("discover".to_string(), String::new()),
    }
}

impl RecommendState {
    pub(crate) fn load_local(&mut self, data: &[u8]) {
        let Ok(v) = bl_json::parse_bytes(data) else {
            return;
        };
        if let Some(o) = v.get("songs").as_object() {
            for (id, names) in o.iter() {
                let list: Vec<String> = names
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect();
                if !list.is_empty() {
                    self.local_labels.insert(id.to_string(), list);
                }
            }
        }
    }
}

/// A name a person may give: as the server wants it.
fn clean_name(s: &str) -> Option<String> {
    let s = s.trim().replace(':', " ");
    let s = s.trim();
    (!s.is_empty() && s.chars().count() <= 40 && !s.chars().any(char::is_control))
        .then(|| s.to_string())
}

impl Core {
    pub(crate) fn load_recommend(&mut self, host: &dyn Host) {
        if let Some(d) = host.storage_read(STORAGE) {
            self.recommend.load_local(&d);
        }
    }

    /// All songs known here, with the labels known here.
    pub(crate) fn song_catalog(&mut self) -> Arc<Catalog> {
        let key = (
            self.library.revision,
            self.recommend.labels_rev,
            self.artists.genres_rev,
        );
        if let Some((k, c)) = &self.recommend.catalog {
            if *k == key {
                return c.clone();
            }
        }
        let songs = self
            .library
            .tracks
            .iter()
            .map(|t| {
                let labels = if library::is_server_id(&t.id) {
                    self.recommend.server_labels.get(&t.id)
                } else {
                    self.recommend.local_labels.get(&t.id)
                };
                let mut tagged = bl_recommend::Tagged::from_text(
                    t.meta.bpm,
                    &t.meta.key,
                    &t.meta.mood,
                    &t.meta.publisher,
                    &t.meta.language,
                    &t.meta.composer,
                    t.meta.rating,
                );
                if let Some(artist) = t.artists.first() {
                    tagged.artist_genres = self.artist_genres(artist).to_vec();
                }
                Song {
                    id: t.id.clone(),
                    artists: t.artists.clone(),
                    album_id: t.album_id.clone(),
                    genre: t.genre.clone(),
                    year: t.meta.original_year.or(t.year),
                    added: t.modified,
                    features: None,
                    labels: labels.cloned().unwrap_or_default(),
                    tagged,
                }
            })
            .collect();
        let c = Arc::new(Catalog::new(songs));
        self.recommend.catalog = Some((key, c.clone()));
        c
    }

    fn local_taste(&self, c: &Catalog, now: u64) -> Taste {
        let plays: Vec<Play> = self
            .history
            .plays
            .iter()
            .map(|p| Play {
                id: self.history.songs[p.song as usize].id.clone(),
                t: p.t,
                skip: p.skip,
            })
            .collect();
        let favorites = self.favorites.with_times();
        let lists: Vec<Vec<String>> = self
            .local_playlists
            .iter()
            .chain(self.server_playlists.iter())
            .filter(|p| p.kind == bl_net::api::PlaylistKind::Track)
            .map(|p| p.items.clone())
            .collect();
        Taste::new(
            c,
            &Signals {
                plays: &plays,
                favorites: &favorites,
                playlists: lists.iter().map(Vec::as_slice).collect(),
                now,
            },
        )
    }

    /// The suggestions without a server, redone when something changed.
    fn refresh_local_suggestions(&mut self, host: &dyn Host) {
        let made_for = (
            self.library.revision,
            self.revs.favorites,
            self.revs.history,
        );
        if self.recommend.made_for == Some(made_for) && !self.recommend.from_server {
            return;
        }
        self.recommend.made_for = Some(made_for);
        self.recommend.from_server = false;
        let c = self.song_catalog();
        let now = host.unix_time() as u64;
        let t = self.local_taste(&c, now);
        let day = now / 86_400;
        self.recommend.shelves = bl_recommend::shelves(&c, &t, day)
            .into_iter()
            .map(|s| ShelfData {
                key: s.key.to_string(),
                arg: s.arg,
                items: s
                    .items
                    .into_iter()
                    .map(|i| {
                        let (k, a) = reason_parts(&i.reason);
                        (i.id, k, a)
                    })
                    .collect(),
            })
            .collect();
        self.recommend.mixes = bl_recommend::mixes(&c, &t, day)
            .into_iter()
            .map(|m| MixData {
                id: m.id,
                key: m.key.to_string(),
                arg: m.arg,
                items: m.items,
            })
            .collect();
        self.recommend.rev += 1;
    }

    pub(crate) fn poll_recommend(&mut self, host: &mut dyn Host) {
        self.extend_queue(host);
        if !self.server.is_connected() {
            self.recommend.fetched_at = None;
            self.recommend.labels_fetched_at = None;
            self.recommend.fetching = false;
            self.recommend.labels_fetching = false;
            if !self.recommend.server_labels.is_empty() {
                self.recommend.server_labels.clear();
                self.recommend.labels_rev += 1;
                self.revs.labels += 1;
            }
            return;
        }
        if !self.recommend.fetching
            && self
                .recommend
                .fetched_at
                .is_none_or(|t| self.time - t > REFRESH)
        {
            self.recommend.fetched_at = Some(self.time);
            self.recommend.fetching = true;
            let rid = self.server.get(host, paths::RECOMMENDATIONS);
            self.requests.insert(rid, Req::Recommendations);
        }
        if !self.recommend.labels_fetching
            && self
                .recommend
                .labels_fetched_at
                .is_none_or(|t| self.time - t > LABELS_REFRESH)
        {
            self.recommend.labels_fetched_at = Some(self.time);
            self.recommend.labels_fetching = true;
            let rid = self.server.get(host, paths::LABELS);
            self.requests.insert(rid, Req::Labels);
        }
    }

    pub(crate) fn recommendations_fetched(&mut self, res: &Result<HttpResponse, String>) {
        self.recommend.fetching = false;
        let Some(v) = res
            .as_ref()
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| bl_json::parse_bytes(&r.body).ok())
        else {
            return;
        };
        self.recommend.shelves = v
            .get("shelves")
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| ShelfData {
                key: s.str_or("key", "").to_string(),
                arg: s.str_or("arg", "").to_string(),
                items: s
                    .get("items")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| {
                        (
                            i.str_or("id", "").to_string(),
                            i.get("reason").str_or("key", "").to_string(),
                            i.get("reason").str_or("arg", "").to_string(),
                        )
                    })
                    .collect(),
            })
            .collect();
        self.recommend.mixes = v
            .get("mixes")
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| MixData {
                id: m.str_or("id", "").to_string(),
                key: m.str_or("key", "").to_string(),
                arg: m.str_or("arg", "").to_string(),
                items: m
                    .get("items")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect(),
            })
            .collect();
        self.recommend.from_server = true;
        self.recommend.rev += 1;
    }

    pub(crate) fn labels_fetched(&mut self, res: &Result<HttpResponse, String>) {
        self.recommend.labels_fetching = false;
        let Some(v) = res
            .as_ref()
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| bl_json::parse_bytes(&r.body).ok())
        else {
            return;
        };
        let names: Vec<String> = v
            .get("names")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n.as_str().map(str::to_string))
            .collect();
        let mut map = HashMap::new();
        if let Some(o) = v.get("songs").as_object() {
            for (id, list) in o.iter() {
                let labels: Vec<String> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| names.get(n.as_u64()? as usize).cloned())
                    .collect();
                map.insert(id.to_string(), labels);
            }
        }
        if map != self.recommend.server_labels {
            self.recommend.server_labels = map;
            self.recommend.labels_rev += 1;
            self.revs.labels += 1;
        }
    }

    /// `home.overview`'s rows of suggestions.
    pub(crate) fn suggestions_json(&mut self, host: &dyn Host) -> (Value, Value) {
        if !self.recommend.from_server || !self.server.is_connected() {
            self.refresh_local_suggestions(host);
        }
        let shelves: Vec<Value> = self
            .recommend
            .shelves
            .iter()
            .map(|s| {
                // Songs no longer in the library are left out.
                let items: Vec<Value> = s
                    .items
                    .iter()
                    .filter_map(|(id, key, arg)| {
                        let t = self.library.track(id)?;
                        let arg = if key == "played-with" {
                            self.library
                                .track(arg)
                                .map_or(String::new(), |o| o.title.clone())
                        } else {
                            arg.clone()
                        };
                        Some(
                            self.track_json(t)
                                .with("reason", key.as_str())
                                .with("reasonArg", arg),
                        )
                    })
                    .collect();
                let arg_title = self.library.track(&s.arg).map(|t| t.title.clone());
                Value::object()
                    .with("key", s.key.as_str())
                    .with("arg", s.arg.as_str())
                    .with("argTitle", arg_title)
                    .with("items", Value::Array(items))
            })
            .collect();
        let mixes: Vec<Value> = self
            .recommend
            .mixes
            .iter()
            .filter_map(|m| {
                let first = m.items.iter().find_map(|id| self.library.track(id))?;
                let count = m
                    .items
                    .iter()
                    .filter(|id| self.library.track(id).is_some())
                    .count();
                Some(
                    Value::object()
                        .with("id", m.id.as_str())
                        .with("key", m.key.as_str())
                        .with("arg", m.arg.as_str())
                        .with("count", count)
                        .with("coverTrack", first.id.as_str())
                        .with("cover", self.cover_url_cached(first)),
                )
            })
            .collect();
        (Value::Array(shelves), Value::Array(mixes))
    }

    pub(crate) fn mix_play(&mut self, host: &mut dyn Host, id: &str, shuffle: bool) -> Value {
        let Some(items) = self
            .recommend
            .mixes
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.items.clone())
        else {
            return Value::object().with("error", "unknown mix");
        };
        let items: Vec<String> = items
            .into_iter()
            .filter(|i| self.library.track(i).is_some())
            .collect();
        if shuffle {
            self.queue.shuffle = true;
        }
        self.play_tracks(host, items, 0);
        Value::object().with("ok", true)
    }

    // ---- radio and endless playback ---------------------------------------

    /// Songs in the queue plus what was just played: not again for now.
    fn recent_ids(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for (_, id) in self.queue.ordered() {
            if seen.insert(id.clone()) {
                out.push(id.clone());
            }
        }
        for id in self.history.recent_ids().take(MAX_RECENT) {
            if out.len() >= MAX_RECENT {
                break;
            }
            if seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        }
        out
    }

    /// Whether the server should choose for this seed: it knows nothing of
    /// local files.
    fn server_can_answer(&self, seed: &Seed) -> bool {
        if !self.server.is_connected() {
            return false;
        }
        match seed {
            Seed::Songs(ids) => ids.iter().all(|i| library::is_server_id(i)),
            _ => self
                .library
                .tracks
                .iter()
                .any(|t| library::is_server_id(&t.id)),
        }
    }

    fn local_radio(&mut self, host: &dyn Host, seed: &Seed, count: usize) -> Vec<String> {
        let c = self.song_catalog();
        let t = self.local_taste(&c, host.unix_time() as u64);
        let recent = self.recent_ids();
        self.recommend.random = self.recommend.random.wrapping_add(1);
        let random = self.recommend.random.wrapping_add(host.unix_time() as u64);
        bl_recommend::radio(&c, &t, seed, &recent, count, random)
    }

    fn ask_server_radio(&mut self, host: &mut dyn Host, seed: &Value, start: bool) {
        let body = Value::object()
            .with("seed", seed.clone())
            .with("recent", self.recent_ids())
            .with("count", RADIO_BATCH)
            .to_json()
            .into_bytes();
        let rid = self
            .server
            .send(host, "POST", paths::RADIO, body, "application/json");
        self.requests.insert(rid, Req::Radio { start });
    }

    /// `songs.radio`: replaces the queue with a radio.
    pub(crate) fn radio_start(&mut self, host: &mut dyn Host, seed: Value, name: String) -> Value {
        let Some(parsed) = parse_seed(&seed) else {
            return Value::object().with("error", "unknown seed");
        };
        self.recommend.radio = Some(RadioState {
            seed: seed.clone(),
            name,
        });
        self.recommend.extending = true;
        if self.server_can_answer(&parsed) {
            self.ask_server_radio(host, &seed, true);
        } else {
            let items = self.local_radio(host, &parsed, RADIO_BATCH);
            self.radio_songs(host, items, true);
        }
        Value::object().with("ok", true)
    }

    pub(crate) fn radio_answer(
        &mut self,
        host: &mut dyn Host,
        res: &Result<HttpResponse, String>,
        start: bool,
    ) {
        let items: Option<Vec<String>> = res
            .as_ref()
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| bl_json::parse_bytes(&r.body).ok())
            .map(|v| {
                v.get("items")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            });
        let items = match items {
            Some(i) => i,
            // The server is away: choose here.
            None => {
                let seed = self
                    .recommend
                    .radio
                    .as_ref()
                    .and_then(|r| parse_seed(&r.seed))
                    .unwrap_or(Seed::Taste);
                self.local_radio(host, &seed, RADIO_BATCH)
            }
        };
        self.radio_songs(host, items, start);
    }

    fn radio_songs(&mut self, host: &mut dyn Host, items: Vec<String>, start: bool) {
        self.recommend.extending = false;
        let items: Vec<String> = items
            .into_iter()
            .filter(|i| self.library.track(i).is_some())
            .collect();
        if items.is_empty() {
            self.recommend.wait_until = self.time + RETRY;
            if start {
                self.recommend.radio = None;
                self.toast(self.tr("Nothing to play for this radio"), true);
            }
            return;
        }
        if start {
            // Playing a list ends a radio: this one goes on.
            let radio = self.recommend.radio.take();
            self.queue.shuffle = false;
            self.play_tracks(host, items, 0);
            self.recommend.radio = radio;
        } else {
            self.queue.push(items, &mut self.rng);
            self.revs.queue += 1;
            self.settings_changed();
        }
    }

    /// A list the user chose plays now: a radio is over.
    pub(crate) fn radio_end(&mut self) {
        self.recommend.radio = None;
    }

    pub(crate) fn radio_stop(&mut self) {
        self.radio_end();
        self.settings.endless = false;
        self.settings_changed();
    }

    pub(crate) fn radio_json(&self) -> Value {
        match &self.recommend.radio {
            Some(r) => Value::object()
                .with("active", true)
                .with("name", r.name.as_str()),
            None => Value::object().with("active", false),
        }
    }

    /// Radio or endless playback with few songs left: more of them.
    fn extend_queue(&mut self, host: &mut dyn Host) {
        let wanted = self.recommend.radio.is_some() || self.settings.endless;
        if !wanted
            || self.recommend.extending
            || self.settings.repeat != Repeat::None
            || self.time < self.recommend.wait_until
            || self.queue.is_empty()
        {
            return;
        }
        let Some(pos) = self.queue.position() else {
            return;
        };
        if self.queue.len() - 1 - pos > QUEUE_LOW {
            return;
        }
        let seed = match &self.recommend.radio {
            Some(r) => r.seed.clone(),
            None => {
                // Endless: more like the last songs.
                let last: Vec<String> = self
                    .queue
                    .ordered()
                    .map(|(_, id)| id.clone())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .filter(|id| {
                        !id.starts_with(crate::sources::RADIO_PREFIX)
                            && self.library.track(id).is_some()
                    })
                    .take(5)
                    .collect();
                if last.is_empty() {
                    return;
                }
                Value::object().with("tracks", last)
            }
        };
        let Some(parsed) = parse_seed(&seed) else {
            return;
        };
        self.recommend.extending = true;
        if self.server_can_answer(&parsed) {
            self.ask_server_radio(host, &seed, false);
        } else {
            let items = self.local_radio(host, &parsed, RADIO_BATCH);
            self.radio_songs(host, items, false);
        }
    }

    // ---- labels -------------------------------------------------------------

    /// Every label known here with its number of songs.
    pub(crate) fn labels_list(&mut self) -> Value {
        let c = self.song_catalog();
        Value::Array(
            c.label_counts()
                .into_iter()
                .map(|(name, count)| {
                    Value::object()
                        .with("name", name)
                        .with("count", count)
                        .with("given", !name.contains(':'))
                })
                .collect(),
        )
    }

    pub(crate) fn labels_tracks(&mut self, name: &str) -> Value {
        let c = self.song_catalog();
        Value::Array(
            c.label_songs(name)
                .into_iter()
                .filter_map(|i| self.library.track(c.id_of(i)))
                .map(|t| self.track_json(t))
                .collect(),
        )
    }

    /// `labels.of`: the labels of a song, sorted into automatic ones and
    /// those given by people.
    pub(crate) fn labels_of(&mut self, id: &str) -> Value {
        let c = self.song_catalog();
        let names: Vec<String> = c
            .index_of(id)
            .map(|i| c.labels_of(i).map(str::to_string).collect())
            .unwrap_or_default();
        let (auto, given): (Vec<String>, Vec<String>) =
            names.into_iter().partition(|n| n.contains(':'));
        Value::object().with("auto", auto).with("given", given)
    }

    /// `labels.change`: adds and removes labels given to a song.
    pub(crate) fn labels_change(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Value {
        let add: Vec<String> = add.iter().filter_map(|n| clean_name(n)).collect();
        let server = library::is_server_id(id);
        if server && !self.server.is_connected() {
            return Value::object().with("error", self.tr("Not connected to a server"));
        }
        let map = if server {
            &mut self.recommend.server_labels
        } else {
            &mut self.recommend.local_labels
        };
        let list = map.entry(id.to_string()).or_default();
        list.retain(|n| !remove.contains(n));
        for n in &add {
            if !list.contains(n) {
                list.push(n.clone());
            }
        }
        if list.is_empty() {
            map.remove(id);
        }
        self.recommend.labels_rev += 1;
        self.revs.labels += 1;
        if server {
            let body = Value::object()
                .with("add", add)
                .with("remove", remove)
                .to_json()
                .into_bytes();
            let rid = self.server.send(
                host,
                "PUT",
                &paths::track_labels(id),
                body,
                "application/json",
            );
            self.requests.insert(rid, Req::LabelsSaved);
        } else {
            let mut songs = Value::object();
            for (k, v) in &self.recommend.local_labels {
                songs = songs.with(k.as_str(), v.clone());
            }
            host.storage_write(
                STORAGE,
                Value::object().with("songs", songs).to_json().as_bytes(),
            );
        }
        Value::object().with("ok", true)
    }

    pub(crate) fn labels_saved(&mut self, res: &Result<HttpResponse, String>) {
        // Refused or lost: the server's list wins again.
        if !matches!(res, Ok(r) if (200..300).contains(&r.status)) {
            self.toast(self.tr("Could not save the label"), true);
            self.recommend.labels_fetched_at = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds() {
        let v = Value::object().with("tracks", vec!["a".to_string()]);
        assert_eq!(parse_seed(&v), Some(Seed::Songs(vec!["a".to_string()])));
        let v = Value::object().with("label", "genre:rock");
        assert_eq!(parse_seed(&v), Some(Seed::Label("genre:rock".to_string())));
        assert_eq!(
            parse_seed(&Value::object().with("taste", true)),
            Some(Seed::Taste)
        );
        assert_eq!(parse_seed(&Value::object()), None);
        assert_eq!(
            parse_seed(&Value::object().with("tracks", Vec::<String>::new())),
            None
        );
    }

    #[test]
    fn label_names() {
        assert_eq!(clean_name("  Party "), Some("Party".to_string()));
        assert_eq!(clean_name("a:b"), Some("a b".to_string()));
        assert_eq!(clean_name(" : "), None);
        assert_eq!(clean_name(&"x".repeat(41)), None);
    }
}
