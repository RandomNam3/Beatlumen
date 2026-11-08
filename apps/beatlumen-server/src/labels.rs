//! Labels of the library's songs.
//!
//! * How a song sounds (tempo, key, energy, ...) is worked out in the
//!   background, one song at a time and resting as long as it worked, and
//!   kept in `data/sound.jsonl` (a cache, worked out again when missing).
//!   Automatic labels follow from that and from the tags
//!   (`bl_recommend::auto_labels`). Songs of remote sources only get those
//!   from their tags: the server would have to download all of them.
//! * Labels people give are stored per login in the database
//!   (`<scope>labels/<song id>`: the names), like playlists.

use crate::library::Entry;
use crate::sources::Sources;
use crate::store::Part;
use crate::util;
use crate::State;
use bl_json::Value;
use bl_recommend::Features;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Audio decoded of a song at most (minutes); the analysis looks at its
/// middle part only.
const MAX_MINUTES: u64 = 8;
/// Longest name of a label given by a person.
const MAX_NAME: usize = 40;
const MAX_PER_SONG: usize = 64;

pub type Failure = (u16, String);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Analysed {
    /// Modification time of the file it was worked out from.
    mtime_ns: u64,
    /// `None`: nothing to tell (silence, cannot be read); not tried again
    /// until the file changes.
    features: Option<Features>,
    /// Of the analysis it comes from (lines without one: the first).
    version: u64,
}

impl Analysed {
    fn current(&self, mtime_ns: u64) -> bool {
        self.mtime_ns == mtime_ns && self.version == bl_analysis::features::FEATURES_VERSION
    }
}

/// The features of all songs and the worker that fills them in.
pub struct SoundIndex {
    path: PathBuf,
    sources: Arc<Sources>,
    map: RwLock<HashMap<String, Analysed>>,
    enabled: AtomicBool,
    /// Bumped when results arrive (the recommendations rebuild).
    revision: AtomicU64,
    working: Mutex<Option<String>>,
}

fn features_json(id: &str, a: &Analysed) -> Value {
    let mut v = Value::object()
        .with("id", id)
        .with("mtime", a.mtime_ns)
        .with("v", a.version);
    if let Some(f) = &a.features {
        v = v
            .with("bpm", f.bpm)
            .with("beat", f.beat)
            .with("key", f.key)
            .with("minor", f.minor)
            .with("energy", f.energy)
            .with("bright", f.brightness);
    }
    v
}

fn parse_line(v: &Value) -> Option<(String, Analysed)> {
    let id = v.get("id").as_str().filter(|s| util::is_id(s))?;
    let features = v.get("bpm").as_f64().map(|bpm| Features {
        bpm: bpm as f32,
        beat: v.f32_or("beat", 0.0),
        key: v.get("key").as_u64().unwrap_or(0).min(11) as u8,
        minor: v.bool_or("minor", false),
        energy: v.f32_or("energy", 0.0),
        brightness: v.f32_or("bright", 0.0),
    });
    Some((
        id.to_string(),
        Analysed {
            mtime_ns: v.get("mtime").as_u64().unwrap_or(0),
            features,
            version: v.get("v").as_u64().unwrap_or(1),
        },
    ))
}

impl SoundIndex {
    pub fn new(path: PathBuf, sources: Arc<Sources>, enabled: bool) -> SoundIndex {
        let mut map = HashMap::new();
        if let Ok(data) = std::fs::read(&path) {
            for line in data.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                if let Some((id, a)) = bl_json::parse_bytes(line)
                    .ok()
                    .as_ref()
                    .and_then(parse_line)
                {
                    map.insert(id, a);
                }
            }
        }
        SoundIndex {
            path,
            sources,
            map: RwLock::new(map),
            enabled: AtomicBool::new(enabled),
            revision: AtomicU64::new(0),
            working: Mutex::new(None),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn features(&self, id: &str, mtime_ns: u64) -> Option<Features> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            // An older analysis still counts until the new one is done.
            .filter(|a| a.mtime_ns == mtime_ns)
            .and_then(|a| a.features)
    }

    /// For the admin page.
    pub fn summary(&self, state: &State) -> Value {
        let lib = state.library();
        let map = self.map.read().unwrap_or_else(|e| e.into_inner());
        let mut total = 0usize;
        let mut done = 0usize;
        for e in lib.tracks().iter().filter(|e| e.remote.is_none()) {
            total += 1;
            if map.get(&e.track.id).is_some_and(|a| a.current(e.mtime_ns)) {
                done += 1;
            }
        }
        Value::object()
            .with("enabled", self.enabled.load(Ordering::Relaxed))
            .with("total", total)
            .with("done", done)
            .with(
                "working",
                self.working
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some(),
            )
    }

