//! Music from server extensions (e.g. YouTube, `docs/EXTENSIONS.md`): search
//! through the server, play a result right away while the server imports
//! it, then switch to the library track (full analysis) or keep the song
//! in the user's encrypted files. A song waits in the queue under an id of its
//! own and is only imported when it starts. Extensions can also offer rows of
//! suggestions, collections and a radio.

use crate::core::{Core, Req};
use crate::host::{Host, HttpResponse};
use bl_json::Value;
use bl_net::api::Track;
use std::collections::{HashMap, VecDeque};
mod playback;

/// Track id prefix of songs still being imported (followed by the job id).
pub(crate) const ID_PREFIX: &str = "ext:";
/// Track id prefix of songs not imported yet: `extitem:<extension>:<item>`.
pub(crate) const ITEM_PREFIX: &str = "extitem:";

/// Seconds between asking for an extension's suggestions.
const SHELVES_REFRESH: f64 = 15.0 * 60.0;
/// Songs of a collection fetched at most.
const MAX_COLLECTION: usize = 500;
const PAGE: usize = 50;
/// Songs fetched at once for a radio.
const RADIO_BATCH: usize = 25;
/// Songs known but not in the queue, before they are forgotten.
const MAX_ITEMS: usize = 3000;
/// Songs added to the library at the same time: the server does three, and
/// what is being played must still fit.
const ADDING_AT_ONCE: usize = 2;
/// Seconds to wait when the server is busy.
const ADD_WAIT: f64 = 5.0;

fn item_id(ext: &str, item: &str) -> String {
    format!("{ITEM_PREFIX}{ext}:{item}")
}

fn split_item(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(ITEM_PREFIX)?.split_once(':')
}

/// Seconds between job status checks.
const POLL: f64 = 1.5;

#[derive(Clone, Debug)]
pub(crate) enum ExtReq {
    Playback(String, u64),
    List,
    Search(u64, String),
    /// A waiting song (its id) is imported because it starts.
    Item {
        id: String,
        keep: bool,
        autoplay: bool,
    },
    Job(String),
    Download(String),
    Shelves(u64, String),
    Collection(u64, String),
    Radio {
        ext: String,
        start: bool,
    },
    /// A song goes into the library without being played.
    Add(String),
}

struct Job {
    job: String,
    /// The waiting song this job came from.
    item: String,
    keep: bool,
    title: String,
    artist: String,
    audio: String,
    cover: String,
    polled_at: f64,
    asked: bool,
    done: bool,
    started: f64,
    /// The browser cannot play a file still being written: it plays the
    /// library track once the import is done.
    play_when_done: bool,
    /// Added to the library without being played.
    adding: bool,
}

struct ExtInfo {
    id: String,
    name: String,
    search: bool,
    shelves: bool,
    collections: bool,
    radio: bool,
    playback: bool,
}

/// The collection being looked at.
#[derive(Default)]
struct Collection {
    ext: String,
    id: String,
    title: String,
    artist: String,
    detail: String,
    image: String,
    items: Vec<Value>,
    loading: bool,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct ExtState {
    feedback: playback::Feedback,
    list: Vec<ExtInfo>,
    query: String,
    serial: u64,
    pending: usize,
    results: Vec<Value>,
    error: Option<String>,
    rev: u64,
    jobs: Vec<Job>,
    /// Waiting for the answer to where songs go; the song that waits for it.
    asking: bool,
    asked_for: Option<String>,
    /// Pictures of waiting songs, by their ids.
    covers: HashMap<String, String>,
    /// Waiting songs already in the library: their tracks.
    in_library: HashMap<String, String>,
    shelves: Vec<Value>,
    shelves_at: Option<f64>,
    shelves_pending: usize,
    shelves_serial: u64,
    collection: Collection,
    collection_serial: u64,
    /// Songs still to go into the library, and how many are on their way.
    adding: VecDeque<String>,
    add_asked: usize,
    add_wait_until: f64,
    added: usize,
}

fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

impl Core {
    /// Asks the connected server for its extensions.
    pub(crate) fn extensions_refresh(&mut self, host: &mut dyn Host) {
        self.ext.feedback = playback::Feedback::default();
        let id = self.server.get(host, "/api/extensions");
        self.requests.insert(id, Req::Ext(ExtReq::List));
    }

    pub(crate) fn extensions_json(&self) -> Value {
        let list: Vec<Value> = self
            .ext
            .list
            .iter()
            .filter(|e| e.search)
            .map(|e| {
                Value::object()
                    .with("id", e.id.as_str())
                    .with("name", e.name.as_str())
                    .with("shelves", e.shelves)
                    .with("collections", e.collections)
                    .with("radio", e.radio)
            })
            .collect();
        Value::object()
            .with("list", Value::Array(list))
            .with("searching", self.ext.pending > 0)
            .with("asking", self.ext.asking)
            .with("adding", self.adding_count() as u64)
            .with("rev", self.ext.rev)
            .with("error", self.ext.error.clone())
            .with("target", self.settings.extension_target.as_str())
            .with(
                "importing",
                self.ext.jobs.iter().filter(|j| !j.done).count() as u64,
            )
    }

