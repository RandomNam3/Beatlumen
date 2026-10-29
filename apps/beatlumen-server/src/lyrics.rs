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

    /// `{"text", "source"}` of a track, `None` without a song text.
    pub fn get(&self, e: &Entry) -> Option<Value> {
        let (text, source) = match bl_media::lyrics::read_text_file(&self.imported(&e.track.id)) {
            Some(t) => (t, "imported"),
            None => {
                let (t, s) = bl_media::lyrics::find_lyrics(&e.abs)?;
                (t, s.name())
            }
        };
        Some(Value::object().with("text", text).with("source", source))
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
            .and_then(|_| util::write_atomic(&self.imported(&e.track.id), text.replace("\r\n", "\n").as_bytes()))
            .map_err(|err| (500, format!("cannot store the song text: {err}")))
    }

    /// Removes an imported song text; false when there was none.
    pub fn delete(&self, e: &Entry) -> bool {
        std::fs::remove_file(self.imported(&e.track.id)).is_ok()
    }
}
