//! Speech recognition on the device itself, without a server: the same
//! model list as the server's (downloaded into the app's storage when
//! picked), started per song from the song text view or for all songs of
//! this device. A song text (plain, or LRC with line times) gets word
//! times; without a text, what is sung is written down. The result is
//! kept as the song's text on this device (enhanced LRC, naming the model
//! in its `[re:]` tag: a run over all songs skips what the same or a
//! better model did).

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::player::SourceOpener;
use bl_asr::models::{self, Models};
use bl_json::Value;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Songs longer than this are recognised for their first part only.
const MAX_MINUTES: u64 = 15;

type Loaded = Arc<Mutex<Option<(PathBuf, Arc<bl_asr::model::Model>)>>>;

struct Job {
    id: String,
    title: String,
    /// Progress in thousandths.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct SpeechState {
    models: Option<Arc<Models>>,
    /// Models whose download finished (ids).
    picked: Mailbox<String>,
    job: Option<Job>,
    /// (track id, LRC, None when nothing was to be done, or why not).
    results: Mailbox<(String, Result<Option<String>, String>)>,
    /// Songs of this device still to do in a run over all of them, and
    /// how many that run had.
    queue: VecDeque<String>,
    batch_total: usize,
    loaded: Loaded,
    error: Option<String>,
    pub(crate) rev: u64,
}

/// Where the app keeps its models.
fn models_dir(host: &dyn Host) -> Option<PathBuf> {
    host.storage_path("models/.keep")
        .and_then(|p| PathBuf::from(p).parent().map(PathBuf::from))
}

impl Core {
    fn speech_models(&mut self, host: &dyn Host) -> Option<Arc<Models>> {
        if self.speech.models.is_none() {
            self.speech.models = Some(Arc::new(Models::new(models_dir(host)?)));
        }
        self.speech.models.clone()
    }

    /// The picked model's file, when it is there.
    fn speech_model_path(&self, host: &dyn Host) -> Option<PathBuf> {
        let m = models::find(&self.settings.speech_model)?;
        let path = models_dir(host)?.join(m.file);
        path.is_file().then_some(path)
    }

    pub(crate) fn speech_json(&self, host: &dyn Host) -> Value {
        let available = host.has_threads() && host.platform() != "web";
        let ready = available && self.speech_model_path(host).is_some();
        let running = self.speech.job.as_ref().map(|j| {
            Value::object()
                .with("id", j.id.as_str())
                .with("title", j.title.as_str())
                .with(
                    "progress",
                    j.progress.load(Ordering::Relaxed) as f64 / 1000.0,
                )
        });
        let batch = (self.speech.batch_total > 0).then(|| {
            Value::object()
                .with("total", self.speech.batch_total as u64)
                .with("left", self.speech.queue.len() as u64)
        });
        Value::object()
            .with("available", available)
            .with("model", self.settings.speech_model.as_str())
            .with("ready", ready)
            .with("running", running)
            .with("all", batch)
            .with("error", self.speech.error.clone())
            .with("gpu", bl_asr::net::gpu_device())
            .with("rev", self.speech.rev)
    }

    /// The current track's audio, when the device can read it itself.
    fn speech_opener(&self, host: &dyn Host, id: &str) -> Option<SourceOpener> {
        if let Some(path) = crate::library::local_path(id).map(str::to_string) {
            let ext = crate::jobs::extension(&path);
            return Some(Box::new(move || {
                let f = std::fs::File::open(&path).map_err(|e| format!("{path}: {e}"))?;
                Ok((
                    Box::new(std::io::BufReader::new(f)) as Box<dyn bl_media::Source>,
                    ext,
                ))
            }));
        }
        if id.starts_with(crate::sources::ID_PREFIX) {
            return self.source_opener(id);
        }
        if id.starts_with(crate::vault::ID_PREFIX) {
            return self.vault_opener(host, id);
        }
        self.offline_opener(id)
    }

