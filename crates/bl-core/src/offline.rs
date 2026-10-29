//! Offline copies of server songs (`bl_offline` containers in the app's
//! storage, `offline/<server>-<track>.blo`): audio, song text, cover and
//! analysis, so the songs play with their visuals without a connection.

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::library;
use bl_json::Value;
use bl_net::api::{Album, Library, Track};
use bl_offline::Header;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub(crate) struct OfflineEntry {
    pub path: PathBuf,
    pub track: Track,
    pub header: Header,
    /// The cover extracted for the UI (file path).
    pub cover_file: Option<String>,
}

struct Saving {
    id: String,
    title: String,
    size: u64,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

/// A finished download: the track id and the container (or why not).
type Saved = (String, Result<(PathBuf, Header), String>);

#[derive(Default)]
pub(crate) struct OfflineState {
    pub entries: HashMap<String, OfflineEntry>,
    saving: Vec<Saving>,
    results: Mailbox<Saved>,
    /// Server the entries were loaded for.
    loaded_for: Option<String>,
    /// The offline songs stand in for the server's library.
    pub shown: bool,
}

fn valid_id(id: &str) -> bool {
    library::is_server_id(id)
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Core {
    fn offline_prefix(&self) -> String {
        format!("{:016x}-", library::fnv(&self.settings.server_url))
    }

    fn offline_dir(&self, host: &dyn Host) -> Option<PathBuf> {
        if !host.has_threads() {
            return None;
        }
        host.storage_path("offline").map(PathBuf::from)
    }

    /// Loads the offline copies of the configured server (once per server).
    pub(crate) fn offline_load(&mut self, host: &mut dyn Host) {
        if self.offline.loaded_for.as_deref() == Some(self.settings.server_url.as_str()) {
            return;
        }
        self.offline.loaded_for = Some(self.settings.server_url.clone());
        self.offline.entries.clear();
        self.offline.shown = false;
        let Some(dir) = self.offline_dir(host) else {
            return;
        };
        let prefix = self.offline_prefix();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || !name.ends_with(".blo") {
                continue;
            }
            match bl_offline::read_header(&e.path()) {
                Ok(h) if valid_id(h.id()) => self.offline_add(host, e.path(), h),
                // Damaged or foreign: removed.
                _ => {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    fn offline_add(&mut self, host: &dyn Host, path: PathBuf, header: Header) {
        let track = Track::from_json(&header.track);
        let id = track.id.clone();
        // The cover is extracted next to the container for the image views.
        let cover_file = header.cover.and_then(|s| {
            let key = format!("offline/covers/{}.img", path.file_stem()?.to_string_lossy());
            let file = host.storage_path(&key)?;
            if !std::path::Path::new(&file).exists() {
                let data = bl_offline::read_section(&path, s).ok()?;
                host.storage_write(&key, &data).then_some(())?;
            }
            Some(file)
        });
        self.offline.entries.insert(
            id,
            OfflineEntry {
                path,
                track,
                header,
                cover_file,
            },
        );
    }

    /// `offline.save`: keeps the given server songs on this device.
    pub(crate) fn offline_save(&mut self, host: &mut dyn Host, ids: Vec<String>) -> Value {
        let fail = |core: &mut Core, msg: &str| {
            let msg = core.tr(msg).to_string();
            core.toast(msg.clone(), true);
            Value::object().with("error", msg)
        };
        let Some(dir) = self.offline_dir(host) else {
            return fail(self, "Offline copies are not available here");
        };
        if !self.server.is_connected() {
            return fail(self, "Not connected to the server");
        }
        if std::fs::create_dir_all(&dir).is_err() {
            return fail(self, "Offline copies are not available here");
        }
        let cfg = self.preset.analysis_config();
        let cfg_json = Value::object()
            .with("analysisConfig", self.preset.analysis.clone())
            .with("noveltyConfigs", self.preset.novelty.clone())
            .to_json();
        let mut started = 0;
        for id in ids {
            if !valid_id(&id)
                || self.offline.entries.contains_key(&id)
                || self.offline.saving.iter().any(|s| s.id == id)
            {
                continue;
            }
            let Some(track) = self.library.track(&id).cloned() else {
                continue;
            };
            let analysis_key = format!(
                "{}|{}|{}|{:016x}",
                track.id,
                track.size,
                track.modified,
                cfg.cache_key()
            );
            let urls = (
                self.server.track_stream_url(&id),
                track
                    .has_cover
                    .then(|| self.server.url(&bl_net::api::paths::track_cover(&id, 512))),
                self.server.url(&bl_net::api::paths::track_lyrics(&id)),
                self.server.url(&self.server.analysis_path(&id, &cfg_json)),
            );
            let headers = self.server.headers();
            let path = dir.join(format!("{}{id}.blo", self.offline_prefix()));
            let done = Arc::new(AtomicU64::new(0));
            let cancel = Arc::new(AtomicBool::new(false));
            self.offline.saving.push(Saving {
                id: id.clone(),
                title: track.title.clone(),
                size: track.size,
                done: done.clone(),
                cancel: cancel.clone(),
            });
            let mailbox = self.offline.results.clone();
            host.spawn(Box::new(move || {
                let res = save_job(&track, &urls, &headers, &path, analysis_key, &done, &cancel);
                mailbox.post((track.id.clone(), res));
            }));
            started += 1;
        }
        Value::object().with("started", started as u64)
    }

    /// `offline.remove`: drops offline copies.
    pub(crate) fn offline_remove(&mut self, ids: Vec<String>) {
        for id in ids {
            if let Some(s) = self.offline.saving.iter().find(|s| s.id == id) {
                s.cancel.store(true, Ordering::Relaxed);
            }
            if let Some(e) = self.offline.entries.remove(&id) {
                let _ = std::fs::remove_file(&e.path);
                if let Some(c) = e.cover_file {
                    let _ = std::fs::remove_file(c);
                }
            }
        }
        self.library.revision += 1;
    }

    /// Takes finished downloads (from `tick`) and shows the offline songs
    /// in place of the server's library while it cannot be reached.
    pub(crate) fn poll_offline(&mut self, host: &mut dyn Host) {
        self.offline_load(host);
        for (id, res) in self.offline.results.take() {
            self.offline.saving.retain(|s| s.id != id);
            match res {
                Ok((path, header)) => {
                    self.offline_add(host, path, header);
                    self.library.revision += 1;
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => {
                    let title = self.track_for(&id).title;
                    self.toast(
                        format!(
                            "{} \"{title}\": {e}",
                            self.tr("Could not save for offline use")
                        ),
                        true,
                    );
                }
            }
        }
        if !self.server.is_connected()
            && self.library.remote.tracks.is_empty()
            && !self.offline.entries.is_empty()
            && !self.offline.shown
        {
            self.offline.shown = true;
            let lib = self.offline_library();
            self.library.set_remote(lib);
        }
    }

    /// The offline songs as a library (albums from their tags).
    fn offline_library(&self) -> Library {
        let mut tracks: Vec<Track> = self
            .offline
            .entries
            .values()
            .map(|e| e.track.clone())
            .collect();
        tracks.sort_by(|a, b| a.id.cmp(&b.id));
        let mut albums: HashMap<String, Album> = HashMap::new();
        for t in &tracks {
            let a = albums.entry(t.album_id.clone()).or_insert_with(|| Album {
                id: t.album_id.clone(),
                name: t.album.clone(),
                artist: if t.album_artist.is_empty() {
                    t.artists.first().cloned().unwrap_or_default()
                } else {
                    t.album_artist.clone()
                },
                year: t.year,
                track_count: 0,
                duration_ms: 0,
                cover_track: None,
            });
            a.track_count += 1;
            a.duration_ms += t.duration_ms;
            if a.cover_track.is_none() && t.has_cover {
                a.cover_track = Some(t.id.clone());
            }
        }
        Library {
            revision: 0,
            tracks,
            albums: albums.into_values().filter(|a| !a.id.is_empty()).collect(),
        }
    }

    pub(crate) fn offline_track(&self, id: &str) -> Option<Track> {
        self.offline.entries.get(id).map(|e| e.track.clone())
    }

    /// Opens the audio of an offline copy.
    pub(crate) fn offline_opener(&self, id: &str) -> Option<crate::player::SourceOpener> {
        let e = self.offline.entries.get(id)?;
        let (path, header) = (e.path.clone(), e.header.clone());
        let ext = (!header.ext.is_empty()).then(|| header.ext.clone());
        Some(Box::new(move || {
            let w = bl_offline::audio(&path, &header).map_err(|e| e.to_string())?;
            Ok((Box::new(w) as Box<dyn bl_media::Source>, ext.clone()))
        }))
    }

    pub(crate) fn offline_cover(&self, id: &str) -> Option<&str> {
        self.offline.entries.get(id)?.cover_file.as_deref()
    }

    pub(crate) fn offline_lyrics(&self, id: &str) -> Option<(String, String)> {
        self.offline.entries.get(id)?.header.lyrics.clone()
    }

    /// The stored analysis when it was made with the same settings.
    pub(crate) fn offline_analysis(&self, id: &str, key: &str) -> Option<Vec<u8>> {
        let e = self.offline.entries.get(id)?;
        if e.header.analysis_key != key {
            return None;
        }
        bl_offline::read_section(&e.path, e.header.analysis?).ok()
    }

    /// Keeps the song text of an offline copy current (`None`: removed).
    pub(crate) fn offline_set_lyrics(
        &mut self,
        host: &mut dyn Host,
        id: &str,
        lyrics: Option<(String, String)>,
    ) {
        let Some(e) = self.offline.entries.get_mut(id) else {
            return;
        };
        if e.header.lyrics == lyrics {
            return;
        }
        e.header.lyrics = lyrics.clone();
        let path = e.path.clone();
        host.spawn(Box::new(move || {
            let _ = bl_offline::set_lyrics(&path, lyrics);
        }));
    }

    pub(crate) fn offline_json(&self, host: &dyn Host) -> Value {
        let bytes: u64 = self
            .offline
            .entries
            .values()
            .map(|e| e.header.audio.len)
            .sum();
        Value::object()
            .with("available", self.offline_dir(host).is_some())
            .with("count", self.offline.entries.len() as u64)
            .with("bytes", bytes)
            .with("shown", self.offline.shown)
            .with(
                "saving",
                Value::Array(
                    self.offline
                        .saving
                        .iter()
                        .map(|s| {
                            Value::object()
                                .with("id", s.id.as_str())
                                .with("title", s.title.as_str())
                                .with(
                                    "progress",
                                    s.done.load(Ordering::Relaxed) as f64 / s.size.max(1) as f64,
                                )
                        })
                        .collect(),
                ),
            )
    }

    pub(crate) fn is_offline(&self, id: &str) -> bool {
        self.offline.entries.contains_key(id)
    }
}

/// Downloads a song with its cover, song text and analysis into a container
/// (worker thread).
fn save_job(
    track: &Track,
    urls: &(String, Option<String>, String, String),
    headers: &[(String, String)],
    path: &std::path::Path,
    analysis_key: String,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<(PathBuf, Header), String> {
    let get = |url: &str| -> Option<Vec<u8>> {
        let r = bl_net::client::request(
            "GET",
            url,
            headers,
            &[],
            &bl_net::client::Options::default(),
        )
        .ok()?;
        (r.status == 200).then_some(r.body)
    };
    let cover = urls.1.as_deref().and_then(get);
    let analysis = get(&urls.3).filter(|b| bl_analysis::decode(b).is_some());
    let lyrics = get(&urls.2)
        .and_then(|b| bl_json::parse_bytes(&b).ok())
        .and_then(|v| {
            Some((
                v.get("text").as_str()?.to_string(),
                v.str_or("source", "server").to_string(),
            ))
        });
    let mut audio = bl_net::client::HttpStream::open(&urls.0, headers)?;
    let header = Header {
        track: track.to_json(),
        ext: if track.codec.is_empty() {
            crate::jobs::extension(&track.path).unwrap_or_default()
        } else {
            crate::jobs::extension(&track.path).unwrap_or_else(|| track.codec.clone())
        },
        saved: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        lyrics,
        analysis_key: if analysis.is_some() {
            analysis_key
        } else {
            String::new()
        },
        ..Default::default()
    };
    let header = bl_offline::write(
        path,
        header,
        cover.as_deref(),
        analysis.as_deref(),
        &mut audio,
        &mut |n| {
            done.store(n, Ordering::Relaxed);
            !cancel.load(Ordering::Relaxed)
        },
    )
    .map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            "cancelled".to_string()
        } else {
            e.to_string()
        }
    })?;
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(path);
        return Err("cancelled".into());
    }
    Ok((path.to_path_buf(), header))
}
