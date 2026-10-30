//! Song texts: imported ones (stored in the data folder) override what the
//! library has (an `.lrc` file next to the song, the embedded tag, a `.txt`
//! file next to it).

use crate::library::Entry;
use crate::util;
use bl_json::Value;
use std::path::PathBuf;

/// Longest song text accepted for import (bytes).
pub const MAX_IMPORT: usize = 256 * 1024;

pub struct Lyrics {
    dir: PathBuf,
}

impl Lyrics {
    pub fn new(dir: PathBuf) -> Lyrics {
        Lyrics { dir }
    }

    fn imported(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.lrc"))
    }

    /// The song text from the import or the library (not the computed
    /// timings) and its source.
    pub fn source_text(&self, e: &Entry) -> Option<(String, &'static str)> {
        if let Some(t) = bl_media::lyrics::read_text_file(&self.imported(&e.track.id)) {
            return Some((t, "imported"));
        }
        if e.remote.is_some() {
            // Remote songs: imported texts and computed timings only.
            return None;
        }
        bl_media::lyrics::find_lyrics(&e.abs).map(|(t, s)| (t, s.name()))
    }

    /// `{"text", "source"}` of a track, `None` without a song text. Timings
    /// computed by speech recognition stand in for a plain (or missing) text.
    pub fn get(&self, e: &Entry) -> Option<Value> {
        let found = self.source_text(e);
        let (text, source) = match found {
            Some((t, s)) if crate::lyrics_sync::is_synced(&t) => (t, s),
            other => {
                let id = &e.track.id;
                let computed = [
                    (self.dir.join(format!("{id}.synced.lrc")), "synced"),
                    (self.dir.join(format!("{id}.recognized.lrc")), "recognized"),
                ]
                .into_iter()
                .find_map(|(p, s)| bl_media::lyrics::read_text_file(&p).map(|t| (t, s)));
                computed.or(other)?
            }
        };
        Some(Value::object().with("text", text).with("source", source))
    }

    /// Removes computed timings (the text they were made from changed).
    fn forget_computed(&self, id: &str) {
        for suffix in ["synced.lrc", "recognized.lrc", "failed"] {
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.{suffix}")));
        }
    }

    /// Stores an imported song text for the track.
    pub fn put(&self, e: &Entry, body: &[u8]) -> Result<(), (u16, String)> {
        if body.len() > MAX_IMPORT {
            return Err((413, "song text too large".into()));
        }
        let text = bl_media::lyrics::decode_text(body);
        let text = text.trim();
        if text.is_empty() {
            return Err((400, "empty song text".into()));
        }
        std::fs::create_dir_all(&self.dir)
            .and_then(|_| {
                util::write_atomic(
                    &self.imported(&e.track.id),
                    text.replace("\r\n", "\n").as_bytes(),
                )
            })
            .map_err(|err| (500, format!("cannot store the song text: {err}")))?;
        self.forget_computed(&e.track.id);
        Ok(())
    }

    /// Removes an imported song text; false when there was none.
    pub fn delete(&self, e: &Entry) -> bool {
        self.forget_computed(&e.track.id);
        std::fs::remove_file(self.imported(&e.track.id)).is_ok()
    }
}