    /// `speech.*` commands.
    pub(crate) fn speech_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let err = |m: &str| Value::object().with("error", m);
        let Some(models) = self.speech_models(host).filter(|_| host.has_threads()) else {
            return err("not available here");
        };
        match name {
            "speech.list" => {
                let current = self.speech_model_path(host);
                models.list(current.as_deref())
            }
            "speech.pick" => {
                let id = c.str_or("id", "").to_string();
                if id.is_empty() {
                    self.set_setting(host, "speechModel", Value::from(""));
                    *self.speech.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    return Value::object().with("ok", true);
                }
                let Some(m) = models::find(&id) else {
                    return err("no such model");
                };
                let picked = self.speech.picked.clone();
                models.download(m, move |_| picked.post(id));
                self.speech.rev += 1;
                Value::object().with("ok", true)
            }
            "speech.delete" => {
                let Some(m) = models::find(c.str_or("id", "")) else {
                    return err("no such model");
                };
                if self.settings.speech_model == m.id {
                    self.set_setting(host, "speechModel", Value::from(""));
                    *self.speech.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                models.delete(m);
                self.speech.rev += 1;
                Value::object().with("ok", true)
            }
            "speech.run" => self.speech_run(host),
            "speech.runAll" => self.speech_run_all(host),
            "speech.cancel" => {
                self.speech.queue.clear();
                self.speech.batch_total = 0;
                if let Some(j) = &self.speech.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                self.speech.rev += 1;
                Value::object().with("ok", true)
            }
            _ => err("unknown command"),
        }
    }

    /// Times (or writes down) the current track's song text on the device.
    fn speech_run(&mut self, host: &mut dyn Host) -> Value {
        if self.speech.job.is_some() {
            return Value::object().with("error", self.tr("Already working on a song"));
        }
        if self.speech_model_path(host).is_none() {
            return Value::object().with("error", self.tr("Pick a speech model first"));
        }
        let Some(track) = self.current.clone() else {
            return Value::object().with("error", "nothing playing");
        };
        // The song text shown (plain or with line times; one with word
        // times is timed again from its lines).
        let text = (self.lyrics.id == track.id)
            .then(|| self.lyrics.found.as_ref())
            .flatten()
            .map(|(l, _)| l.to_lrc());
        match self.speech_start(host, &track.id, &track.title, text, true) {
            Ok(()) => Value::object().with("ok", true),
            Err(e) => Value::object().with("error", e),
        }
    }

    /// Times the song texts of all songs of this device, one after the
    /// other (skipping what is done already).
    fn speech_run_all(&mut self, host: &mut dyn Host) -> Value {
        if self.speech_model_path(host).is_none() {
            return Value::object().with("error", self.tr("Pick a speech model first"));
        }
        let queue: VecDeque<String> = self
            .library
            .tracks
            .iter()
            .filter(|t| crate::library::local_path(&t.id).is_some())
            .filter(|t| !self.speech_done(host, &t.id))
            .map(|t| t.id.clone())
            .collect();
        if queue.is_empty() {
            return Value::object().with("error", self.tr("All songs on this device are done"));
        }
        self.speech.batch_total = queue.len();
        self.speech.queue = queue;
        self.speech.rev += 1;
        self.speech_next(host);
        Value::object()
            .with("ok", true)
            .with("songs", self.speech.batch_total as u64)
    }

    /// Whether the text kept on this device for `id` was timed by this
    /// timing with the same or a better model.
    fn speech_done(&self, host: &dyn Host, id: &str) -> bool {
        host.storage_read(&crate::lyrics::import_key(id))
            .map(|b| bl_media::lyrics::decode_text(&b))
            .and_then(|t| bl_asr::sync::made_with(&t))
            .is_some_and(|(v, m)| {
                v >= bl_asr::sync::VERSION
                    && models::rank(&m) >= models::rank(&self.settings.speech_model)
            })
    }

    /// Starts the next song of a run over all songs.
    fn speech_next(&mut self, host: &mut dyn Host) {
        while self.speech.job.is_none() {
            let Some(id) = self.speech.queue.pop_front() else {
                self.speech.batch_total = 0;
                self.speech.rev += 1;
                return;
            };
            let title = self.track_for(&id).title;
            // A text kept on this device (else the file's own, read there).
            let text = host
                .storage_read(&crate::lyrics::import_key(&id))
                .map(|b| bl_media::lyrics::decode_text(&b));
            if let Err(e) = self.speech_start(host, &id, &title, text, false) {
                host.log(&format!("speech: {title}: {e}"));
            }
        }
    }

