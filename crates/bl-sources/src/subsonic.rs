//! Subsonic / OpenSubsonic servers (Navidrome, Gonic, Airsonic-Advanced,
//! Ampache, …): songs with their tags, the original files (no
//! transcoding, `format=raw`), covers, playlists and scrobbling.
//!
//! Login with a token: `md5(password + salt)` with a new random salt per
//! request, as the protocol defines; the password itself never goes over
//! the wire. An OpenSubsonic API key (`secret` = `apikey:<key>`) works too.

use crate::{md5, MusicSource, RemotePlaylist, RemoteTrack, SourceConfig};
use bl_json::Value;
use bl_net::client::{self, HttpStream, Options};
use std::collections::HashMap;
use std::sync::Mutex;

const API_VERSION: &str = "1.16.1";
const CLIENT: &str = "Beatlumen";
const PAGE: usize = 500;
/// Upper bound of songs taken from one server.
const MAX_SONGS: usize = 500_000;
/// Upper bound of pages asked for in one listing (also when the pages
/// hold nothing usable).
const MAX_PAGES: usize = MAX_SONGS / PAGE + 1;
/// Upper bound of albums looked at in a listing album by album.
const MAX_ALBUMS: usize = 100_000;

pub struct Subsonic {
    base: String,
    user: String,
    secret: String,
    /// Song id -> cover art id (from the listing).
    covers: Mutex<HashMap<String, String>>,
}

/// A salt for one request: it only has to differ between requests (the
/// protocol uses it against replaying the token), so time and a counter do.
fn salt() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    md5::md5_hex(format!("{t}:{n}:{}", std::process::id()).as_bytes())[..12].to_string()
}

impl Subsonic {
    pub fn connect(cfg: &SourceConfig) -> Result<Subsonic, String> {
        if !(cfg.url.starts_with("http://") || cfg.url.starts_with("https://")) {
            return Err("the address must start with http:// or https://".into());
        }
        let s = Subsonic {
            base: cfg.url.trim_end_matches('/').to_string(),
            user: cfg.user.clone(),
            secret: cfg.secret.clone(),
            covers: Mutex::new(HashMap::new()),
        };
        s.call("ping", &[])?;
        Ok(s)
    }

    /// The URL of an API method with the login parameters.
    fn url(&self, method: &str, params: &[(&str, String)]) -> String {
        let mut q = format!("v={API_VERSION}&c={CLIENT}&f=json");
        match self.secret.strip_prefix("apikey:") {
            Some(key) => q.push_str(&format!("&apiKey={}", crate::encode(key))),
            None => {
                let s = salt();
                let t = md5::md5_hex(format!("{}{s}", self.secret).as_bytes());
                q.push_str(&format!("&u={}&t={t}&s={s}", crate::encode(&self.user)));
            }
        }
        for (k, v) in params {
            q.push_str(&format!("&{k}={}", crate::encode(v)));
        }
        format!("{}/rest/{method}.view?{q}", self.base)
    }

    /// Calls a method; returns the `subsonic-response` object.
    fn call(&self, method: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let opts = Options {
            max_body: 64 << 20,
            ..Default::default()
        };
        let r = client::request("GET", &self.url(method, params), &[], &[], &opts)?;
        if r.status != 200 {
            return Err(format!("HTTP {}", r.status));
        }
        let v = bl_json::parse_bytes(&r.body).map_err(|e| format!("invalid answer: {e}"))?;
        let resp = v.get("subsonic-response").clone();
        if resp.str_or("status", "") != "ok" {
            let e = resp.get("error");
            return Err(match e.get("code").as_u64() {
                Some(40 | 41) => "wrong user name or password".into(),
                _ => e
                    .str_or("message", "the server refused the request")
                    .to_string(),
            });
        }
        Ok(resp)
    }

