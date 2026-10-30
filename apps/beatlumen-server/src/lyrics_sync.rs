//! Timing song texts with speech recognition (`bl_asr`, a Whisper model
//! given with `--speech-model`): a song text (plain, or LRC with line
//! times only) gets word times (`<id>.synced.lrc`); without a text the
//! recognised words become one (`<id>.recognized.lrc`). One song at a time
//! on a background thread; requests from the apps go first, the whole
//! library follows with `--auto-sync-lyrics` (on half the cores, when
//! nothing else waits) and skips songs already timed by this timing with
//! the same or a better model (named in the files' `[re:]` tag).

use crate::library::Entry;
use crate::State;
use bl_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, Weak};

/// Longest song analysed (minutes).
const MAX_MINUTES: u32 = 15;

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
    /// Songs the automatic run looked at already (this process).
    seen: HashSet<String>,
}

pub struct LyricsSync {
    /// The model in use (changed in the admin interface).
    model_path: RwLock<Option<PathBuf>>,
    auto: AtomicBool,
    /// The worker holds a model in memory.
    loaded: AtomicBool,
    dir: PathBuf,
    inner: Mutex<Inner>,
    wake: Condvar,
    state: OnceLock<Weak<State>>,
}

pub fn is_synced(text: &str) -> bool {
    text.lines().any(|l| {
        let b = l.trim_start().as_bytes();
        b.len() > 4 && b[0] == b'[' && b[1].is_ascii_digit() && l.contains(':') && l.contains(']')
    })
}

