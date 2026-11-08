//! Karaoke tracks as the server and the apps keep them: the track's JSON
//! plus where it came from and, for a computed one, which song text it was
//! made from, so that a changed text makes it stale.

use crate::track::KaraokeTrack;
use bl_json::Value;

/// Version of the computation (`bl-karaoke`); stored with its results.
pub const VERSION: u32 = 1;

/// Where a stored karaoke track came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Computed from the song and its timed text.
    Computed,
    /// Imported (UltraStar file or JSON).
    Imported,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Computed => "computed",
            Source::Imported => "imported",
        }
    }
}

/// FNV-1a 64 of a song text, as stored with a computed track.
pub fn text_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.trim().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The track as stored: its JSON plus `source`, `made` (the computation's
/// version) and, for a computed one, the hash of the song text it was
/// made from.
pub fn to_stored(t: &KaraokeTrack, source: Source, lyrics: Option<&str>) -> Value {
    let mut v = t.to_json().with("source", source.name());
    if source == Source::Computed {
        v.set("made", VERSION);
    }
    if let Some(text) = lyrics {
        v.set("lyricsHash", text_hash(text));
    }
    v
}

/// Whether a stored track still fits the song text: imported ones always
/// do, computed ones while the text is the one they were made from (and
/// by this version or a later one).
pub fn is_current(stored: &Value, lyrics: Option<&str>) -> bool {
    if stored.str_or("source", "imported") != Source::Computed.name() {
        return true;
    }
    let made = stored.get("made").as_u64().unwrap_or(0);
    made >= VERSION as u64
        && match (stored.get("lyricsHash").as_str(), lyrics) {
            (Some(h), Some(text)) => h == text_hash(text),
            _ => false,
        }
}

/// Reads an import: UltraStar TXT or the JSON of a karaoke track (with at
/// least one note).
pub fn import(body: &[u8]) -> Result<KaraokeTrack, String> {
    let first = body
        .iter()
        .find(|b| !b.is_ascii_whitespace() && ![0xef, 0xbb, 0xbf].contains(*b));
    let track = if first == Some(&b'{') {
        let v = bl_json::parse_bytes(body).map_err(|_| "not valid JSON".to_string())?;
        KaraokeTrack::from_json(&v)?
    } else {
        crate::ultrastar::from_ultrastar_bytes(body)?
    };
    if track.notes().next().is_none() {
        return Err("no notes".into());
    }
    Ok(track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_tracks_go_stale_with_their_text() {
        let t = KaraokeTrack::default();
        let v = to_stored(&t, Source::Computed, Some("[00:01.00]la"));
        assert!(is_current(&v, Some("[00:01.00]la\n")));
        assert!(!is_current(&v, Some("[00:01.00]lo")));
        assert!(!is_current(&v, None));
        let v = to_stored(&t, Source::Imported, None);
        assert!(is_current(&v, Some("anything")));
        // The stored form reads back as a track.
        assert!(KaraokeTrack::from_json(&v).is_ok());
    }

    #[test]
    fn imports() {
        let txt = b"#TITLE:Song\n#ARTIST:Someone\n#BPM:300\n#GAP:1000\n: 0 4 60 La\n: 4 4 62 la\n- 10\n: 12 4 64 lu\nE\n";
        let t = import(txt).expect("UltraStar");
        assert_eq!(t.title, "Song");
        assert_eq!(t.notes().count(), 3);
        let json = t.to_json().to_json();
        assert_eq!(import(json.as_bytes()).expect("JSON"), t);
        assert!(import(b"#TITLE:x\nE\n").is_err());
        assert!(import(b"{\"format\":\"other\"}").is_err());
    }
}