    pub(crate) fn extensions_command(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Value {
        match name {
            "extensions.search" => {
                let q: String = c.str_or("query", "").trim().chars().take(200).collect();
                self.ext.serial += 1;
                self.ext.query = q.clone();
                self.ext.results.clear();
                self.ext.error = None;
                self.ext.pending = 0;
                self.ext.rev += 1;
                if q.is_empty() || !self.server.is_connected() {
                    return Value::object().with("ok", true);
                }
                let exts: Vec<String> = self
                    .ext
                    .list
                    .iter()
                    .filter(|e| e.search)
                    .map(|e| e.id.clone())
                    .collect();
                for ext in exts {
                    let path = format!(
                        "/api/extensions/{ext}/search?q={}&limit=25",
                        bl_net::http::percent_encode(&q)
                    );
                    let id = self.device_request(host, "GET", &path, Vec::new(), None);
                    self.requests
                        .insert(id, Req::Ext(ExtReq::Search(self.ext.serial, ext)));
                    self.ext.pending += 1;
                }
                Value::object().with("ok", true)
            }
            "extensions.results" => Value::Array(self.ext.results.clone()),
            "extensions.play" => self.extensions_play(host, c),
            "extensions.target" => self.extensions_target(host, c),
            "extensions.add" => self.extensions_add(c),
            "extensions.shelves" => {
                self.extensions_shelves(host);
                Value::Array(self.ext.shelves.clone())
            }
            "extensions.collection" => {
                self.extensions_collection(host, c.str_or("ext", ""), c.str_or("id", ""))
            }
            "extensions.collectionData" => self.collection_json(),
            _ => Value::object().with("error", format!("unknown command {name}")),
        }
    }

    /// Plays a result: a known library track directly, else the song is
    /// imported as it starts. The first time, the app asks where songs go.
    fn extensions_play(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let ext = c.str_or("ext", "").to_string();
        let item = c.str_or("id", "").to_string();
        let id = item_id(&ext, &item);
        let result = self
            .ext
            .results
            .iter()
            .find(|r| r.str_or("ext", "") == ext && r.str_or("id", "") == item)
            .cloned();
        let track = result
            .as_ref()
            .and_then(|r| r.get("track").as_str())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| self.ext.in_library.get(&id).cloned());
        if let Some(track) = track {
            if let Some(r) = &result {
                self.add_result_track(&track, r);
            }
            self.play_tracks(host, vec![track], 0);
            return Value::object().with("ok", true);
        }
        if !self.extra_tracks.contains_key(&id) {
            return Value::object().with("error", "unknown result");
        }
        if let t @ ("library" | "vault") = c.str_or("target", "") {
            if c.get("remember").as_bool().unwrap_or(false) {
                self.settings.extension_target = t.to_string();
                self.settings_changed();
            }
            self.ext.asking = false;
            self.ext.asked_for = None;
            let keep = t == "library";
            if !keep && !self.vault_unlocked() {
                let msg = self.tr("Unlock your encrypted files first").to_string();
                self.toast(msg, true);
                return Value::object().with("error", "vault locked");
            }
            // Just this once where it goes, if not to be remembered.
            let saved = std::mem::replace(&mut self.settings.extension_target, t.to_string());
            self.play_tracks(host, vec![id], 0);
            if !c.get("remember").as_bool().unwrap_or(false) {
                self.settings.extension_target = saved;
            }
            return Value::object().with("ok", true);
        }
        match self.settings.extension_target.as_str() {
            "library" | "vault" => {
                if self.settings.extension_target == "vault" && !self.vault_unlocked() {
                    let msg = self.tr("Unlock your encrypted files first").to_string();
                    self.toast(msg, true);
                    return Value::object().with("error", "vault locked");
                }
                self.play_tracks(host, vec![id], 0);
                Value::object().with("ok", true)
            }
            _ => {
                self.ext.asking = true;
                self.ext.asked_for = Some(id);
                self.ext.rev += 1;
                Value::object().with("ask", true)
            }
        }
    }

    /// The answer to where songs go (empty: not at all, for now).
    fn extensions_target(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let target = c.str_or("target", "");
        let waiting = self.ext.asked_for.take();
        self.ext.asking = false;
        self.ext.rev += 1;
        if !matches!(target, "library" | "vault") {
            if self
                .current
                .as_ref()
                .is_some_and(|t| t.id.starts_with(ITEM_PREFIX))
            {
                self.player.stop(host);
            }
            return Value::object().with("ok", true);
        }
        self.settings.extension_target = target.to_string();
        self.settings_changed();
        if let Some(id) = waiting {
            if self.queue.current() == Some(&id) {
                self.play_current(host, true);
            } else {
                self.play_tracks(host, vec![id], 0);
            }
        }
        Value::object().with("ok", true)
    }

