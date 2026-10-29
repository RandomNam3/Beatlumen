//! Music from server extensions (e.g. YouTube, `docs/EXTENSIONS.md`): search
//! through the server, play a result right away while the server imports
//! it, then switch to the library track (full analysis) or keep the song
//! in the user's encrypted files.

use crate::core::{Core, Req};
use crate::host::{Host, HttpResponse};
use bl_json::Value;
use bl_net::api::Track;

/// Track id prefix of songs still being imported (followed by the job id).
pub(crate) const ID_PREFIX: &str = "ext:";

/// Seconds between job status checks.
const POLL: f64 = 1.5;

#[derive(Clone, Debug)]
pub(crate) enum ExtReq {
    List,
    Search(u64, String),
    Import { keep: bool, item: Value },
    Job(String),
    Download(String),
}

struct Job {
    job: String,
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
}

#[derive(Default)]
pub(crate) struct ExtState {
    /// (id, name) of the server's extensions that search.
    list: Vec<(String, String)>,
    query: String,
    serial: u64,
    pending: usize,
    results: Vec<Value>,
    error: Option<String>,
    rev: u64,
    jobs: Vec<Job>,
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
        let id = self.server.get(host, "/api/extensions");
        self.requests.insert(id, Req::Ext(ExtReq::List));
    }

    pub(crate) fn extensions_json(&self) -> Value {
        let list: Vec<Value> = self
            .ext
            .list
            .iter()
            .map(|(id, name)| {
                Value::object()
                    .with("id", id.as_str())
                    .with("name", name.as_str())
            })
            .collect();
        Value::object()
            .with("list", Value::Array(list))
            .with("searching", self.ext.pending > 0)
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
                let exts: Vec<String> = self.ext.list.iter().map(|(id, _)| id.clone()).collect();
                for ext in exts {
                    let path = format!(
                        "/api/extensions/{ext}/search?q={}&limit=25",
                        bl_net::http::percent_encode(&q)
                    );
                    let id = self.server.get(host, &path);
                    self.requests
                        .insert(id, Req::Ext(ExtReq::Search(self.ext.serial, ext)));
                    self.ext.pending += 1;
                }
                Value::object().with("ok", true)
            }
            "extensions.results" => Value::Array(self.ext.results.clone()),
            "extensions.play" => self.extensions_play(host, c),
            _ => Value::object().with("error", format!("unknown command {name}")),
        }
    }

    /// Plays a search result: a known library track directly, else the
    /// server imports it (`keep`: into the library; else for the user's
    /// encrypted files) and it plays while that runs.
    fn extensions_play(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let ext = c.str_or("ext", "").to_string();
        let item = c.str_or("id", "").to_string();
        let Some(result) = self
            .ext
            .results
            .iter()
            .find(|r| r.str_or("ext", "") == ext && r.str_or("id", "") == item)
            .cloned()
        else {
            return Value::object().with("error", "unknown result");
        };
        if let Some(track) = result.get("track").as_str().filter(|t| !t.is_empty()) {
            let track = track.to_string();
            self.add_result_track(&track, &result);
            self.play_tracks(host, vec![track], 0);
            return Value::object().with("ok", true);
        }
        let target = match c.str_or("target", "") {
            "" => self.settings.extension_target.clone(),
            t => {
                if c.get("remember").as_bool().unwrap_or(false) {
                    self.settings.extension_target = t.to_string();
                    self.settings_changed();
                }
                t.to_string()
            }
        };
        let keep = match target.as_str() {
            "library" => true,
            "vault" => false,
            _ => return Value::object().with("ask", true),
        };
        if !keep && !self.vault_unlocked() {
            let msg = self.tr("Unlock your encrypted files first").to_string();
            self.toast(msg, true);
            return Value::object().with("error", "vault locked");
        }
        if !safe_id(&ext) || !safe_id(&item) {
            return Value::object().with("error", "invalid id");
        }
        let body = Value::object()
            .with("id", item.as_str())
            .with("keep", keep)
            .to_json()
            .into_bytes();
        let id = self.server.send(
            host,
            "POST",
            &format!("/api/extensions/{ext}/import"),
            body,
            "application/json",
        );
        self.requests
            .insert(id, Req::Ext(ExtReq::Import { keep, item: result }));
        let msg = self.tr("Loading…").to_string();
        self.toast(msg, false);
        Value::object().with("ok", true)
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
            ExtReq::List => {
                self.ext.list = json
                    .as_ref()
                    .and_then(|v| v.get("extensions").as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter(|e| e.get("search").as_bool().unwrap_or(false))
                    .filter(|e| safe_id(e.str_or("id", "")))
                    .map(|e| {
                        (
                            e.str_or("id", "").to_string(),
                            e.str_or("name", "").to_string(),
                        )
                    })
                    .collect();
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
                            let image = it
                                .get("image")
                                .as_str()
                                .filter(|p| p.starts_with("/api/extensions/"))
                                .map(|p| self.server.image_url(&format!("{p}?size=300")));
                            let in_library = it
                                .get("track")
                                .as_str()
                                .is_some_and(|t| self.library.track(t).is_some());
                            self.ext.results.push(
                                it.clone()
                                    .with("ext", ext.as_str())
                                    .with("image", image.unwrap_or_default())
                                    .with("inLibrary", in_library),
                            );
                        }
                    }
                    None => self.ext.error = Some(error()),
                }
                self.ext.rev += 1;
            }
            ExtReq::Import { keep, item } => {
                let Some(v) = json else {
                    let msg = format!("{}: {}", self.tr("Could not load the song"), error());
                    self.toast(msg, true);
                    return;
                };
                if let Some(track) = v.get("track").as_str().filter(|t| !t.is_empty()) {
                    // Already in the library.
                    let track = track.to_string();
                    self.add_result_track(&track, &item);
                    self.play_tracks(host, vec![track], 0);
                    return;
                }
                let job = v.str_or("job", "").to_string();
                let audio = v.str_or("audio", "").to_string();
                if !safe_id(&job) || !audio.starts_with("/api/extensions/jobs/") {
                    return;
                }
                let id = format!("{ID_PREFIX}{job}");
                let mut t = Track {
                    id: id.clone(),
                    title: v.str_or("title", item.str_or("title", "")).to_string(),
                    duration_ms: (v.f64_or("duration", item.f64_or("duration", 0.0)).max(0.0)
                        * 1000.0) as u64,
                    codec: "opus".into(),
                    has_cover: !item.str_or("image", "").is_empty(),
                    ..Default::default()
                };
                let artist = v.str_or("artist", item.str_or("artist", "")).to_string();
                if !artist.is_empty() {
                    t.artists = vec![artist.clone()];
                }
                t.album = v.str_or("album", "").to_string();
                self.ext.jobs.push(Job {
                    job,
                    keep,
                    title: t.title.clone(),
                    artist,
                    audio,
                    cover: item.str_or("image", "").to_string(),
                    polled_at: self.time,
                    asked: false,
                    done: false,
                    started: self.time,
                    play_when_done: !host.has_threads() || host.audio_is_push(),
                });
                let later = self.ext.jobs.last().is_some_and(|j| j.play_when_done);
                self.extra_tracks.insert(id.clone(), t);
                if !later {
                    self.play_tracks(host, vec![id], 0);
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
        if self.current.as_ref().is_some_and(|t| t.id == id) {
            self.player.stop(host);
        }
        self.ext.jobs.retain(|j| j.job != job);
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
            self.play_tracks(host, vec![track], 0);
        }
        self.refresh_server_data(host);
    }

    /// Polls running imports (every tick).
    pub(crate) fn update_extensions(&mut self, host: &mut dyn Host) {
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
