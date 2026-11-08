//! Karaoke data per song (`bl_karaoke`): the melody of the separated
//! vocals as notes on the timed song text. Imported tracks (UltraStar TXT
//! or JSON) are kept as `<id>.imported.json` and win; computed ones as
//! `<id>.json`, which go stale when the song text changes. Computing runs
//! one song at a time on a background thread once the admin turns it on
//! (`--karaoke`; the vocal separation model is downloaded then): requests
//! from the apps first, the whole library with `--auto-karaoke` (on half
//! the cores, when nothing else waits).

use crate::library::Entry;
use crate::State;
use bl_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// Largest karaoke file accepted for import (bytes).
pub const MAX_IMPORT: usize = 1 << 20;
/// How long the automatic run rests after going through the library.
const AUTO_REST: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Queued,
    Running(f32),
    Done,
    Failed(String),
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<String>,
    status: HashMap<String, Status>,
    /// Songs the automatic run looked at in this pass.
    seen: HashSet<String>,
    /// When the automatic run found nothing to do.
    rested: Option<Instant>,
    /// A request came after the model's download failed: try again.
    retry_model: bool,
}

pub struct Karaoke {
    dir: PathBuf,
    enabled: AtomicBool,
    auto: AtomicBool,
    inner: Mutex<Inner>,
    wake: Condvar,
    state: OnceLock<Weak<State>>,
}

impl Karaoke {
    pub fn new(dir: PathBuf, enabled: bool, auto: bool) -> Karaoke {
        Karaoke {
            dir,
            enabled: AtomicBool::new(enabled || auto),
            auto: AtomicBool::new(auto),
            inner: Mutex::default(),
            wake: Condvar::new(),
            state: OnceLock::new(),
        }
    }

    /// Whether this server computes karaoke data.
    pub fn available(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turns computing and the automatic run on or off (admin interface).
    pub fn configure(&self, enabled: bool, auto: bool) {
        if auto && !self.auto.swap(true, Ordering::Relaxed) {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.seen.clear();
            inner.rested = None;
        }
        self.auto.store(auto, Ordering::Relaxed);
        self.enabled.store(enabled || auto, Ordering::Relaxed);
        self.wake.notify_all();
    }

    fn imported_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.imported.json"))
    }