    /// Songs go into the library without being played. Whoever asks for this
    /// wants them there, so nothing is asked and they stay when unplayed.
    fn extensions_add(&mut self, c: &Value) -> Value {
        let before = self.ext.adding.len();
        for id in c.get("ids").as_array().into_iter().flatten() {
            let Some(id) = id.as_str().filter(|id| split_item(id).is_some()) else {
                continue;
            };
            if !self.ext.adding.iter().any(|a| a == id)
                && !self.ext.jobs.iter().any(|j| j.adding && j.item == id)
                && !self.ext.in_library.contains_key(id)
            {
                self.ext.adding.push_back(id.to_string());
            }
        }
        if self.ext.adding.len() > before {
            let msg = self.tr("Adding to the library…").to_string();
            self.toast(msg, false);
        }
        self.ext.rev += 1;
        Value::object().with("ok", true)
    }

    fn adding_count(&self) -> usize {
        self.ext.adding.len()
            + self.ext.add_asked
            + self.ext.jobs.iter().filter(|j| j.adding).count()
    }

    /// Sends the next songs on their way; says so when all have arrived.
    fn extension_add_next(&mut self, host: &mut dyn Host) {
        if self.time < self.ext.add_wait_until || !self.server.is_connected() {
            return;
        }
        while self.ext.add_asked + self.ext.jobs.iter().filter(|j| j.adding).count()
            < ADDING_AT_ONCE
        {
            let Some(id) = self.ext.adding.pop_front() else {
                break;
            };
            let Some((ext, item)) = split_item(&id).filter(|(e, i)| safe_id(e) && safe_id(i))
            else {
                continue;
            };
            let body = Value::object()
                .with("id", item)
                .with("keep", true)
                .with("pin", true)
                .to_json()
                .into_bytes();
            let path = format!("/api/extensions/{ext}/import");
            let rid = self.device_request(host, "POST", &path, body, Some("application/json"));
            self.requests.insert(rid, Req::Ext(ExtReq::Add(id)));
            self.ext.add_asked += 1;
        }
        if self.ext.added > 0 && self.adding_count() == 0 {
            let msg = if self.ext.added == 1 {
                self.tr("Added to the library").to_string()
            } else {
                format!("{}: {}", self.tr("Added to the library"), self.ext.added)
            };
            self.ext.added = 0;
            self.toast(msg, false);
        }
    }

    /// A waiting song turned out to be, or has become, a library track.
    fn song_in_library(&mut self, id: &str, track: &str) {
        self.ext
            .in_library
            .insert(id.to_string(), track.to_string());
        if let Some(t) = self.extra_tracks.get(id).cloned() {
            if self.library.track(track).is_none() {
                self.extra_tracks.insert(
                    track.to_string(),
                    Track {
                        id: track.to_string(),
                        ..t
                    },
                );
            }
        }
        // Not the one that is just starting: that takes its own way there.
        if self.queue.current().map(String::as_str) != Some(id) {
            self.queue_replace(id, track);
        }
        let mark = |list: &mut Vec<Value>| {
            for r in list.iter_mut().filter(|r| r.str_or("songId", "") == id) {
                *r = r
                    .clone()
                    .with("inLibrary", true)
                    .with("track", track)
                    .with("songId", track);
            }
        };
        mark(&mut self.ext.results);
        mark(&mut self.ext.collection.items);
        self.ext.rev += 1;
    }

    /// The queue reached a song not imported yet: now it is.
    pub(crate) fn extension_item_start(&mut self, host: &mut dyn Host, id: &str, autoplay: bool) {
        let keep = match self.settings.extension_target.as_str() {
            "library" => true,
            "vault" => false,
            _ => {
                self.ext.asking = true;
                self.ext.asked_for = Some(id.to_string());
                self.ext.rev += 1;
                return;
            }
        };
        if !keep && !self.vault_unlocked() {
            let msg = self.tr("Unlock your encrypted files first").to_string();
            self.toast(msg, true);
            return;
        }
        let Some((ext, item)) = split_item(id).filter(|(e, i)| safe_id(e) && safe_id(i)) else {
            self.queue_drop(host, id);
            return;
        };
        if !self.server.is_connected() {
            let msg = self.tr("The song's source is not available").to_string();
            self.toast(msg, true);
            return;
        }
        let body = Value::object()
            .with("id", item)
            .with("keep", keep)
            .to_json()
            .into_bytes();
        let path = format!("/api/extensions/{ext}/import");
        let rid = self.device_request(host, "POST", &path, body, Some("application/json"));
        self.requests.insert(
            rid,
            Req::Ext(ExtReq::Item {
                id: id.to_string(),
                keep,
                autoplay,
            }),
        );
        let msg = self.tr("Loading…").to_string();
        self.toast(msg, false);
    }