impl LyricsSync {
    pub fn new(model_path: Option<PathBuf>, auto: bool, dir: PathBuf) -> LyricsSync {
        LyricsSync {
            model_path: RwLock::new(model_path),
            auto: AtomicBool::new(auto),
            loaded: AtomicBool::new(false),
            dir,
            inner: Mutex::default(),
            wake: Condvar::new(),
            state: OnceLock::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.model().is_some_and(|p| p.is_file())
    }

    pub fn model(&self) -> Option<PathBuf> {
        self.model_path
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Switches the model and the automatic run (admin interface).
    pub fn configure(&self, model: Option<PathBuf>, auto: bool) {
        let changed = {
            let mut m = self.model_path.write().unwrap_or_else(|e| e.into_inner());
            let changed = *m != model;
            *m = model;
            changed
        };
        // Turned on, or another model: every song is looked at again.
        if auto && (!self.auto.swap(true, Ordering::Relaxed) || changed) {
            self.inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .seen
                .clear();
        }
        self.auto.store(auto, Ordering::Relaxed);
        self.wake.notify_all();
    }

    /// Lets the automatic run try the songs that failed before.
    pub fn retry_failed(&self) -> usize {
        let mut n = 0;
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().ends_with(".failed")
                && std::fs::remove_file(e.path()).is_ok()
            {
                n += 1;
            }
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.seen.clear();
        inner.status.retain(|_, s| !matches!(s, Status::Failed(_)));
        drop(inner);
        self.wake.notify_all();
        n
    }

    /// What the admin interface shows: model, automatic run, queue.
    pub fn summary(&self) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let running = inner.status.iter().find_map(|(id, s)| match s {
            Status::Running(p) => Some(
                Value::object()
                    .with("id", id.as_str())
                    .with("progress", *p as f64),
            ),
            _ => None,
        });
        let count = |f: fn(&Status) -> bool| inner.status.values().filter(|s| f(s)).count() as u64;
        let failed_files = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".failed"))
            .count() as u64;
        let timed = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.ends_with(".synced.lrc") || n.ends_with(".recognized.lrc")
            })
            .count() as u64;
        Value::object()
            .with(
                "model",
                self.model()
                    .map_or(Value::Null, |p| Value::from(p.display().to_string())),
            )
            .with("available", self.available())
            .with("loaded", self.loaded.load(Ordering::Relaxed))
            .with("auto", self.auto.load(Ordering::Relaxed))
            .with("queued", inner.queue.len() as u64)
            .with("running", running.unwrap_or(Value::Null))
            .with("done", count(|s| matches!(s, Status::Done)))
            .with(
                "failed",
                failed_files.max(count(|s| matches!(s, Status::Failed(_)))),
            )
            .with("timed", timed)
    }

    pub fn synced_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.synced.lrc"))
    }

    pub fn recognized_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.recognized.lrc"))
    }

    fn failed_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.failed"))
    }

    /// Forgets computed timings (e.g. after a new song text was imported).
    pub fn forget(&self, id: &str) {
        for p in [
            self.synced_path(id),
            self.recognized_path(id),
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

    /// Starts the worker (with a model).
    pub fn attach(self: &Arc<Self>, state: &Arc<State>) {
        if self.state.set(Arc::downgrade(state)).is_err() {
            return;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("lyrics-sync".into())
            .spawn(move || me.run());
    }

    /// Queues a song (first in line).
    pub fn request(&self, id: &str) -> Status {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            inner.status.get(id),
            Some(Status::Queued | Status::Running(_))
        ) {
            return inner.status[id].clone();
        }
        let _ = std::fs::remove_file(self.failed_path(id));
        inner.queue.retain(|q| q != id);
        inner.queue.push_front(id.to_string());
        inner.status.insert(id.to_string(), Status::Queued);
        self.wake.notify_all();
        Status::Queued
    }

    pub fn status_json(&self, id: &str) -> Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let v = Value::object();
        match inner.status.get(id) {
            None => v.with("state", "idle"),
            Some(Status::Queued) => v.with("state", "queued"),
            Some(Status::Running(p)) => v.with("state", "running").with("progress", *p as f64),
            Some(Status::Done) => v.with("state", "done"),
            Some(Status::Failed(e)) => v.with("state", "failed").with("error", e.as_str()),
        }
    }

    fn set(&self, id: &str, s: Status) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .insert(id.to_string(), s);
    }

    fn run(self: Arc<Self>) {
        let mut model: Option<(PathBuf, bl_asr::model::Model)> = None;
        loop {
            let (id, background) = {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    // A model that was switched off leaves memory.
                    if model.is_some() && self.model().is_none() {
                        model = None;
                        self.loaded.store(false, Ordering::Relaxed);
                    }
                    if let Some(id) = inner.queue.pop_front() {
                        break (id, false);
                    }
                    if self.auto.load(Ordering::Relaxed) && self.available() {
                        if let Some(id) = self.next_automatic(&mut inner) {
                            break (id, true);
                        }
                    }
                    inner = self
                        .wake
                        .wait_timeout(inner, std::time::Duration::from_secs(60))
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            };
            let Some(state) = self.state.get().and_then(Weak::upgrade) else {
                return;
            };
            let idx = state.library();
            let Some(entry) = idx.track(&id) else {
                self.set(&id, Status::Failed("song not found".into()));
                continue;
            };
            let Some(path) = self.model() else {
                self.set(&id, Status::Failed("no speech model".into()));
                continue;
            };
            if !matches!(&model, Some((p, _)) if *p == path) {
                model = None;
                self.loaded.store(false, Ordering::Relaxed);
                match std::fs::File::open(&path)
                    .and_then(|f| bl_asr::model::Model::load(&mut std::io::BufReader::new(f)))
                {
                    Ok(m) => {
                        crate::info!(
                            "speech model {} loaded ({} MB)",
                            path.display(),
                            m.weight_bytes() / 1_000_000
                        );
                        model = Some((path, m));
                        self.loaded.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        crate::warn!("cannot load the speech model {}: {e}", path.display());
                        self.set(&id, Status::Failed(format!("speech model: {e}")));
                        continue;
                    }
                }
            }
            let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
            bl_asr::ops::set_max_threads(if background { (cores / 2).max(1) } else { 0 });
            self.set(&id, Status::Running(0.0));
            let (path, m) = model.as_ref().expect("loaded");
            let res = self.sync_one(&state, entry, m, &bl_asr::models::id_of(path));
            match res {
                Ok(()) => self.set(&id, Status::Done),
                Err(e) => {
                    if background {
                        let _ = std::fs::write(self.failed_path(&id), e.as_bytes());
                    }
                    crate::info!("song text of {id} not timed: {e}");
                    self.set(&id, Status::Failed(e));
                }
            }
        }
    }

    /// The next song without a timed text (automatic run).
    fn next_automatic(&self, inner: &mut Inner) -> Option<String> {
        let state = self.state.get()?.upgrade()?;
        let idx = state.library();
        for e in idx.tracks() {
            let id = &e.track.id;
            if inner.seen.contains(id) {
                continue;
            }
            inner.seen.insert(id.clone());
            if self.failed_path(id).exists() {
                continue;
            }
            // Made by this timing already: again only with a better model.
            let done = [self.synced_path(id), self.recognized_path(id)]
                .iter()
                .find_map(|p| std::fs::read_to_string(p).ok());
            if let Some(done) = done {
                let current = self.model().map(|p| bl_asr::models::id_of(&p));
                let good = bl_asr::sync::made_with(&done).is_some_and(|(v, m)| {
                    v >= bl_asr::sync::VERSION
                        && current.as_deref().map_or(true, |c| {
                            bl_asr::models::rank(&m) >= bl_asr::models::rank(c)
                        })
                });
                if good {
                    continue;
                }
                return Some(id.clone());
            }
            match state.lyrics.source_text(e) {
                // A text with word times needs nothing.
                Some((t, _)) if bl_asr::sync::has_word_times(&t) => continue,
                _ => return Some(id.clone()),
            }
        }
        None
    }

    fn sync_one(
        &self,
        state: &State,
        e: &Entry,
        model: &bl_asr::model::Model,
        model_id: &str,
    ) -> Result<(), String> {
        let id = e.track.id.clone();
        let mut progress = |p: f32| {
            self.set(&id, Status::Running(p));
            true
        };
        let src = state.sources.open_entry(e).map_err(|(_, m)| m)?;
        let samples = bl_media::decode_mono(
            src,
            Some(&e.ext()),
            bl_asr::mel::SAMPLE_RATE,
            MAX_MINUTES * 60,
            &mut || true,
        )
        .map_err(|e| e.to_string())?;
        // The song's own text (plain or with line times); a text with word
        // times is timed again from its lines.
        let text = state.lyrics.source_text(e).map(|(t, _)| t);
        let timed = bl_asr::sync::time_song(
            model,
            model_id,
            &samples,
            text.as_deref(),
            Some(&e.track.title),
            None,
            &mut progress,
        )?;
        let path = if timed.written_down {
            self.recognized_path(&id)
        } else {
            self.synced_path(&id)
        };
        // One computed text per song.
        for other in [self.synced_path(&id), self.recognized_path(&id)] {
            if other != path {
                let _ = std::fs::remove_file(other);
            }
        }
        write(&self.dir, &path, &timed.lrc)
    }
}

fn write(dir: &Path, path: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .and_then(|_| crate::util::write_atomic(path, text.as_bytes()))
        .map_err(|e| e.to_string())
}
