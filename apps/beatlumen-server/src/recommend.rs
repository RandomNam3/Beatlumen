//! Recommendations, radios and mixes for a login (`bl_recommend`, fed with
//! the library, the sound analysis, the login's labels, favorites, playlists
//! and, if the user agreed to send it, their listening history).

use crate::favorites::Favorites;
use crate::history::History;
use crate::labels::SongLabels;
use crate::playlists::Playlists;
use crate::{util, State};
use bl_json::Value;
use bl_recommend::{Catalog, Play, Reason, Seed, Signals, Song, Taste};
use std::sync::{Arc, Mutex};

/// Songs of a radio in one go.
const MAX_COUNT: usize = 200;
/// Ids a device may list as recently played.
const MAX_RECENT: usize = 5000;

/// What belongs to one login.
pub struct Login<'a> {
    pub favorites: &'a Favorites,
    pub history: &'a History,
    pub playlists: &'a Playlists,
    pub labels: &'a SongLabels,
}

/// Catalogs kept (each login has its own, for its labels).
const CATALOGS: usize = 4;

/// What a catalog was built from: (login's labels, library revision, sound
/// revision, labels revision).
type Built = (usize, u64, u64, u64);

#[derive(Default)]
pub struct Cache {
    built: Mutex<Vec<(Built, Arc<Catalog>)>>,
}

impl Cache {
    /// Building takes a moment with a large library, and most requests find
    /// nothing changed.
    pub fn catalog(&self, state: &State, login: &Login) -> Arc<Catalog> {
        let lib = state.library();
        let who = login.labels as *const SongLabels as usize;
        let key = (
            who,
            lib.revision,
            state.sound.revision(),
            login.labels.revision(),
        );
        let mut built = self.built.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, c)) = built.iter().find(|(k, _)| *k == key) {
            return c.clone();
        }
        built.retain(|(k, _)| k.0 != who);
        if built.len() >= CATALOGS {
            built.remove(0);
        }
        let given = login.labels.snapshot();
        let songs = lib
            .tracks()
            .iter()
            .map(|e| Song {
                id: e.track.id.clone(),
                artists: e.track.artists.clone(),
                album_id: e.track.album_id.clone(),
                genre: e.track.genre.clone(),
                year: e.track.year,
                added: e.track.modified,
                features: state.sound.features(&e.track.id, e.mtime_ns),
                labels: given.get(&e.track.id).cloned().unwrap_or_default(),
            })
            .collect();
        let c = Arc::new(Catalog::new(songs));
        built.push((key, c.clone()));
        c
    }
}

fn taste(c: &Catalog, login: &Login) -> Taste {
    let plays: Vec<Play> = login
        .history
        .events()
        .into_iter()
        .map(|e| Play {
            id: e.id,
            t: e.t,
            skip: e.skip,
        })
        .collect();
    let favorites: Vec<(String, u64)> = login
        .favorites
        .ids()
        .into_iter()
        .map(|f| (f.id, f.added))
        .collect();
    let lists = login.playlists.track_items();
    Taste::new(
        c,
        &Signals {
            plays: &plays,
            favorites: &favorites,
            playlists: lists.iter().map(Vec::as_slice).collect(),
            now: util::now_secs(),
        },
    )
}

/// The same for a day, different the next.
fn today() -> u64 {
    util::now_secs() / 86_400
}

fn reason_json(r: &Reason) -> Value {
    let (key, arg) = match r {
        Reason::Artist(a) => ("artist", a.as_str()),
        Reason::Genre(g) => ("genre", g.as_str()),
        Reason::PlayedWith(s) => ("played-with", s.as_str()),
        Reason::New => ("new", ""),
        Reason::Forgotten => ("forgotten", ""),
        Reason::Discover => ("discover", ""),
    };
    Value::object().with("key", key).with("arg", arg)
}