    /// A song that cannot play leaves the queue; the next one starts.
    fn queue_drop(&mut self, host: &mut dyn Host, id: &str) {
        let current = self.queue.current().is_some_and(|c| c == id);
        let pos = self.queue.ordered().position(|(_, x)| x == id);
        if let Some(pos) = pos {
            self.queue.remove_at(pos);
            self.revs.queue += 1;
        }
        if current {
            self.play_current(host, true);
        }
    }

    /// Replaces a song in the queue everywhere.
    fn queue_replace(&mut self, old: &str, new: &str) {
        for it in self.queue.items.iter_mut() {
            if it == old {
                *it = new.to_string();
            }
        }
        self.revs.queue += 1;
    }

    /// A result as the pages show it: what to play is in `songId`.
    fn register_entry(&mut self, ext: &str, raw: &Value) -> Option<Value> {
        let image = raw
            .get("image")
            .as_str()
            .filter(|p| p.starts_with("/api/extensions/"))
            .map(|p| self.server.image_url(&format!("{p}?size=300")))
            .unwrap_or_default();
        let entry = raw.clone().with("ext", ext).with("image", image.as_str());
        if raw.str_or("kind", "song") == "collection" {
            return Some(entry);
        }
        let item = raw.str_or("id", "");
        if !safe_id(item) {
            return None;
        }
        let id = item_id(ext, item);
        let known = raw
            .get("track")
            .as_str()
            .filter(|t| self.library.track(t).is_some())
            .map(str::to_string);
        match &known {
            Some(track) => {
                self.ext.in_library.insert(id.clone(), track.clone());
            }
            None => {
                self.ext.in_library.remove(&id);
                let t = Track {
                    id: id.clone(),
                    title: raw.str_or("title", "").to_string(),
                    artists: Some(raw.str_or("artist", ""))
                        .filter(|a| !a.is_empty())
                        .map(|a| vec![a.to_string()])
                        .unwrap_or_default(),
                    album: raw.str_or("album", "").to_string(),
                    duration_ms: (raw.f64_or("duration", 0.0).max(0.0) * 1000.0) as u64,
                    codec: "opus".into(),
                    has_cover: !image.is_empty(),
                    ..Default::default()
                };
                self.extra_tracks.insert(id.clone(), t);
                self.ext.covers.insert(id.clone(), image);
            }
        }
        Some(
            entry
                .with("inLibrary", known.is_some())
                .with("songId", known.unwrap_or(id)),
        )
    }

    /// Forgets songs that were only seen, once there are very many.
    fn forget_items(&mut self) {
        if self.ext.covers.len() <= MAX_ITEMS {
            return;
        }
        let queued: std::collections::HashSet<String> = self.queue.items.iter().cloned().collect();
        let shown: std::collections::HashSet<String> = self
            .ext
            .results
            .iter()
            .chain(self.ext.collection.items.iter())
            .map(|r| r.str_or("songId", "").to_string())
            .collect();
        let keep = |id: &String| queued.contains(id) || shown.contains(id);
        self.ext.covers.retain(|id, _| keep(id));
        self.extra_tracks
            .retain(|id, _| !id.starts_with(ITEM_PREFIX) || keep(id));
    }

    /// Asks for the suggestions when they are old; the answer comes later.
    fn extensions_shelves(&mut self, host: &mut dyn Host) {
        if !self.server.is_connected()
            || self.ext.shelves_pending > 0
            || self
                .ext
                .shelves_at
                .is_some_and(|t| self.time - t < SHELVES_REFRESH)
        {
            return;
        }
        self.ext.shelves_at = Some(self.time);
        self.ext.shelves_serial += 1;
        self.ext.shelves.clear();
        let exts: Vec<String> = self
            .ext
            .list
            .iter()
            .filter(|e| e.shelves)
            .map(|e| e.id.clone())
            .collect();
        for ext in exts {
            let rid = self.device_request(
                host,
                "GET",
                &format!("/api/extensions/{ext}/shelves"),
                Vec::new(),
                None,
            );
            self.requests
                .insert(rid, Req::Ext(ExtReq::Shelves(self.ext.shelves_serial, ext)));
            self.ext.shelves_pending += 1;
        }
    }

    fn extensions_collection(&mut self, host: &mut dyn Host, ext: &str, id: &str) -> Value {
        if !safe_id(ext) || !safe_id(id) {
            return Value::object().with("error", "invalid id");
        }
        self.ext.collection = Collection {
            ext: ext.to_string(),
            id: id.to_string(),
            loading: true,
            ..Default::default()
        };
        self.ext.collection_serial += 1;
        self.ext.rev += 1;
        self.collection_page(host, None);
        Value::object().with("ok", true)
    }

