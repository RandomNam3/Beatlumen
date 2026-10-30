//! Remote music sources: the songs of a Subsonic/OpenSubsonic or Jellyfin
//! server, of a WebDAV (Nextcloud) or SFTP folder or of a DLNA media
//! server, with metadata, covers and seekable streams of the original
//! files (so playback gets the full analysis like local files); internet
//! radio and playlist files.
//!
//! The server indexes sources the admin sets up and serves their songs
//! like its own; the apps can also use sources directly (app-only use).
//! Only the addresses configured for a source are contacted.

pub mod dlna;
pub mod jellyfin;
pub mod md5;
pub mod playlist;
pub mod radio;
pub mod sftp;
pub mod subsonic;
pub mod tags;
pub mod web;
pub mod webdav;

use bl_json::Value;

/// A song of a source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteTrack {
    /// Identifies the song within its source (a path or the source's id).
    pub key: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub album_artist: String,
    pub track: u32,
    pub disc: u32,
    pub year: u32,
    pub genre: String,
    pub duration_ms: u64,
    /// Bytes (0 = unknown).
    pub size: u64,
    /// Seconds since the Unix epoch (0 = unknown).
    pub modified: u64,
    /// File extension or container hint ("mp3", "flac", …).
    pub ext: String,
    pub has_cover: bool,
}

/// A playlist of a source (keys of its songs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemotePlaylist {
    pub name: String,
    pub keys: Vec<String>,
}

/// What a source needs to connect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceConfig {
    /// "subsonic", "jellyfin", "webdav", "sftp", "dlna".
    pub kind: String,
    /// Address: http(s) for servers and WebDAV, `sftp://host[:port]`,
    /// the device description URL of a DLNA server.
    pub url: String,
    pub user: String,
    /// Password, app password, access token or private key (OpenSSH).
    pub secret: String,
    /// Folder within the source (WebDAV, SFTP).
    pub path: String,
    /// Pinned server key (SFTP host key, SHA-256 fingerprint) or TLS
    /// certificate fingerprint; empty: the first one seen is reported.
    pub pin: String,
}

impl SourceConfig {
    pub fn from_json(v: &Value) -> SourceConfig {
        SourceConfig {
            kind: v.str_or("kind", "").to_string(),
            url: v.str_or("url", "").trim().trim_end_matches('/').to_string(),
            user: v.str_or("user", "").to_string(),
            secret: v.str_or("secret", "").to_string(),
            path: v.str_or("path", "").to_string(),
            pin: v.str_or("pin", "").to_string(),
        }
    }

    pub fn to_json(&self) -> Value {
        Value::object()
            .with("kind", self.kind.as_str())
            .with("url", self.url.as_str())
            .with("user", self.user.as_str())
            .with("secret", self.secret.as_str())
            .with("path", self.path.as_str())
            .with("pin", self.pin.as_str())
    }
}

/// Songs already known (from an earlier listing): returns the song when
/// the file with this key, size and modification time was read before, so
/// file sources do not read its tags again.
pub type Known<'a> = &'a dyn Fn(&str, u64, u64) -> Option<RemoteTrack>;

/// A connected source.
pub trait MusicSource: Send + Sync {
    /// All songs. `progress` gets the number found so far and returns false
    /// to stop.
    fn list(
        &self,
        known: Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String>;
    /// The song's file as a seekable stream.
    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String>;
    /// The song's cover (image bytes) at about `size` pixels.
    fn cover(&self, key: &str, size: u32) -> Option<Vec<u8>>;
    /// The song was played (scrobbling, play counts).
    fn played(&self, _key: &str) {}
    /// The source's playlists.
    fn playlists(&self) -> Result<Vec<RemotePlaylist>, String> {
        Ok(Vec::new())
    }
}

/// Connects to a source (checks the login).
pub fn connect(cfg: &SourceConfig) -> Result<Box<dyn MusicSource>, String> {
    match cfg.kind.as_str() {
        "subsonic" => Ok(Box::new(subsonic::Subsonic::connect(cfg)?)),
        "jellyfin" => Ok(Box::new(jellyfin::Jellyfin::connect(cfg)?)),
        "webdav" => Ok(Box::new(webdav::WebDav::connect(cfg)?)),
        "dlna" => Ok(Box::new(dlna::Dlna::connect(cfg)?)),
        "sftp" => Ok(Box::new(sftp::SftpSource::connect(cfg)?)),
        "web" => Ok(Box::new(web::Web::connect(cfg)?)),
        other => Err(format!("unknown source kind \"{other}\"")),
    }
}

/// Largest cover image read.
pub(crate) const MAX_COVER: usize = 16 << 20;
/// Largest small answer read (logins, device descriptions, short JSON).
pub(crate) const MAX_SMALL: usize = 1 << 20;

/// Request options that read at most `max_body` bytes.
pub(crate) fn opts(max_body: usize) -> bl_net::client::Options {
    bl_net::client::Options {
        max_body,
        ..Default::default()
    }
}

/// Lowercase extension of a path or URL (without query), "" if none.
pub fn ext_of(path: &str) -> String {
    let name = path.split(['?', '#']).next().unwrap_or(path);
    let name = name.rsplit('/').next().unwrap_or(name);
    match name.rsplit_once('.') {
        Some((_, e)) if !e.is_empty() && e.len() <= 5 => e.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Percent-encodes a query or path component.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encodes a path, keeping `/`.
pub fn encode_path(p: &str) -> String {
    p.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// Decodes `%xx` escapes.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            // Bytes, not `&s[..]`: a multi-byte character may follow '%'.
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(ext_of("/a/b/Song.Name.FLAC?x=1"), "flac");
        assert_eq!(ext_of("/a/b/noext"), "");
        assert_eq!(encode("a b/ä"), "a%20b%2F%C3%A4");
        assert_eq!(encode_path("/Musik/a b"), "/Musik/a%20b");
        assert_eq!(decode("a%20b%2F%C3%A4"), "a b/ä");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%aä%ä"), "%aä%ä");
    }
}