    fn computed_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn failed_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.failed"))
    }

    fn read(&self, path: &std::path::Path) -> Option<Value> {
        let b = std::fs::read(path).ok()?;
        bl_json::parse_bytes(&b).ok()
    }

    /// The song's karaoke track (as stored, with `source`): the imported
    /// one, else the computed one while it fits the song text `lyrics`.
    pub fn get(&self, id: &str, lyrics: Option<&str>) -> Option<Value> {
        if let Some(v) = self.read(&self.imported_path(id)) {
            return Some(v);
        }
        self.read(&self.computed_path(id))
            .filter(|v| bl_karaoke::is_current(v, lyrics))
    }

    /// Stores an imported karaoke track.
    pub fn put(&self, e: &Entry, body: &[u8]) -> Result<(), (u16, String)> {
        if body.len() > MAX_IMPORT {
            return Err((413, "karaoke file too large".into()));
        }
        let mut track = bl_karaoke::import(body).map_err(|m| (400, m))?;
        if track.title.is_empty() {
            track.title = e.track.title.clone();
        }
        if track.artist.is_empty() {
            track.artist = e.track.artists.join(", ");
        }
        let v = bl_karaoke::to_stored(&track, bl_karaoke::Source::Imported, None);
        std::fs::create_dir_all(&self.dir)
            .and_then(|_| {
                crate::util::write_atomic(&self.imported_path(&e.track.id), v.to_json().as_bytes())
            })
            .map_err(|err| (500, format!("cannot store the karaoke file: {err}")))
    }

    /// Removes the song's karaoke data (imported and computed).
    pub fn delete(&self, id: &str) {
        for p in [
            self.imported_path(id),
            self.computed_path(id),
            self.failed_path(id),
        ] {
            let _ = std::fs::remove_file(p);
        }
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .remove(id);
    }

    /// Starts the worker.
    pub fn attach(self: &Arc<Self>, state: &Arc<State>) {
        if self.state.set(Arc::downgrade(state)).is_err() {
            return;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("karaoke".into())
            .spawn(move || me.run());
    }

    /// Queues a song (first in line).
    pub fn request(&self, id: &str) -> Value {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            inner.status.get(id),
            Some(Status::Queued | Status::Running(_))
        ) {
            let _ = std::fs::remove_file(self.failed_path(id));
            inner.queue.retain(|q| q != id);
            inner.queue.push_front(id.to_string());
            inner.status.insert(id.to_string(), Status::Queued);
            inner.retry_model = true;
            self.wake.notify_all();
        }
        drop(inner);
        self.status_json(id)
    }

    pub fn status_json(&self, id: &str) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let v = Value::object();
        let v = match inner.status.get(id) {
            None => v.with("state", "idle"),
            Some(Status::Queued) => v.with("state", "queued"),
            Some(Status::Running(p)) => v.with("state", "running").with("progress", *p as f64),
            Some(Status::Done) => v.with("state", "done"),
            Some(Status::Failed(e)) => v.with("state", "failed").with("error", e.as_str()),
        };
        // The separation model on its way.
        match self.model_download() {
            Some(d) => v.with("model", d),
            None => v,
        }
    }

    /// What the admin interface shows.
    pub fn summary(&self) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let count = |f: fn(&Status) -> bool| inner.status.values().filter(|s| f(s)).count() as u64;
        let (mut computed, mut imported) = (0u64, 0u64);
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.ends_with(".imported.json") {
                imported += 1;
            } else if n.ends_with(".json") {
                computed += 1;
            }
        }
        let v = Value::object()
            .with("enabled", self.available())
            .with("auto", self.auto.load(Ordering::Relaxed))
            .with("modelReady", self.model_path().is_some())
            .with("queued", inner.queue.len() as u64)
            .with("done", count(|s| matches!(s, Status::Done)))
            .with("failed", count(|s| matches!(s, Status::Failed(_))))
            .with("computed", computed)
            .with("imported", imported);
        drop(inner);
        match self.model_download() {
            Some(d) => v.with("model", d),
            None => v,
        }
    }

    fn set(&self, id: &str, s: Status) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .insert(id.to_string(), s);
    }

    /// The separation model's file, when it is there.
    fn model_path(&self) -> Option<PathBuf> {
        let state = self.state.get()?.upgrade()?;
        let p = state
            .models
            .dir
            .join(bl_sep::MODEL_ID)
            .join(bl_sep::MODEL_FILE);
        p.is_file().then_some(p)
    }

    /// The separation model's download while it runs or failed.
    fn model_download(&self) -> Option<Value> {
        let state = self.state.get()?.upgrade()?;
        Some(match state.models.state(bl_sep::MODEL_ID)? {
            crate::models::Download::Running { done, total } => Value::object()
                .with("state", "downloading")
                .with("done", done)
                .with("total", total),
            crate::models::Download::Checking => Value::object().with("state", "checking"),
            crate::models::Download::Failed(e) => Value::object()
                .with("state", "failed")
                .with("error", e.as_str()),
        })
    }

    /// Starts downloading the separation model (once).
    fn fetch_model(self: &Arc<Self>, state: &State) {
        let me = self.clone();
        state
            .models
            .download_files(bl_sep::MODEL_ID, bl_sep::model_files(), move |_| {
                me.wake.notify_all()
            });
    }

    fn run(self: Arc<Self>) {
        let mut sep: Option<bl_sep::Separator> = None;
        loop {
            let (id, background) = {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if !self.available() {
                        sep = None;
                    }
                    let can = self.available() && self.model_path().is_some();
                    if can {
                        if let Some(id) = inner.queue.pop_front() {
                            break (id, false);
                        }
                        if self.auto.load(Ordering::Relaxed) {
                            if inner.rested.is_some_and(|t| t.elapsed() >= AUTO_REST) {
                                inner.seen.clear();
                                inner.rested = None;
                            }
                            if inner.rested.is_none() {
                                if let Some(id) = self.next_automatic(&mut inner) {
                                    break (id, true);
                                }
                                inner.rested = Some(Instant::now());
                            }
                        }
                    } else if self.available() {
                        // The model first.
                        if let Some(state) = self.state.get().and_then(Weak::upgrade) {
                            match state.models.state(bl_sep::MODEL_ID) {
                                Some(crate::models::Download::Failed(e)) if !inner.retry_model => {
                                    // Requests waiting for it fail; a new one tries again.
                                    let waiting: Vec<String> = inner.queue.drain(..).collect();
                                    for id in waiting {
                                        inner.status.insert(
                                            id,
                                            Status::Failed(format!("vocal separation model: {e}")),
                                        );
                                    }
                                }
                                Some(
                                    crate::models::Download::Running { .. }
                                    | crate::models::Download::Checking,
                                ) => {}
                                _ => {
                                    inner.retry_model = false;
                                    drop(inner);
                                    self.fetch_model(&state);
                                    inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                                }
                            }
                        }
                    }
                    inner = self
                        .wake
                        .wait_timeout(inner, Duration::from_secs(if can { 60 } else { 5 }))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            };
            let Some(state) = self.state.get().and_then(Weak::upgrade) else {
                return;
            };
            if sep.is_none() {
                let Some(path) = self.model_path() else {
                    self.set(&id, Status::Failed("no vocal separation model".into()));
                    continue;
                };
                match bl_sep::Separator::load(&path) {
                    Ok(s) => sep = Some(s),
                    Err(e) => {
                        crate::warn!("cannot load the vocal separation model: {e}");
                        self.set(&id, Status::Failed(format!("vocal separation model: {e}")));
                        continue;
                    }
                }
            }
            let idx = state.library();
            let Some(entry) = idx.track(&id) else {
                self.set(&id, Status::Failed("song not found".into()));
                continue;
            };
            let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
            bl_asr::ops::set_max_threads(if background { (cores / 2).max(1) } else { 0 });
            self.set(&id, Status::Running(0.0));
            let res = self.compute_one(&state, entry, sep.as_ref().expect("loaded"));
            match res {
                Ok(()) => self.set(&id, Status::Done),
                Err(e) => {
                    if background {
                        let _ = std::fs::write(self.failed_path(&id), e.as_bytes());
                    }
                    crate::info!("karaoke data of {id} not computed: {e}");
                    self.set(&id, Status::Failed(e));
                }
            }
        }
    }

    /// The song text to compute from: one with times.
    fn timed_text(state: &State, e: &Entry) -> Option<String> {
        let v = state.lyrics.get(e)?;
        let text = v.get("text").as_str()?.to_string();
        crate::lyrics_sync::is_synced(&text).then_some(text)
    }

    /// The next song to compute (automatic run).
    fn next_automatic(&self, inner: &mut Inner) -> Option<String> {
        let state = self.state.get()?.upgrade()?;
        let idx = state.library();
        for e in idx.tracks() {
            let id = &e.track.id;
            if !inner.seen.insert(id.clone()) {
                continue;
            }
            if self.failed_path(id).exists() || self.imported_path(id).exists() {
                continue;
            }
            let Some(text) = Self::timed_text(&state, e) else {
                continue;
            };
            // Word times come with the speech timing: wait for them.
            if !bl_asr::sync::has_word_times(&text) && state.lyrics_sync.available() {
                continue;
            }
            let current = self
                .read(&self.computed_path(id))
                .is_some_and(|v| bl_karaoke::is_current(&v, Some(&text)));
            if !current {
                return Some(id.clone());
            }
        }
        None
    }

    fn compute_one(&self, state: &State, e: &Entry, sep: &bl_sep::Separator) -> Result<(), String> {
        let id = e.track.id.clone();
        let text = Self::timed_text(state, e).ok_or("no timed song text")?;
        let src = state.sources.open_entry(e).map_err(|(_, m)| m)?;
        let audio = bl_karaoke::decode(src, Some(&e.ext()), &mut || true)?;
        let mut progress = |p: f32| {
            self.set(&id, Status::Running(p));
            true
        };
        let (mut track, report) = bl_karaoke::compute(sep, &audio, &text, &mut progress)?;
        if track.title.is_empty() {
            track.title = e.track.title.clone();
        }
        if track.artist.is_empty() {
            track.artist = e.track.artists.join(", ");
        }
        crate::info!(
            "karaoke data of {id}: {} notes, {:.0} % voiced, separated in {:.1} s",
            report.notes.notes,
            report.notes.voiced_share * 100.0,
            report.separation_secs
        );
        let v = bl_karaoke::to_stored(&track, bl_karaoke::Source::Computed, Some(&text));
        std::fs::create_dir_all(&self.dir)
            .and_then(|_| {
                crate::util::write_atomic(&self.computed_path(&id), v.to_json().as_bytes())
            })
            .map_err(|e| e.to_string())
    }
}