    fn collection_page(&mut self, host: &mut dyn Host, after: Option<&str>) {
        let c = &self.ext.collection;
        let mut path = format!(
            "/api/extensions/{}/collections/{}?limit={PAGE}",
            c.ext, c.id
        );
        if let Some(after) = after {
            path.push_str(&format!("&after={}", bl_net::http::percent_encode(after)));
        }
        let ext = c.ext.clone();
        let rid = self.device_request(host, "GET", &path, Vec::new(), None);
        self.requests.insert(
            rid,
            Req::Ext(ExtReq::Collection(self.ext.collection_serial, ext)),
        );
    }

    fn collection_json(&self) -> Value {
        let c = &self.ext.collection;
        Value::object()
            .with("ext", c.ext.as_str())
            .with("id", c.id.as_str())
            .with("title", c.title.as_str())
            .with("artist", c.artist.as_str())
            .with("detail", c.detail.as_str())
            .with("image", c.image.as_str())
            .with("items", Value::Array(c.items.clone()))
            .with("loading", c.loading)
            .with("error", c.error.clone())
    }

    /// `songs.radio` for a song of an extension.
    pub(crate) fn extensions_radio_start(
        &mut self,
        host: &mut dyn Host,
        seed_id: &str,
        name: String,
    ) -> Option<Value> {
        let item = if seed_id.starts_with(ID_PREFIX) {
            let job = seed_id.strip_prefix(ID_PREFIX)?;
            self.ext.jobs.iter().find(|j| j.job == job)?.item.clone()
        } else {
            seed_id.to_string()
        };
        let (ext, song) = split_item(&item)?;
        let (ext, song) = (ext.to_string(), song.to_string());
        if !self.ext.list.iter().any(|e| e.id == ext && e.radio) {
            let msg = self.tr("Nothing to play for this radio").to_string();
            self.toast(msg, true);
            return Some(Value::object().with("error", "no radio"));
        }
        let name = if name.is_empty() {
            self.track_for(&item).title
        } else {
            name
        };
        let seed = Value::object().with(
            "extRadio",
            Value::object()
                .with("ext", ext.as_str())
                .with("item", song.as_str()),
        );
        self.radio_began(seed, name);
        self.ask_extension_radio(host, &ext, &song, true);
        Some(Value::object().with("ok", true))
    }

    /// More songs for a running radio, following the last song in the queue.
    pub(crate) fn extensions_radio_more(&mut self, host: &mut dyn Host, seed: &Value) -> bool {
        let Some((ext, item)) = seed
            .get("extRadio")
            .as_object()
            .map(|_| seed.get("extRadio"))
            .map(|r| {
                (
                    r.str_or("ext", "").to_string(),
                    r.str_or("item", "").to_string(),
                )
            })
        else {
            return false;
        };
        let last = self
            .queue
            .items
            .iter()
            .rev()
            .find_map(|id| split_item(id).filter(|(e, _)| *e == ext))
            .map(|(_, i)| i.to_string())
            .unwrap_or(item);
        self.ask_extension_radio(host, &ext, &last, false);
        true
    }

    fn ask_extension_radio(&mut self, host: &mut dyn Host, ext: &str, item: &str, start: bool) {
        let path = format!("/api/extensions/{ext}/radio/{item}?limit={RADIO_BATCH}");
        let rid = self.device_request(host, "GET", &path, Vec::new(), None);
        self.requests.insert(
            rid,
            Req::Ext(ExtReq::Radio {
                ext: ext.to_string(),
                start,
            }),
        );
    }

    /// A song on its way to the user's encrypted files. A job no longer known
    /// counts as one, it may have ended up there.
    pub(crate) fn extension_job_is_private(&self, id: &str) -> bool {
        id.strip_prefix(ID_PREFIX)
            .is_some_and(|job| !self.ext.jobs.iter().any(|j| j.job == job && j.keep))
    }

    /// Metadata of a result for a track the library list may not have yet.
    fn add_result_track(&mut self, id: &str, r: &Value) {
        if self.library.track(id).is_some() {
            return;
        }
        let t = Track {
            id: id.to_string(),
            title: r.str_or("title", "").to_string(),
            artists: Some(r.str_or("artist", ""))
                .filter(|a| !a.is_empty())
                .map(|a| vec![a.to_string()])
                .unwrap_or_default(),
            duration_ms: (r.f64_or("duration", 0.0).max(0.0) * 1000.0) as u64,
            codec: "opus".into(),
            has_cover: true,
            ..Default::default()
        };
        self.extra_tracks.insert(id.to_string(), t);
    }

