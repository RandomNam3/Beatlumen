//! Timing song texts with speech recognition (`bl_asr`, a Whisper model
//! given with `--speech-model`): a plain song text is aligned with what is
//! sung (`<id>.synced.lrc`); without a text the recognised words become
//! one (`<id>.recognized.lrc`). One song at a time on a background thread;
//! requests from the apps go first, the whole library follows with
//! `--auto-sync-lyrics` (on half the cores, when nothing else waits).

use crate::library::Entry;
use crate::State;
use bl_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

/// Longest song analysed (minutes).
const MAX_MINUTES: u32 = 15;
/// Share of the song text's words that must be recognised.
const MIN_MATCH: f32 = 0.25;

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
    model_path: Option<PathBuf>,
    auto: bool,
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
            model_path,
            auto,
            dir,
            inner: Mutex::default(),
            wake: Condvar::new(),
            state: OnceLock::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.model_path.is_some()
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
        if self.model_path.is_none() || self.state.set(Arc::downgrade(state)).is_err() {
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
        let mut model: Option<bl_asr::model::Model> = None;
        loop {
            let (id, background) = {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(id) = inner.queue.pop_front() {
                        break (id, false);
                    }
                    if self.auto {
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
            if model.is_none() {
                let path = self.model_path.clone().expect("checked in attach");
                match std::fs::File::open(&path)
                    .and_then(|f| bl_asr::model::Model::load(&mut std::io::BufReader::new(f)))
                {
                    Ok(m) => {
                        crate::info!("speech model {} loaded", path.display());
                        model = Some(m);
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
            let res = self.sync_one(&state, entry, model.as_ref().expect("loaded"));
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
            if [
                self.synced_path(id),
                self.recognized_path(id),
                self.failed_path(id),
            ]
            .iter()
            .any(|p| p.exists())
            {
                continue;
            }
            match state.lyrics.source_text(e) {
                Some((t, _)) if is_synced(&t) => continue,
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
    ) -> Result<(), String> {
        let id = e.track.id.clone();
        let progress = |p: f32| {
            self.set(&id, Status::Running(p));
            true
        };
        let src: Box<dyn bl_media::Source> = Box::new(std::io::BufReader::with_capacity(
            256 * 1024,
            std::fs::File::open(&e.abs).map_err(|e| e.to_string())?,
        ));
        let samples = bl_media::decode_mono(
            src,
            Some(&e.ext()),
            bl_asr::mel::SAMPLE_RATE,
            MAX_MINUTES * 60,
            &mut || true,
        )
        .map_err(|e| e.to_string())?;
        let text = state
            .lyrics
            .source_text(e)
            .map(|(t, _)| t)
            .filter(|t| !is_synced(t));
        // The last part of the text steers the recognition (prompt).
        let prompt = text.as_deref().map(|t| {
            let words: Vec<&str> = t.split_whitespace().collect();
            words[words.len().saturating_sub(120)..].join(" ")
        });
        let t = bl_asr::transcribe::Transcriber::new(model);
        let mut cb = progress;
        let segments = t
            .transcribe(
                &samples,
                bl_asr::transcribe::Options {
                    language: None,
                    prompt: prompt.as_deref(),
                    progress: &mut cb,
                },
            )
            .map_err(|e| e.to_string())?;
        if segments.is_empty() {
            return Err("no singing recognised".into());
        }
        let (lrc, path) = match &text {
            Some(t) => {
                let (lrc, matched) = bl_asr::align::align_text(t, &segments);
                if matched < MIN_MATCH {
                    return Err(format!(
                        "the song text was not recognised ({:.0} % of the words)",
                        matched * 100.0
                    ));
                }
                (lrc, self.synced_path(&id))
            }
            None => {
                let conf =
                    segments.iter().map(|s| s.avg_logprob).sum::<f32>() / segments.len() as f32;
                if conf < -1.2 {
                    return Err("the words could not be recognised reliably".into());
                }
                (
                    bl_asr::align::segments_to_lrc(&segments),
                    self.recognized_path(&id),
                )
            }
        };
        write(&self.dir, &path, &lrc)
    }
}

fn write(dir: &Path, path: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .and_then(|_| crate::util::write_atomic(path, text.as_bytes()))
        .map_err(|e| e.to_string())
}
