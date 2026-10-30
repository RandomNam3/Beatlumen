//! Jellyfin: the audio items of a user's libraries with their metadata,
//! the original files (`static=true`, no transcoding), covers, playlists,
//! and playback reported back (play counts and "played").
//!
//! The login (`/Users/AuthenticateByName`) yields an access token; only
//! the token is kept in memory and sent in the `Authorization` header.

use crate::{MusicSource, RemotePlaylist, RemoteTrack, SourceConfig};
use bl_json::Value;
use bl_net::client::{self, HttpStream, Options};
use std::sync::Mutex;

const PAGE: usize = 500;
const MAX_SONGS: usize = 500_000;
/// Upper bound of pages asked for in one listing (also when the pages
/// hold nothing usable).
const MAX_PAGES: usize = MAX_SONGS / PAGE + 1;

pub struct Jellyfin {
    base: String,
    user_id: String,
    token: String,
    device_id: String,
    /// Song id -> album id (album covers when a song has none).
    albums: Mutex<std::collections::HashMap<String, String>>,
}

fn authorization(device_id: &str, token: Option<&str>) -> String {
    let mut h = format!(
        "MediaBrowser Client=\"Beatlumen\", Device=\"Beatlumen\", DeviceId=\"{device_id}\", Version=\"{}\"",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(t) = token {
        h.push_str(&format!(", Token=\"{t}\""));
    }
    h
}

impl Jellyfin {
    pub fn connect(cfg: &SourceConfig) -> Result<Jellyfin, String> {
        if !(cfg.url.starts_with("http://") || cfg.url.starts_with("https://")) {
            return Err("the address must start with http:// or https://".into());
        }
        let base = cfg.url.trim_end_matches('/').to_string();
        // Stable per server and user, so Jellyfin lists one device.
        let device_id = crate::md5::md5_hex(format!("beatlumen|{base}|{}", cfg.user).as_bytes());
        let body = Value::object()
            .with("Username", cfg.user.as_str())
            .with("Pw", cfg.secret.as_str());
        let r = client::request(
            "POST",
            &format!("{base}/Users/AuthenticateByName"),
            &[
                ("Authorization".into(), authorization(&device_id, None)),
                ("Content-Type".into(), "application/json".into()),
            ],
            body.to_json().as_bytes(),
            &crate::opts(crate::MAX_SMALL),
        )?;
        match r.status {
            200 => {}
            401 | 403 => return Err("wrong user name or password".into()),
            s => return Err(format!("HTTP {s}")),
        }
        let v = bl_json::parse_bytes(&r.body).map_err(|e| format!("invalid answer: {e}"))?;
        let token = v.str_or("AccessToken", "").to_string();
        let user_id = v.get("User").str_or("Id", "").to_string();
        if token.is_empty() || user_id.is_empty() {
            return Err("the server sent no access token".into());
        }
        Ok(Jellyfin {
            base,
            user_id,
            token,
            device_id,
            albums: Mutex::new(Default::default()),
        })
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![(
            "Authorization".into(),
            authorization(&self.device_id, Some(&self.token)),
        )]
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let opts = Options {
            max_body: 64 << 20,
            ..Default::default()
        };
        let r = client::request(
            "GET",
            &format!("{}{path}", self.base),
            &self.headers(),
            &[],
            &opts,
        )?;
        if r.status != 200 {
            return Err(format!("HTTP {}", r.status));
        }
        bl_json::parse_bytes(&r.body).map_err(|e| format!("invalid answer: {e}"))
    }

    fn song(&self, i: &Value) -> Option<RemoteTrack> {
        let id = i.str_or("Id", "");
        if id.is_empty() {
            return None;
        }
        if let Some(a) = i.get("AlbumId").as_str() {
            self.albums
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), a.to_string());
        }
        let strings = |k: &str| -> Vec<String> {
            i.get(k)
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        };
        let source = i.get("MediaSources").at(0);
        let ext = source
            .get("Container")
            .as_str()
            .or(i.get("Container").as_str())
            .unwrap_or("")
            .split(',')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Some(RemoteTrack {
            key: id.to_string(),
            title: i.str_or("Name", "").to_string(),
            artists: strings("Artists"),
            album: i.str_or("Album", "").to_string(),
            album_artist: i.str_or("AlbumArtist", "").to_string(),
            track: i.get("IndexNumber").as_u64().unwrap_or(0) as u32,
            disc: i.get("ParentIndexNumber").as_u64().unwrap_or(0) as u32,
            year: i.get("ProductionYear").as_u64().unwrap_or(0) as u32,
            genre: strings("Genres").into_iter().next().unwrap_or_default(),
            // Ticks of 100 ns.
            duration_ms: i.get("RunTimeTicks").as_u64().unwrap_or(0) / 10_000,
            size: source.get("Size").as_u64().unwrap_or(0),
            modified: 0,
            ext,
            has_cover: i.get("ImageTags").get("Primary").as_str().is_some()
                || i.get("AlbumPrimaryImageTag").as_str().is_some(),
        })
    }
}