    /// Starts timing `id` on a worker thread. `text`: the song text to time
    /// (None: the file's own, if any); `asked`: started for this song by
    /// the user (else a text with word times is left as it is).
    fn speech_start(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        title: &str,
        text: Option<String>,
        asked: bool,
    ) -> Result<(), String> {
        let Some(model_path) = self.speech_model_path(host) else {
            return Err(self.tr("Pick a speech model first").to_string());
        };
        let Some(opener) = self.speech_opener(host, id) else {
            return Err(self
                .tr("This song cannot be read on this device")
                .to_string());
        };
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        self.speech.job = Some(Job {
            id: id.to_string(),
            title: title.to_string(),
            progress: progress.clone(),
            cancel: cancel.clone(),
        });
        self.speech.error = None;
        self.speech.rev += 1;
        let (loaded, results, id) = (
            self.speech.loaded.clone(),
            self.speech.results.clone(),
            id.to_string(),
        );
        let file = crate::library::local_path(&id).map(PathBuf::from);
        let model_id = self.settings.speech_model.clone();
        let title = title.to_string();
        host.spawn(Box::new(move || {
            let text = text.or_else(|| {
                file.as_deref()
                    .and_then(bl_media::lyrics::find_lyrics)
                    .map(|(t, _)| t)
            });
            // Word times from elsewhere (not this timing) are kept.
            let keep = !asked
                && text.as_deref().is_some_and(|t| {
                    bl_asr::sync::has_word_times(t) && bl_asr::sync::made_with(t).is_none()
                });
            let res = if keep {
                Ok(None)
            } else {
                recognize(
                    &loaded,
                    &model_path,
                    &model_id,
                    opener,
                    text,
                    &title,
                    &progress,
                    &cancel,
                )
                .map(Some)
            };
            results.post((id, res));
        }));
        Ok(())
    }

    pub(crate) fn poll_speech(&mut self, host: &mut dyn Host) {
        for id in self.speech.picked.take() {
            self.set_setting(host, "speechModel", Value::from(id));
            *self.speech.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.speech.rev += 1;
        }
        let mut finished = false;
        for (id, res) in self.speech.results.take() {
            self.speech.job = None;
            self.speech.rev += 1;
            finished = true;
            let batch = self.speech.batch_total > 0;
            match res {
                Ok(None) => {}
                Ok(Some(lrc)) if self.queue.current() == Some(&id) => {
                    let r = self.lyrics_import(host, &lrc);
                    if let Some(e) = r.get("error").as_str() {
                        self.speech.error = Some(e.to_string());
                    } else if !batch {
                        let msg = self.tr("The song text was timed").to_string();
                        self.toast(msg, false);
                    }
                }
                // Another song plays now: keep the result for this one.
                Ok(Some(lrc)) => {
                    host.storage_write(&crate::lyrics::import_key(&id), lrc.as_bytes());
                }
                Err(e) if batch => host.log(&format!("speech: {id}: {e}")),
                Err(e) => {
                    self.toast(e.clone(), true);
                    self.speech.error = Some(e);
                }
            }
        }
        if finished && !self.speech.queue.is_empty() {
            self.speech_next(host);
        } else if finished && self.speech.batch_total > 0 && self.speech.job.is_none() {
            self.speech.batch_total = 0;
            let msg = self.tr("All songs on this device are done").to_string();
            self.toast(msg, false);
        }
    }
}

/// Loads the model (kept for the next song), decodes and times the song
/// (on a worker thread).
fn recognize(
    loaded: &Loaded,
    model_path: &std::path::Path,
    model_id: &str,
    opener: SourceOpener,
    text: Option<String>,
    title: &str,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let model = {
        let mut l = loaded.lock().unwrap_or_else(|e| e.into_inner());
        match l.as_ref().filter(|(p, _)| p == model_path) {
            Some((_, m)) => m.clone(),
            None => {
                let f = std::fs::File::open(model_path).map_err(|e| e.to_string())?;
                let m = Arc::new(
                    bl_asr::model::Model::load(&mut std::io::BufReader::new(f))
                        .map_err(|e| e.to_string())?,
                );
                *l = Some((model_path.to_path_buf(), m.clone()));
                m
            }
        }
    };
    let (src, ext) = opener()?;
    let samples = bl_media::decode_mono(
        src,
        ext.as_deref(),
        bl_asr::mel::SAMPLE_RATE,
        (MAX_MINUTES * 60) as u32,
        &mut || !cancel.load(Ordering::Relaxed),
    )
    .map_err(|e| e.to_string())?;
    // The device stays usable: half of the cores.
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    bl_asr::ops::set_max_threads((cores / 2).max(1));
    let mut cb = |p: f32| {
        progress.store((p.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
        !cancel.load(Ordering::Relaxed)
    };
    bl_asr::sync::time_song(
        &model,
        model_id,
        &samples,
        text.as_deref(),
        Some(title),
        &mut cb,
    )
    .map(|t| t.lrc)
}
