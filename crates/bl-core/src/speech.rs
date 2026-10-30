//! Speech recognition on the device itself, without a server: the same
//! model list as the server's (downloaded into the app's storage when
//! picked), started per song from the song text view. A plain song text
//! gets its line and word times; without a text, what is sung is written
//! down. The result is kept as the song's text (enhanced LRC).

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::player::SourceOpener;
use bl_asr::models::{self, Models};
use bl_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Songs longer than this are recognised for their first part only.
const MAX_MINUTES: u64 = 15;
/// Share of the song text that must be recognised.
const MIN_MATCH: f32 = 0.3;

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
    /// (track id, LRC or why not).
    results: Mailbox<(String, Result<String, String>)>,
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
        Value::object()
            .with("available", available)
            .with("model", self.settings.speech_model.as_str())
            .with("ready", ready)
            .with("running", running)
            .with("error", self.speech.error.clone())
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
            "speech.cancel" => {
                if let Some(j) = &self.speech.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
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
        let Some(model_path) = self.speech_model_path(host) else {
            return Value::object().with("error", self.tr("Pick a speech model first"));
        };
        let Some(track) = self.current.clone() else {
            return Value::object().with("error", "nothing playing");
        };
        let Some(opener) = self.speech_opener(host, &track.id) else {
            return Value::object()
                .with("error", self.tr("This song cannot be read on this device"));
        };
        // A plain text to time (an already timed one needs nothing).
        let text = (self.lyrics.id == track.id)
            .then(|| self.lyrics.found.as_ref())
            .flatten()
            .filter(|(l, _)| !l.synced)
            .map(|(l, _)| {
                l.lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        self.speech.job = Some(Job {
            id: track.id.clone(),
            title: track.title.clone(),
            progress: progress.clone(),
            cancel: cancel.clone(),
        });
        self.speech.error = None;
        self.speech.rev += 1;
        let (loaded, results, id) = (
            self.speech.loaded.clone(),
            self.speech.results.clone(),
            track.id.clone(),
        );
        host.spawn(Box::new(move || {
            let res = recognize(&loaded, &model_path, opener, text, &progress, &cancel);
            results.post((id, res));
        }));
        Value::object().with("ok", true)
    }

    pub(crate) fn poll_speech(&mut self, host: &mut dyn Host) {
        for id in self.speech.picked.take() {
            self.set_setting(host, "speechModel", Value::from(id));
            *self.speech.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.speech.rev += 1;
        }
        for (id, res) in self.speech.results.take() {
            self.speech.job = None;
            self.speech.rev += 1;
            match res {
                Ok(lrc) if self.queue.current() == Some(&id) => {
                    let r = self.lyrics_import(host, &lrc);
                    if let Some(e) = r.get("error").as_str() {
                        self.speech.error = Some(e.to_string());
                    } else {
                        let msg = self.tr("The song text was timed").to_string();
                        self.toast(msg, false);
                    }
                }
                // Another song plays now: keep the result for this one.
                Ok(lrc) => {
                    host.storage_write(&crate::lyrics::import_key(&id), lrc.as_bytes());
                }
                Err(e) => {
                    self.toast(e.clone(), true);
                    self.speech.error = Some(e);
                }
            }
        }
    }
}

/// Decodes, recognises and aligns (on a worker thread).
fn recognize(
    loaded: &Loaded,
    model_path: &std::path::Path,
    opener: SourceOpener,
    text: Option<String>,
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
    let prompt = text.as_deref().map(|t| {
        let words: Vec<&str> = t.split_whitespace().collect();
        words[words.len().saturating_sub(120)..].join(" ")
    });
    let t = bl_asr::transcribe::Transcriber::new(&model);
    let mut cb = |p: f32| {
        progress.store((p.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
        !cancel.load(Ordering::Relaxed)
    };
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
    match text {
        Some(t) => {
            let (lrc, matched) = bl_asr::align::align_text_words(&t, &segments);
            if matched < MIN_MATCH {
                return Err(format!(
                    "the song text was not recognised ({:.0} % of the words)",
                    matched * 100.0
                ));
            }
            Ok(lrc)
        }
        None => Ok(bl_asr::align::segments_to_lrc_words(&segments)),
    }
}
