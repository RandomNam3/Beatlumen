//! Data types of the OpenChroma server HTTP API (shared by server and
//! clients) with JSON conversion.
//!
//! See `docs/server-api.md` for the endpoint list.

use oc_json::{Map, Value};

pub const API_VERSION: u32 = 1;

fn s(v: &Value, k: &str) -> String {
    v.get(k).as_str().unwrap_or("").to_string()
}
fn os(v: &Value, k: &str) -> Option<String> {
    v.get(k).as_str().map(str::to_string)
}
fn u(v: &Value, k: &str) -> u64 {
    v.get(k).as_u64().unwrap_or(0)
}
fn ou32(v: &Value, k: &str) -> Option<u32> {
    v.get(k).as_u64().map(|x| x as u32)
}
fn strs(v: &Value, k: &str) -> Vec<String> {
    v.get(k).as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub auth_required: bool,
    pub track_count: u64,
    pub scanning: bool,
    /// Library revision; changes whenever the library changes.
    pub revision: u64,
}

impl ServerInfo {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("name", self.name.as_str())
            .with("version", self.version.as_str())
            .with("apiVersion", self.api_version)
            .with("authRequired", self.auth_required)
            .with("trackCount", self.track_count)
            .with("scanning", self.scanning)
            .with("revision", self.revision)
    }
    pub fn from_json(v: &Value) -> ServerInfo {
        ServerInfo {
            name: s(v, "name"),
            version: s(v, "version"),
            api_version: u(v, "apiVersion") as u32,
            auth_required: v.bool_or("authRequired", false),
            track_count: u(v, "trackCount"),
            scanning: v.bool_or("scanning", false),
            revision: u(v, "revision"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    /// Stable id (hex string).
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_id: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<i32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub duration_ms: u64,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate_kbps: Option<u32>,
    pub size: u64,
    /// Path relative to its library folder (for folder browsing/display).
    pub path: String,
    pub has_cover: bool,
    /// Modification time (seconds since the Unix epoch).
    pub modified: u64,
}

impl Track {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id", self.id.as_str());
        m.insert("title", self.title.as_str());
        m.insert("artists", self.artists.clone());
        m.insert("album", self.album.as_str());
        m.insert("albumId", self.album_id.as_str());
        m.insert("albumArtist", self.album_artist.as_str());
        m.insert("genre", self.genre.as_str());
        m.insert("year", self.year);
        m.insert("track", self.track);
        m.insert("disc", self.disc);
        m.insert("durationMs", self.duration_ms);
        m.insert("codec", self.codec.as_str());
        m.insert("sampleRate", self.sample_rate);
        m.insert("channels", self.channels);
        m.insert("bitrate", self.bitrate_kbps);
        m.insert("size", self.size);
        m.insert("path", self.path.as_str());
        m.insert("hasCover", self.has_cover);
        m.insert("modified", self.modified);
        Value::Object(m)
    }
    pub fn from_json(v: &Value) -> Track {
        Track {
            id: s(v, "id"),
            title: s(v, "title"),
            artists: strs(v, "artists"),
            album: s(v, "album"),
            album_id: s(v, "albumId"),
            album_artist: s(v, "albumArtist"),
            genre: s(v, "genre"),
            year: v.get("year").as_i64().map(|y| y as i32),
            track: ou32(v, "track"),
            disc: ou32(v, "disc"),
            duration_ms: u(v, "durationMs"),
            codec: s(v, "codec"),
            sample_rate: u(v, "sampleRate") as u32,
            channels: u(v, "channels") as u16,
            bitrate_kbps: ou32(v, "bitrate"),
            size: u(v, "size"),
            path: s(v, "path"),
            has_cover: v.bool_or("hasCover", false),
            modified: u(v, "modified"),
        }
    }
    /// "Artist 1, Artist 2" (or "Unknown Artist").
    pub fn artist_line(&self) -> String {
        if self.artists.is_empty() {
            "Unknown Artist".into()
        } else {
            self.artists.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub year: Option<i32>,
    pub track_count: u32,
    pub duration_ms: u64,
    /// Track whose cover represents the album (if any has one).
    pub cover_track: Option<String>,
}

impl Album {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with("artist", self.artist.as_str())
            .with("year", self.year)
            .with("trackCount", self.track_count)
            .with("durationMs", self.duration_ms)
            .with("coverTrack", self.cover_track.clone())
    }
    pub fn from_json(v: &Value) -> Album {
        Album {
            id: s(v, "id"),
            name: s(v, "name"),
            artist: s(v, "artist"),
            year: v.get("year").as_i64().map(|y| y as i32),
            track_count: u(v, "trackCount") as u32,
            duration_ms: u(v, "durationMs"),
            cover_track: os(v, "coverTrack"),
        }
    }
}

/// The complete library index as served by `GET /api/library`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Library {
    pub revision: u64,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
}

impl Library {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("revision", self.revision)
            .with("tracks", Value::Array(self.tracks.iter().map(Track::to_json).collect()))
            .with("albums", Value::Array(self.albums.iter().map(Album::to_json).collect()))
    }
    pub fn from_json(v: &Value) -> Library {
        Library {
            revision: u(v, "revision"),
            tracks: v.get("tracks").as_array().map(|a| a.iter().map(Track::from_json).collect()).unwrap_or_default(),
            albums: v.get("albums").as_array().map(|a| a.iter().map(Album::from_json).collect()).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaylistKind {
    #[default]
    Track,
    Background,
}

impl PlaylistKind {
    pub fn name(self) -> &'static str {
        match self {
            PlaylistKind::Track => "track",
            PlaylistKind::Background => "background",
        }
    }
    pub fn parse(s: &str) -> PlaylistKind {
        if s == "background" {
            PlaylistKind::Background
        } else {
            PlaylistKind::Track
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub kind: PlaylistKind,
    /// Track ids or image ids (in order).
    pub items: Vec<String>,
    pub modified: u64,
}

impl Playlist {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with("kind", self.kind.name())
            .with("items", self.items.clone())
            .with("modified", self.modified)
    }
    pub fn from_json(v: &Value) -> Playlist {
        Playlist { id: s(v, "id"), name: s(v, "name"), kind: PlaylistKind::parse(v.str_or("kind", "track")), items: strs(v, "items"), modified: u(v, "modified") }
    }
}

/// A background image available on the server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImageEntry {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub modified: u64,
}

impl ImageEntry {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with("folder", self.folder.as_str())
            .with("size", self.size)
            .with("modified", self.modified)
    }
    pub fn from_json(v: &Value) -> ImageEntry {
        ImageEntry { id: s(v, "id"), name: s(v, "name"), folder: s(v, "folder"), size: u(v, "size"), modified: u(v, "modified") }
    }
}

/// Endpoint paths (relative to the server base URL).
pub mod paths {
    pub const INFO: &str = "/api/info";
    pub const LOGIN: &str = "/api/login";
    pub const LIBRARY: &str = "/api/library";
    pub const RESCAN: &str = "/api/rescan";
    pub const PLAYLISTS: &str = "/api/playlists";
    pub const IMAGES: &str = "/api/images";
    pub const PRESETS: &str = "/api/presets";

    pub fn track_stream(id: &str) -> String {
        format!("/api/tracks/{id}/stream")
    }
    pub fn track_cover(id: &str, size: u32) -> String {
        format!("/api/tracks/{id}/cover?size={size}")
    }
    /// `cfg` is the compact JSON of the preset's analysis settings
    /// (`{"analysisConfig":..,"noveltyConfigs":..}`), percent-encoded.
    pub fn track_analysis(id: &str, cfg_encoded: &str) -> String {
        format!("/api/tracks/{id}/analysis?cfg={cfg_encoded}")
    }
    pub fn playlist(id: &str) -> String {
        format!("/api/playlists/{id}")
    }
    pub fn image(id: &str, max: u32) -> String {
        if max == 0 {
            format!("/api/images/{id}")
        } else {
            format!("/api/images/{id}?size={max}")
        }
    }
    pub fn preset(name: &str) -> String {
        format!("/api/presets/{}", crate::http::percent_encode(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips() {
        let t = Track { id: "ab".into(), title: "T".into(), artists: vec!["A".into(), "B".into()], year: Some(1999), track: Some(3), ..Default::default() };
        assert_eq!(Track::from_json(&oc_json::parse(&t.to_json().to_json()).unwrap()), t);
        let lib = Library { revision: 5, tracks: vec![t], albums: vec![Album { id: "x".into(), cover_track: Some("ab".into()), ..Default::default() }] };
        assert_eq!(Library::from_json(&lib.to_json()), lib);
        let p = Playlist { id: "p".into(), name: "N".into(), kind: PlaylistKind::Background, items: vec!["1".into()], modified: 2 };
        assert_eq!(Playlist::from_json(&p.to_json()), p);
    }
}