    pub(crate) fn ext_http(
        &mut self,
        host: &mut dyn Host,
        req: ExtReq,
        res: Result<HttpResponse, String>,
    ) {
        let json = match &res {
            Ok(r) if (200..300).contains(&r.status) => bl_json::parse_bytes(&r.body).ok(),
            _ => None,
        };
        let error = || match &res {
            Ok(r) => bl_json::parse_bytes(&r.body)
                .ok()
                .map(|v| v.str_or("error", "").to_string())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| format!("HTTP {}", r.status)),
            Err(e) => e.clone(),
        };
        match req {
            ExtReq::Playback(session, seq) => {
                self.ext
                    .feedback
                    .complete(&session, seq, json.is_some(), self.time)
            }
            ExtReq::List => {
                self.ext.list = json
                    .as_ref()
                    .and_then(|v| v.get("extensions").as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter(|e| safe_id(e.str_or("id", "")))
                    .map(|e| ExtInfo {
                        id: e.str_or("id", "").to_string(),
                        name: e.str_or("name", "").to_string(),
                        search: e.bool_or("search", false),
                        shelves: e.bool_or("shelves", false),
                        collections: e.bool_or("collections", false),
                        radio: e.bool_or("radio", false),
                        playback: e.bool_or("playback", false),
                    })
                    .collect();
                self.ext.shelves_at = None;
                self.ext.rev += 1;
            }
            ExtReq::Search(serial, ext) => {
                if serial != self.ext.serial {
                    return;
                }
                self.ext.pending = self.ext.pending.saturating_sub(1);
                match json {
                    Some(v) => {
                        for it in v.get("items").as_array().cloned().unwrap_or_default() {
                            if let Some(entry) = self.register_entry(&ext, &it) {
                                self.ext.results.push(entry);
                            }
                        }
                        self.forget_items();
                    }
                    None => self.ext.error = Some(error()),
                }
                self.ext.rev += 1;
            }
            ExtReq::Item { id, keep, autoplay } => {
                let Some(v) = json else {
                    let msg = format!("{}: {}", self.tr("Could not load the song"), error());
                    self.toast(msg, true);
                    self.queue_drop(host, &id);
                    return;
                };
                let current = self.queue.current().is_some_and(|c| *c == id);
                if let Some(track) = v.get("track").as_str().filter(|t| !t.is_empty()) {
                    // Already in the library.
                    let track = track.to_string();
                    let known = self.track_for(&id);
                    if self.library.track(&track).is_none() {
                        self.extra_tracks.insert(
                            track.clone(),
                            Track {
                                id: track.clone(),
                                ..known
                            },
                        );
                    }
                    self.queue_replace(&id, &track);
                    if current {
                        self.play_current(host, autoplay);
                    }
                    return;
                }
                let job = v.str_or("job", "").to_string();
                let audio = v.str_or("audio", "").to_string();
                if !safe_id(&job) || !audio.starts_with("/api/extensions/jobs/") {
                    self.queue_drop(host, &id);
                    return;
                }
                let known = self.track_for(&id);
                let new_id = format!("{ID_PREFIX}{job}");
                let mut t = Track {
                    id: new_id.clone(),
                    title: v.str_or("title", &known.title).to_string(),
                    duration_ms: Some(v.f64_or("duration", 0.0))
                        .filter(|d| *d > 0.0)
                        .map_or(known.duration_ms, |d| (d * 1000.0) as u64),
                    codec: "opus".into(),
                    has_cover: known.has_cover,
                    ..Default::default()
                };
                let artist = v
                    .str_or("artist", known.artists.first().map_or("", String::as_str))
                    .to_string();
                if !artist.is_empty() {
                    t.artists = vec![artist.clone()];
                }
                t.album = v.str_or("album", &known.album).to_string();
                self.ext.jobs.push(Job {
                    job,
                    item: id.clone(),
                    keep,
                    title: t.title.clone(),
                    artist,
                    audio,
                    cover: self.ext.covers.get(&id).cloned().unwrap_or_default(),
                    polled_at: self.time,
                    asked: false,
                    done: false,
                    started: self.time,
                    play_when_done: !host.has_threads() || host.audio_is_push(),
                    adding: false,
                });
                let later = self.ext.jobs.last().is_some_and(|j| j.play_when_done);
                self.extra_tracks.insert(new_id.clone(), t);
                self.queue_replace(&id, &new_id);
                if current {
                    if later {
                        self.current = Some(self.track_for(&new_id));
                    } else {
                        self.play_current(host, autoplay);
                    }
                }
            }
            ExtReq::Shelves(serial, ext) => {
                if serial != self.ext.shelves_serial {
                    return;
                }
                self.ext.shelves_pending = self.ext.shelves_pending.saturating_sub(1);
                let name = self
                    .ext
                    .list
                    .iter()
                    .find(|e| e.id == ext)
                    .map_or(String::new(), |e| e.name.clone());
                let shelves = json
                    .as_ref()
                    .and_then(|v| v.get("shelves").as_array().cloned())
                    .unwrap_or_default();
                for shelf in shelves {
                    let items: Vec<Value> = shelf
                        .get("items")
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|it| self.register_entry(&ext, it))
                        .collect();
                    if !items.is_empty() {
                        self.ext.shelves.push(
                            Value::object()
                                .with("ext", ext.as_str())
                                .with("extName", name.as_str())
                                .with("title", shelf.str_or("title", ""))
                                .with("items", Value::Array(items)),
                        );
                    }
                }
                self.forget_items();
                self.ext.rev += 1;
            }
            ExtReq::Collection(serial, ext) => {
                if serial != self.ext.collection_serial {
                    return;
                }
                let Some(v) = json else {
                    self.ext.collection.loading = false;
                    self.ext.collection.error = Some(error());
                    self.ext.rev += 1;
                    return;
                };
                let image = v
                    .get("image")
                    .as_str()
                    .filter(|p| p.starts_with("/api/extensions/"))
                    .map(|p| self.server.image_url(&format!("{p}?size=600")));
                let items: Vec<Value> = v
                    .get("items")
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|it| self.register_entry(&ext, it))
                    .collect();
                let c = &mut self.ext.collection;
                if c.title.is_empty() {
                    c.title = v.str_or("title", "").to_string();
                    c.artist = v.str_or("artist", "").to_string();
                    c.detail = v.str_or("detail", "").to_string();
                    c.image = image.unwrap_or_default();
                }
                c.items.extend(items);
                let next = v.get("next").as_str().map(str::to_string);
                let more = next.filter(|_| c.items.len() < MAX_COLLECTION);
                c.loading = more.is_some();
                self.ext.rev += 1;
                if let Some(after) = more {
                    self.collection_page(host, Some(&after));
                }
            }
            ExtReq::Radio { ext, start } => {
                let items: Vec<String> = json
                    .as_ref()
                    .and_then(|v| v.get("items").as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|it| self.register_entry(&ext, it))
                    .map(|e| e.str_or("songId", "").to_string())
                    .collect();
                let fresh: Vec<String> = items
                    .into_iter()
                    .filter(|i| !self.queue.items.contains(i))
                    .collect();
                self.radio_play(host, fresh, start);
            }
            ExtReq::Add(id) => {
                self.ext.add_asked = self.ext.add_asked.saturating_sub(1);
                let busy = matches!(&res, Ok(r) if r.status == 503);
                let Some(v) = json else {
                    if busy {
                        // Too many at once on the server: a little later.
                        self.ext.adding.push_front(id);
                        self.ext.add_wait_until = self.time + ADD_WAIT;
                    } else {
                        let msg = format!("{}: {}", self.tr("Could not load the song"), error());
                        self.toast(msg, true);
                    }
                    return;
                };
                if let Some(track) = v.get("track").as_str().filter(|t| !t.is_empty()) {
                    let track = track.to_string();
                    self.song_in_library(&id, &track);
                    self.ext.added += 1;
                    // Maybe hidden until now, and someone else may have brought it.
                    self.refresh_server_data(host);
                    return;
                }
                let job = v.str_or("job", "").to_string();
                let audio = v.str_or("audio", "").to_string();
                if safe_id(&job) && audio.starts_with("/api/extensions/jobs/") {
                    self.ext.jobs.push(Job {
                        job,
                        item: id,
                        keep: true,
                        title: v.str_or("title", "").to_string(),
                        artist: v.str_or("artist", "").to_string(),
                        audio,
                        cover: String::new(),
                        polled_at: self.time,
                        asked: false,
                        done: false,
                        started: self.time,
                        play_when_done: false,
                        adding: true,
                    });
                }
            }
            ExtReq::Job(job) => {
                if let Some(j) = self.ext.jobs.iter_mut().find(|j| j.job == job) {
                    j.asked = false;
                }
                let Some(v) = json else {
                    return;
                };
                match v.str_or("state", "") {
                    "failed" => self.ext_job_failed(host, &job, v.str_or("error", "")),
                    "done" => self.ext_job_done(host, &job, v.get("track").as_str()),
                    _ => {}
                }
            }
            ExtReq::Download(job) => {
                let Some(j) = self.ext.jobs.iter().find(|j| j.job == job) else {
                    return;
                };
                let name = if j.artist.is_empty() {
                    format!("{}.opus", j.title)
                } else {
                    format!("{} - {}.opus", j.artist, j.title)
                };
                match res {
                    Ok(r) if r.status == 200 && !r.body.is_empty() => {
                        self.vault_upload_data(host, name, r.body);
                        let msg = self.tr("Saved in your encrypted files").to_string();
                        self.toast(msg, false);
                    }
                    _ => {
                        let msg = format!("{}: {}", self.tr("Could not save the song"), error());
                        self.toast(msg, true);
                    }
                }
                self.ext.jobs.retain(|j| j.job != job);
            }
        }
    }

    fn ext_job_failed(&mut self, host: &mut dyn Host, job: &str, msg: &str) {
        let msg = format!("{}: {msg}", self.tr("Could not load the song"));
        self.toast(msg, true);
        let id = format!("{ID_PREFIX}{job}");
        self.ext.jobs.retain(|j| j.job != job);
        self.queue_drop(host, &id);
    }

    fn ext_job_done(&mut self, host: &mut dyn Host, job: &str, track: Option<&str>) {
        let Some(j) = self.ext.jobs.iter_mut().find(|j| j.job == job) else {
            return;
        };
        if !j.keep {
            if !j.done {
                j.done = true;
                // The finished file goes into the encrypted files.
                let path = j.audio.clone();
                let id = self.server.get(host, &path);
                self.requests
                    .insert(id, Req::Ext(ExtReq::Download(job.to_string())));
            }
            return;
        }
        j.done = true;
        let Some(track) = track.filter(|t| !t.is_empty()).map(str::to_string) else {
            // The library scan has not found it yet: ask again (a while).
            if self.time - j.started > 180.0 {
                self.ext.jobs.retain(|j| j.job != job);
            } else {
                j.done = false;
            }
            return;
        };
        if j.adding {
            let item = j.item.clone();
            self.ext.jobs.retain(|j| j.job != job);
            self.ext.added += 1;
            self.song_in_library(&item, &track);
            self.refresh_server_data(host);
            return;
        }
        let item = j.item.clone();
        self.song_in_library(&item, &track);
        let old = format!("{ID_PREFIX}{job}");
        // The library track replaces the import everywhere.
        if let Some(t) = self.extra_tracks.remove(&old) {
            let mut t = t;
            t.id = track.clone();
            self.extra_tracks.insert(track.clone(), t);
        }
        for it in self.queue.items.iter_mut() {
            if *it == old {
                *it = track.clone();
            }
        }
        let playing = self.current.as_ref().is_some_and(|t| t.id == old);
        if playing {
            if let Some(cur) = self.current.as_mut() {
                cur.id = track.clone();
                cur.has_cover = true;
            }
            // The server analyses the stored file: effects that anticipate.
            self.request_analysis_for_current();
        }
        self.revs.queue += 1;
        let play_now = self
            .ext
            .jobs
            .iter()
            .any(|j| j.job == job && j.play_when_done);
        self.ext.jobs.retain(|j| j.job != job);
        if play_now {
            if self.queue.current() == Some(&track) {
                self.play_current(host, true);
            } else if !self.queue.items.contains(&track) {
                self.play_tracks(host, vec![track], 0);
            }
        }
        self.refresh_server_data(host);
    }

    /// Polls running imports (every tick).
    pub(crate) fn update_extensions(&mut self, host: &mut dyn Host) {
        self.extension_add_next(host);
        if self.ext.jobs.is_empty() || !self.server.is_connected() {
            return;
        }
        let now = self.time;
        let due: Vec<String> = self
            .ext
            .jobs
            .iter()
            .filter(|j| !j.asked && !(j.done && !j.keep) && now - j.polled_at >= POLL)
            .map(|j| j.job.clone())
            .collect();
        for job in due {
            let id = self
                .server
                .get(host, &format!("/api/extensions/jobs/{job}"));
            self.requests.insert(id, Req::Ext(ExtReq::Job(job.clone())));
            if let Some(j) = self.ext.jobs.iter_mut().find(|j| j.job == job) {
                j.asked = true;
                j.polled_at = now;
            }
        }
    }

    /// The stream of a song being imported.
    pub(crate) fn ext_stream_url(&self, id: &str) -> Option<String> {
        let job = id.strip_prefix(ID_PREFIX)?;
        let j = self.ext.jobs.iter().find(|j| j.job == job)?;
        Some(self.server.url(&j.audio))
    }

    pub(crate) fn ext_cover_url(&self, id: &str) -> String {
        if id.starts_with(ITEM_PREFIX) {
            return self.ext.covers.get(id).cloned().unwrap_or_default();
        }
        let Some(job) = id.strip_prefix(ID_PREFIX) else {
            return String::new();
        };
        self.ext
            .jobs
            .iter()
            .find(|j| j.job == job)
            .map(|j| j.cover.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_songs_id_holds_extension_and_item() {
        let id = item_id("tube", "abc:def");
        assert_eq!(id, "extitem:tube:abc:def");
        assert_eq!(split_item(&id), Some(("tube", "abc:def")));
        assert_eq!(split_item("ext:job"), None);
        assert_eq!(split_item("extitem:nocolon"), None);
        assert!(
            !crate::library::is_server_id(&id),
            "not a song of the server"
        );
    }
}