impl MusicSource for Jellyfin {
    fn list(
        &self,
        _known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let mut out = Vec::new();
        // The start counts the server's items, usable or not.
        let mut start = 0usize;
        for _ in 0..MAX_PAGES {
            let v = self.get(&format!(
                "/Users/{}/Items?IncludeItemTypes=Audio&Recursive=true&Fields=MediaSources,Genres&SortBy=SortName&StartIndex={start}&Limit={PAGE}",
                self.user_id,
            ))?;
            let items = v.get("Items").as_array().cloned().unwrap_or_default();
            out.extend(items.iter().filter_map(|i| self.song(i)));
            if items.len() < PAGE || out.len() >= MAX_SONGS || !progress(out.len()) {
                return Ok(out);
            }
            start += items.len();
        }
        Ok(out)
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let url = format!(
            "{}/Audio/{}/stream?static=true",
            self.base,
            crate::encode(key)
        );
        Ok(Box::new(HttpStream::open(&url, &self.headers())?))
    }

    fn cover(&self, key: &str, size: u32) -> Option<Vec<u8>> {
        let album = self
            .albums
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        for id in std::iter::once(key.to_string()).chain(album) {
            let url = format!(
                "{}/Items/{}/Images/Primary?maxHeight={size}&maxWidth={size}",
                self.base,
                crate::encode(&id)
            );
            let opts = crate::opts(crate::MAX_COVER);
            if let Ok(r) = client::request("GET", &url, &self.headers(), &[], &opts) {
                let image = r
                    .headers
                    .get("content-type")
                    .is_some_and(|t| t.starts_with("image/"));
                if r.status == 200 && image {
                    return Some(r.body);
                }
            }
        }
        None
    }

    fn played(&self, key: &str) {
        let _ = client::request(
            "POST",
            &format!(
                "{}/Users/{}/PlayedItems/{}",
                self.base,
                self.user_id,
                crate::encode(key)
            ),
            &self.headers(),
            &[],
            &crate::opts(crate::MAX_SMALL),
        );
    }

    fn playlists(&self) -> Result<Vec<RemotePlaylist>, String> {
        let v = self.get(&format!(
            "/Users/{}/Items?IncludeItemTypes=Playlist&Recursive=true",
            self.user_id
        ))?;
        let mut out = Vec::new();
        for p in v.get("Items").as_array().into_iter().flatten().take(1000) {
            let Some(id) = p.get("Id").as_str() else {
                continue;
            };
            let items = self.get(&format!("/Playlists/{id}/Items?UserId={}", self.user_id))?;
            let keys = items
                .get("Items")
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i.str_or("Type", "") == "Audio")
                .filter_map(|i| i.get("Id").as_str().map(str::to_string))
                .collect();
            out.push(RemotePlaylist {
                name: p.str_or("Name", "").to_string(),
                keys,
            });
        }
        Ok(out)
    }
}