/// `GET /api/recommendations`: the start page's rows and the mixes.
pub fn recommendations(state: &State, login: &Login) -> Value {
    let c = state.recommend.catalog(state, login);
    let t = taste(&c, login);
    let day = today();
    let shelves: Vec<Value> = bl_recommend::shelves(&c, &t, day)
        .iter()
        .map(|s| {
            Value::object()
                .with("key", s.key)
                .with("arg", s.arg.as_str())
                .with(
                    "items",
                    Value::Array(
                        s.items
                            .iter()
                            .map(|i| {
                                Value::object()
                                    .with("id", i.id.as_str())
                                    .with("reason", reason_json(&i.reason))
                            })
                            .collect(),
                    ),
                )
        })
        .collect();
    let mixes: Vec<Value> = bl_recommend::mixes(&c, &t, day)
        .iter()
        .map(|m| {
            Value::object()
                .with("id", m.id.as_str())
                .with("key", m.key)
                .with("arg", m.arg.as_str())
                .with("items", m.items.clone())
        })
        .collect();
    Value::object()
        .with("shelves", Value::Array(shelves))
        .with("mixes", Value::Array(mixes))
}

fn parse_seed(v: &Value) -> Result<Seed, (u16, String)> {
    let s = v.get("seed");
    if let Some(a) = s.get("tracks").as_array() {
        let ids: Vec<String> = a
            .iter()
            .filter_map(|i| i.as_str().filter(|x| util::is_id(x)).map(str::to_string))
            .take(100)
            .collect();
        if ids.is_empty() {
            return Err((400, "seed.tracks has no song".into()));
        }
        return Ok(Seed::Songs(ids));
    }
    if let Some(a) = s.get("artist").as_str() {
        return Ok(Seed::Artist(a.to_string()));
    }
    if let Some(a) = s.get("album").as_str() {
        return Ok(Seed::Album(a.to_string()));
    }
    if let Some(l) = s.get("label").as_str() {
        return Ok(Seed::Label(l.to_string()));
    }
    if s.bool_or("taste", false) {
        return Ok(Seed::Taste);
    }
    Err((
        400,
        "seed must name tracks, artist, album, label or taste".into(),
    ))
}

/// `POST /api/radio` body `{seed: {tracks | artist | album | label | taste},
/// recent?: [ids], count?, random?}`: the next songs of a radio.
pub fn radio(state: &State, login: &Login, body: &[u8]) -> Result<Value, (u16, String)> {
    let v = bl_json::parse_bytes(body).map_err(|e| (400, format!("invalid JSON: {e}")))?;
    let seed = parse_seed(&v)?;
    let recent: Vec<String> = v
        .get("recent")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i.as_str().map(str::to_string))
        .take(MAX_RECENT)
        .collect();
    let count = v
        .get("count")
        .as_u64()
        .unwrap_or(25)
        .clamp(1, MAX_COUNT as u64) as usize;
    let random = v.get("random").as_u64().unwrap_or_else(|| util::now_secs());
    let c = state.recommend.catalog(state, login);
    let t = taste(&c, login);
    let items = bl_recommend::radio(&c, &t, &seed, &recent, count, random);
    Ok(Value::object().with("items", items))
}

/// `GET /api/stats?range=week|month|year|all&utcOffset=seconds`: statistics
/// of what the login's devices sent.
pub fn stats(state: &State, login: &Login, range: &str, utc_offset: i64) -> Value {
    use bl_recommend::stats::{stats, Range, StatPlay, StatSong};
    let lib = state.library();
    let mut songs: Vec<StatSong> = Vec::new();
    let mut known: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let plays: Vec<StatPlay> = login
        .history
        .events()
        .into_iter()
        .map(|e| {
            let song = known.get(&e.id).copied().or_else(|| {
                let t = &lib.track(&e.id)?.track;
                let i = songs.len() as u32;
                songs.push(StatSong {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    artists: t.artists.clone(),
                    album: t.album.clone(),
                    album_id: t.album_id.clone(),
                    genre: t.genre.clone(),
                });
                known.insert(e.id.clone(), i);
                Some(i)
            });
            StatPlay {
                song,
                t: e.t,
                ms: e.ms,
                skip: e.skip,
            }
        })
        .collect();
    stats(
        &songs,
        &plays,
        Range::parse(range),
        util::now_secs(),
        utc_offset.clamp(-14 * 3600, 14 * 3600),
    )
}

/// `GET /api/labels`.
pub fn labels(state: &State, login: &Login) -> Value {
    let c = state.recommend.catalog(state, login);
    crate::labels::labels_json(&c, &login.labels.snapshot())
}