    /// Starts the background worker.
    pub fn attach(self: &Arc<Self>, state: &Arc<State>) {
        let index = self.clone();
        let state = Arc::downgrade(state);
        let _ = std::thread::Builder::new()
            .name("labels".into())
            .spawn(move || loop {
                let Some(s) = state.upgrade() else {
                    return;
                };
                let next = if index.enabled.load(Ordering::Relaxed) && !s.scanning() {
                    index.next_song(&s)
                } else {
                    None
                };
                drop(s);
                let Some(e) = next else {
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                };
                let t0 = Instant::now();
                index.analyse(&e);
                // Half of one core at most.
                std::thread::sleep(t0.elapsed().max(Duration::from_millis(20)));
            });
    }

    fn next_song(&self, state: &State) -> Option<Entry> {
        let lib = state.library();
        let map = self.map.read().unwrap_or_else(|e| e.into_inner());
        lib.tracks()
            .iter()
            .find(|e| {
                e.remote.is_none() && map.get(&e.track.id).is_none_or(|a| !a.current(e.mtime_ns))
            })
            .cloned()
    }

    fn analyse(&self, e: &Entry) {
        *self.working.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.track.id.clone());
        let features = match self.compute(e) {
            Ok(f) => f,
            Err(err) => {
                crate::debug!("no sound analysis for {}: {err}", e.rel);
                None
            }
        };
        let a = Analysed {
            mtime_ns: e.mtime_ns,
            features,
            version: bl_analysis::features::FEATURES_VERSION,
        };
        let line = features_json(&e.track.id, &a).to_json() + "\n";
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let saved = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(err) = saved {
            crate::warn!("cannot write {}: {err}", self.path.display());
        }
        self.map
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(e.track.id.clone(), a);
        self.revision.fetch_add(1, Ordering::Relaxed);
        *self.working.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn compute(&self, e: &Entry) -> Result<Option<Features>, String> {
        let src = self.sources.open_entry(e).map_err(|f| f.1)?;
        let ext = e.ext();
        util::catch(
            "sound analysis",
            move || -> Result<Option<Features>, String> {
                let mut dec = bl_media::open(src, Some(&ext)).map_err(|err| err.to_string())?;
                let spec = dec.spec();
                if spec.sample_rate == 0 || spec.channels == 0 {
                    return Err("invalid stream".to_string());
                }
                let limit = MAX_MINUTES * 60 * spec.sample_rate as u64;
                let mut mono: Vec<f32> = Vec::new();
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match dec.next_chunk(&mut buf) {
                        Ok(more) => {
                            bl_analysis::to_mono(&buf, spec.channels as usize, &mut mono);
                            if !more || mono.len() as u64 >= limit {
                                break;
                            }
                        }
                        // Enough came through to tell.
                        Err(_) if mono.len() >= 30 * spec.sample_rate as usize => break,
                        Err(err) => return Err(err.to_string()),
                    }
                }
                Ok(
                    bl_analysis::features::song_features(&mono, spec.sample_rate).map(|f| {
                        Features {
                            bpm: f.bpm,
                            beat: f.beat,
                            key: f.key,
                            minor: f.minor,
                            energy: f.energy,
                            brightness: f.brightness,
                        }
                    }),
                )
            },
        )
        .map_err(|m| m.to_string())?
    }
}

/// Labels given by people to the songs of a login: label names per song.
pub struct SongLabels {
    part: Part,
    map: RwLock<HashMap<String, BTreeSet<String>>>,
    revision: AtomicU64,
}

/// A name a person may give: trimmed, no control characters, no ":" (those
/// belong to the automatic ones).
fn clean_name(s: &str) -> Option<String> {
    let s = s.trim().replace(':', " ");
    let s = s.trim();
    (!s.is_empty() && s.chars().count() <= MAX_NAME && !s.chars().any(char::is_control))
        .then(|| s.to_string())
}

impl SongLabels {
    pub fn load(part: Part) -> SongLabels {
        let mut map: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (id, names) in part.list_json() {
            if !util::is_id(&id) {
                continue;
            }
            let set: BTreeSet<String> = names
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().and_then(clean_name))
                .collect();
            if !set.is_empty() {
                map.insert(id, set);
            }
        }
        SongLabels {
            part,
            map: RwLock::new(map),
            revision: AtomicU64::new(0),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn of(&self, id: &str) -> Vec<String> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// All songs with labels.
    pub fn snapshot(&self) -> HashMap<String, Vec<String>> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect()
    }