    fn song(&self, s: &Value) -> Option<RemoteTrack> {
        let id = s.str_or("id", "");
        if id.is_empty() || s.bool_or("isDir", false) || s.bool_or("isVideo", false) {
            return None;
        }
        if let Some(c) = s.get("coverArt").as_str() {
            self.covers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), c.to_string());
        }
        let artists: Vec<String> = match s.get("artists").as_array() {
            // OpenSubsonic: several artists.
            Some(list) if !list.is_empty() => list
                .iter()
                .filter_map(|a| a.get("name").as_str().map(str::to_string))
                .collect(),
            _ => s
                .get("artist")
                .as_str()
                .map(|a| vec![a.to_string()])
                .unwrap_or_default(),
        };
        Some(RemoteTrack {
            key: id.to_string(),
            title: s.str_or("title", "").to_string(),
            artists,
            album: s.str_or("album", "").to_string(),
            album_artist: s
                .get("displayAlbumArtist")
                .as_str()
                .or(s.get("albumArtist").as_str())
                .unwrap_or("")
                .to_string(),
            track: s.get("track").as_u64().unwrap_or(0) as u32,
            disc: s.get("discNumber").as_u64().unwrap_or(0) as u32,
            year: s.get("year").as_u64().unwrap_or(0) as u32,
            genre: s.str_or("genre", "").to_string(),
            duration_ms: s.get("duration").as_u64().unwrap_or(0) * 1000,
            size: s.get("size").as_u64().unwrap_or(0),
            modified: 0,
            ext: s.str_or("suffix", "").to_ascii_lowercase(),
            has_cover: s.get("coverArt").as_str().is_some(),
        })
    }

    /// All songs through `search3` with an empty query (Navidrome,
    /// OpenSubsonic and most others).
    fn by_search(
        &self,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let mut out = Vec::new();
        // The offset counts the server's entries, usable or not.
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let r = self.call(
                "search3",
                &[
                    ("query", String::new()),
                    ("artistCount", "0".into()),
                    ("albumCount", "0".into()),
                    ("songCount", PAGE.to_string()),
                    ("songOffset", offset.to_string()),
                ],
            )?;
            let songs = r
                .get("searchResult3")
                .get("song")
                .as_array()
                .cloned()
                .unwrap_or_default();
            let n = songs.len();
            out.extend(songs.iter().filter_map(|s| self.song(s)));
            if n < PAGE || out.len() >= MAX_SONGS || !progress(out.len()) {
                return Ok(out);
            }
            offset += n;
        }
        Ok(out)
    }

    /// All songs album by album (servers whose search needs a query).
    fn by_albums(
        &self,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let mut out = Vec::new();
        let mut offset = 0;
        // A server that keeps sending full pages ends here too.
        while offset < MAX_ALBUMS {
            let r = self.call(
                "getAlbumList2",
                &[
                    ("type", "alphabeticalByName".into()),
                    ("size", PAGE.to_string()),
                    ("offset", offset.to_string()),
                ],
            )?;
            let albums = r
                .get("albumList2")
                .get("album")
                .as_array()
                .cloned()
                .unwrap_or_default();
            for a in &albums {
                let Some(id) = a.get("id").as_str() else {
                    continue;
                };
                let album = self.call("getAlbum", &[("id", id.to_string())])?;
                for s in album
                    .get("album")
                    .get("song")
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    out.extend(self.song(s));
                }
                if out.len() >= MAX_SONGS || !progress(out.len()) {
                    return Ok(out);
                }
            }
            if albums.len() < PAGE {
                return Ok(out);
            }
            offset += PAGE;
        }
        Ok(out)
    }
}

impl MusicSource for Subsonic {
    fn list(
        &self,
        _known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let songs = self.by_search(progress)?;
        if !songs.is_empty() {
            return Ok(songs);
        }
        self.by_albums(progress)
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let url = self.url(
            "stream",
            &[("id", key.to_string()), ("format", "raw".into())],
        );
        Ok(Box::new(HttpStream::open(&url, &[])?))
    }

    fn cover(&self, key: &str, size: u32) -> Option<Vec<u8>> {
        let id = self
            .covers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string());
        let url = self.url("getCoverArt", &[("id", id), ("size", size.to_string())]);
        let r = client::request("GET", &url, &[], &[], &crate::opts(crate::MAX_COVER)).ok()?;
        let image = r
            .headers
            .get("content-type")
            .is_some_and(|t| t.starts_with("image/"));
        (r.status == 200 && image).then_some(r.body)
    }

    fn played(&self, key: &str) {
        let _ = self.call(
            "scrobble",
            &[("id", key.to_string()), ("submission", "true".into())],
        );
    }

    fn playlists(&self) -> Result<Vec<RemotePlaylist>, String> {
        let r = self.call("getPlaylists", &[])?;
        let mut out = Vec::new();
        for p in r
            .get("playlists")
            .get("playlist")
            .as_array()
            .into_iter()
            .flatten()
            .take(1000)
        {
            let Some(id) = p.get("id").as_str() else {
                continue;
            };
            let full = self.call("getPlaylist", &[("id", id.to_string())])?;
            let keys = full
                .get("playlist")
                .get("entry")
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e.get("id").as_str().map(str::to_string))
                .collect();
            out.push(RemotePlaylist {
                name: p.str_or("name", "").to_string(),
                keys,
            });
        }
        Ok(out)
    }
}