    /// `PUT /api/tracks/{id}/labels` body `{add?: [..], remove?: [..]}`; the
    /// song's labels afterwards.
    pub fn change(&self, id: &str, body: &[u8]) -> Result<Vec<String>, Failure> {
        let v = bl_json::parse_bytes(body).map_err(|e| (400, format!("invalid JSON: {e}")))?;
        let names = |key: &str| -> Result<Vec<String>, Failure> {
            match v.get(key) {
                Value::Null => Ok(Vec::new()),
                Value::Array(a) => a
                    .iter()
                    .map(|n| {
                        n.as_str()
                            .and_then(clean_name)
                            .ok_or((400, "invalid label name".to_string()))
                    })
                    .collect(),
                _ => Err((400, format!("{key} must be an array"))),
            }
        };
        let add = names("add")?;
        let remove = names("remove")?;
        let mut map = self.map.write().unwrap_or_else(|e| e.into_inner());
        let mut set = map.get(id).cloned().unwrap_or_default();
        for n in remove {
            set.remove(&n);
        }
        for n in add {
            if set.len() >= MAX_PER_SONG {
                return Err((409, "too many labels".to_string()));
            }
            set.insert(n);
        }
        let result: Vec<String> = set.iter().cloned().collect();
        let saved = if set.is_empty() {
            self.part.delete(id).map(|_| ())
        } else {
            self.part.put_json(id, &Value::from(result.clone()))
        };
        saved.map_err(|e| {
            crate::error!("cannot save labels: {e}");
            (500, "cannot save labels".to_string())
        })?;
        if set.is_empty() {
            map.remove(id);
        } else {
            map.insert(id.to_string(), set);
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }
}

/// `GET /api/labels`: every label with its songs, as indexes into `names`:
/// `{"names": [...], "songs": {"<id>": [0, 3]}, "given": [...]}`. Compact
/// because it covers the whole library.
pub fn labels_json(c: &bl_recommend::Catalog, given: &HashMap<String, Vec<String>>) -> Value {
    let mut names: Vec<String> = Vec::new();
    let mut numbers: HashMap<String, u64> = HashMap::new();
    let mut songs = Value::object();
    for i in 0..c.len() as u32 {
        let mut list = Vec::new();
        for l in c.labels_of(i) {
            let n = match numbers.get(l) {
                Some(n) => *n,
                None => {
                    let n = names.len() as u64;
                    names.push(l.to_string());
                    numbers.insert(l.to_string(), n);
                    n
                }
            };
            list.push(Value::from(n));
        }
        if !list.is_empty() {
            songs = songs.with(c.id_of(i), Value::Array(list));
        }
    }
    let mut own: Vec<String> = given
        .values()
        .flatten()
        .cloned()
        .collect::<HashSet<String>>()
        .into_iter()
        .collect();
    own.sort();
    Value::object()
        .with("names", names)
        .with("songs", songs)
        .with("given", own)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "00000000000000aa";

    fn temp(name: &str) -> Part {
        Part::new(
            &crate::store::temp_store(&format!("labels-{name}")),
            "shared/labels/",
        )
    }

    #[test]
    fn labels_are_kept_and_cleaned() {
        let p = temp("keep");
        let l = SongLabels::load(p.clone());
        let r = l
            .change(A, br#"{"add":["  Party ","genre:rock"]}"#)
            .unwrap();
        // A ":" would pass for an automatic label.
        assert_eq!(r, ["Party", "genre rock"]);
        let again = SongLabels::load(p);
        assert_eq!(again.of(A), ["Party", "genre rock"]);
        assert_eq!(
            again.change(A, br#"{"remove":["Party"]}"#).unwrap(),
            ["genre rock"]
        );
        assert_eq!(
            again
                .change(A, br#"{"remove":["genre rock"]}"#)
                .unwrap()
                .len(),
            0
        );
        assert!(again.snapshot().is_empty());
    }

    #[test]
    fn bad_names_are_refused() {
        let l = SongLabels::load(temp("bad"));
        assert_eq!(l.change(A, br#"{"add":[""]}"#).unwrap_err().0, 400);
        assert_eq!(l.change(A, br#"{"add":[" : "]}"#).unwrap_err().0, 400);
        assert_eq!(l.change(A, br#"{"add":[3]}"#).unwrap_err().0, 400);
        let long = format!(r#"{{"add":["{}"]}}"#, "x".repeat(41));
        assert_eq!(l.change(A, long.as_bytes()).unwrap_err().0, 400);
        assert_eq!(l.change(A, b"nonsense").unwrap_err().0, 400);
    }

    #[test]
    fn features_lines_read_back() {
        let a = Analysed {
            mtime_ns: 77,
            features: Some(Features {
                bpm: 120.0,
                beat: 0.5,
                key: 9,
                minor: true,
                energy: 0.7,
                brightness: 0.25,
            }),
            version: 1,
        };
        let v = bl_json::parse(&features_json(A, &a).to_json()).unwrap();
        assert_eq!(parse_line(&v), Some((A.to_string(), a)));
        let none = Analysed {
            mtime_ns: 77,
            features: None,
            version: 1,
        };
        let v = bl_json::parse(&features_json(A, &none).to_json()).unwrap();
        assert_eq!(parse_line(&v), Some((A.to_string(), none)));
    }
}
