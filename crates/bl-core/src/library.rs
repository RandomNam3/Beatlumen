//! Music library: local folders (scanned on a background thread on native
//! platforms) and the remote server library, presented as one list.

use bl_net::api::{Album, Library, Track};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Prefix of local track ids (followed by the absolute path).
pub const LOCAL_PREFIX: &str = "file:";

pub fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL_PREFIX)
}

pub fn local_path(id: &str) -> Option<&str> {
    id.strip_prefix(LOCAL_PREFIX)
}

pub fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Splits a multi-artist tag.
pub fn split_artists(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = vec![s.to_string()];
    for sep in ["\u{0}", ";", " / ", " feat. ", " ft. ", " Feat. "] {
        parts = parts.iter().flat_map(|p| p.split(sep).map(str::to_string).collect::<Vec<_>>()).collect();
    }
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub running: bool,
    pub found: usize,
    pub processed: usize,
    pub current: String,
    /// Finished results (taken by the app).
    pub result: Option<Vec<Track>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistEntry {
    pub name: String,
    pub tracks: usize,
    pub albums: usize,
}

#[derive(Default)]
pub struct LibraryState {
    pub local: Vec<Track>,
    pub remote: Library,
    /// Combined, sorted view (local first when both are present).
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<ArtistEntry>,
    index: HashMap<String, usize>,
    album_index: HashMap<String, usize>,
    pub scan: Arc<Mutex<ScanProgress>>,
    pub revision: u64,
}

fn sort_key(t: &Track) -> (String, String, u32, u32, String) {
    let artist = if t.album_artist.is_empty() { t.artists.first().cloned().unwrap_or_default() } else { t.album_artist.clone() };
    (artist.to_lowercase(), t.album.to_lowercase(), t.disc.unwrap_or(0), t.track.unwrap_or(0), t.title.to_lowercase())
}

impl LibraryState {
    pub fn track(&self, id: &str) -> Option<&Track> {
        self.index.get(id).map(|&i| &self.tracks[i])
    }

    pub fn album(&self, id: &str) -> Option<&Album> {
        self.album_index.get(id).map(|&i| &self.albums[i])
    }

    pub fn album_tracks(&self, album_id: &str) -> Vec<&Track> {
        let mut v: Vec<&Track> = self.tracks.iter().filter(|t| t.album_id == album_id).collect();
        v.sort_by_key(|t| (t.disc.unwrap_or(0), t.track.unwrap_or(0), t.title.to_lowercase()));
        v
    }

    pub fn artist_tracks(&self, name: &str) -> Vec<&Track> {
        let n = name.to_lowercase();
        self.tracks.iter().filter(|t| t.artists.iter().any(|a| a.to_lowercase() == n) || t.album_artist.to_lowercase() == n).collect()
    }

    pub fn set_remote(&mut self, lib: Library) {
        self.remote = lib;
        self.rebuild();
    }

    pub fn set_local(&mut self, tracks: Vec<Track>) {
        self.local = tracks;
        self.rebuild();
    }

    /// Rebuilds the combined view, albums and artists.
    pub fn rebuild(&mut self) {
        let mut tracks: Vec<Track> = self.local.iter().cloned().chain(self.remote.tracks.iter().cloned()).collect();
        tracks.sort_by_cached_key(sort_key);
        self.index = tracks.iter().enumerate().map(|(i, t)| (t.id.clone(), i)).collect();
        // Albums: remote albums as delivered, local albums computed.
        let mut albums: Vec<Album> = self.remote.albums.clone();
        let mut local_albums: HashMap<String, Album> = HashMap::new();
        for t in &self.local {
            let a = local_albums.entry(t.album_id.clone()).or_insert_with(|| Album {
                id: t.album_id.clone(),
                name: if t.album.is_empty() { "Unknown Album".into() } else { t.album.clone() },
                artist: if t.album_artist.is_empty() { t.artists.first().cloned().unwrap_or_else(|| "Unknown Artist".into()) } else { t.album_artist.clone() },
                year: t.year,
                track_count: 0,
                duration_ms: 0,
                cover_track: None,
            });
            a.track_count += 1;
            a.duration_ms += t.duration_ms;
            if a.year.is_none() {
                a.year = t.year;
            }
            if a.cover_track.is_none() {
                a.cover_track = Some(t.id.clone());
            }
        }
        albums.extend(local_albums.into_values());
        albums.sort_by_cached_key(|a| (a.artist.to_lowercase(), a.year.unwrap_or(0), a.name.to_lowercase()));
        self.album_index = albums.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        // Artists.
        let mut artists: HashMap<String, (String, usize, std::collections::HashSet<String>)> = HashMap::new();
        for t in &tracks {
            let names: Vec<String> = if t.artists.is_empty() { vec!["Unknown Artist".into()] } else { t.artists.clone() };
            for n in names {
                let e = artists.entry(n.to_lowercase()).or_insert_with(|| (n.clone(), 0, Default::default()));
                e.1 += 1;
                e.2.insert(t.album_id.clone());
            }
        }
        let mut artists: Vec<ArtistEntry> = artists.into_values().map(|(name, tracks, albums)| ArtistEntry { name, tracks, albums: albums.len() }).collect();
        artists.sort_by_cached_key(|a| a.name.to_lowercase());
        self.tracks = tracks;
        self.albums = albums;
        self.artists = artists;
        self.revision += 1;
    }

    /// Case-insensitive search over title, artist and album.
    pub fn search(&self, query: &str) -> Vec<usize> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return (0..self.tracks.len()).collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let hay = format!("{} {} {} {} {}", t.title, t.artists.join(" "), t.album, t.album_artist, t.genre).to_lowercase();
                terms.iter().all(|term| hay.contains(term))
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn load_local_cache(&mut self, data: &[u8]) {
        // Version 2: artist/title guessed from file names without tags.
        if let Some(v) = bl_json::parse_bytes(data).ok().filter(|v| v.get("version").as_u64() == Some(2)) {
            self.local = v.get("tracks").as_array().map(|a| a.iter().map(Track::from_json).collect()).unwrap_or_default();
            self.rebuild();
        }
    }

    pub fn local_cache_json(&self) -> String {
        bl_json::Value::object().with("version", 2u32).with("tracks", bl_json::Value::Array(self.local.iter().map(Track::to_json).collect())).to_json()
    }
}

/// Builds a [`Track`] for a local file from its media info.
pub fn local_track(path: &str, size: u64, modified: u64, info: Option<&bl_media::MediaInfo>) -> Track {
    let file_name = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
    let mut t = Track { id: format!("{LOCAL_PREFIX}{path}"), path: path.to_string(), size, modified, title: file_name.clone(), ..Default::default() };
    if let Some(i) = info {
        let tags = &i.tags;
        if let Some(title) = tags.title.as_ref().filter(|s| !s.trim().is_empty()) {
            t.title = title.trim().to_string();
        }
        t.artists = tags.artist.as_deref().map(split_artists).unwrap_or_default();
        t.album = tags.album.clone().unwrap_or_default();
        t.album_artist = tags.album_artist.clone().unwrap_or_default();
        t.genre = tags.genre.clone().unwrap_or_default();
        t.year = tags.year;
        t.track = tags.track;
        t.disc = tags.disc;
        t.duration_ms = i.duration_ms.unwrap_or(0);
        t.codec = i.codec.to_string();
        t.sample_rate = i.sample_rate;
        t.channels = i.channels;
        t.bitrate_kbps = i.bitrate_kbps;
        t.has_cover = true;
    }
    if t.artists.is_empty() || t.title == file_name {
        let (artist, title) = bl_net::api::guess_from_file_name(&file_name);
        if t.title == file_name {
            t.title = title;
        }
        if t.artists.is_empty() {
            t.artists = artist.as_deref().map(split_artists).unwrap_or_default();
        }
    }
    let album_artist = if t.album_artist.is_empty() { t.artists.first().cloned().unwrap_or_default() } else { t.album_artist.clone() };
    let dir = std::path::Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let key = if t.album.is_empty() { format!("dir:{dir}") } else { format!("{}\u{1}{}", album_artist.to_lowercase(), t.album.to_lowercase()) };
    t.album_id = format!("la:{:016x}", fnv(&key));
    t
}

/// Scans local folders on a background thread (native only). Previously
/// known tracks with unchanged size and modification time are reused.
#[cfg(not(target_arch = "wasm32"))]
pub fn start_local_scan(folders: Vec<String>, known: Vec<Track>, progress: Arc<Mutex<ScanProgress>>) {
    if let Ok(mut p) = progress.lock() {
        if p.running {
            return;
        }
        *p = ScanProgress { running: true, ..Default::default() };
    }
    std::thread::Builder::new()
        .name("bl-scan".into())
        .spawn(move || {
            let known: HashMap<String, Track> = known.into_iter().map(|t| (t.path.clone(), t)).collect();
            let mut files = Vec::new();
            for f in &folders {
                walk(std::path::Path::new(f), &mut files, 0);
                if let Ok(mut p) = progress.lock() {
                    p.found = files.len();
                }
            }
            let mut out = Vec::with_capacity(files.len());
            for (i, (path, size, mtime)) in files.iter().enumerate() {
                if let Some(k) = known.get(path) {
                    if k.size == *size && k.modified == *mtime {
                        out.push(k.clone());
                        continue;
                    }
                }
                let info = std::fs::File::open(path).ok().and_then(|f| {
                    let mut r = std::io::BufReader::new(f);
                    let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase());
                    bl_media::read_info(&mut r, ext.as_deref(), false).ok()
                });
                out.push(local_track(path, *size, *mtime, info.as_ref()));
                if i % 16 == 0 {
                    if let Ok(mut p) = progress.lock() {
                        p.processed = i + 1;
                        p.current = path.clone();
                    }
                }
            }
            if let Ok(mut p) = progress.lock() {
                p.processed = files.len();
                p.running = false;
                p.result = Some(out);
            }
        })
        .ok();
}

#[cfg(not(target_arch = "wasm32"))]
fn walk(dir: &std::path::Path, out: &mut Vec<(String, u64, u64)>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_dir() {
            walk(&path, out, depth + 1);
        } else if ft.is_file() {
            let ext = path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if bl_media::is_supported_extension(&ext) {
                if let Ok(md) = e.metadata() {
                    let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                    out.push((path.to_string_lossy().into_owned(), md.len(), mtime));
                }
            }
        }
    }
}

/// Image file extensions accepted as backgrounds.
pub fn is_image_extension(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp")
}

/// Recursively lists image files below `dir` (native).
#[cfg(not(target_arch = "wasm32"))]
pub fn list_images(dir: &str) -> Vec<String> {
    fn rec(d: &std::path::Path, out: &mut Vec<String>, depth: usize) {
        if depth > 16 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                rec(&p, out, depth + 1);
            } else if p.extension().is_some_and(|x| is_image_extension(&x.to_string_lossy())) {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    let mut out = Vec::new();
    rec(std::path::Path::new(dir), &mut out, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artists_split() {
        assert_eq!(split_artists("A; B / C feat. D"), vec!["A", "B", "C", "D"]);
        assert_eq!(split_artists("AC/DC"), vec!["AC/DC"]);
    }

    #[test]
    fn rebuild_groups_albums() {
        let mut lib = LibraryState::default();
        let mut a = local_track("/m/x/1.mp3", 1, 1, None);
        a.album = "Al".into();
        a.artists = vec!["Ar".into()];
        let mut b = a.clone();
        b.id = "file:/m/x/2.mp3".into();
        b.title = "2".into();
        let key = format!("{}\u{1}{}", "ar", "al");
        a.album_id = format!("la:{:016x}", fnv(&key));
        b.album_id = a.album_id.clone();
        lib.set_local(vec![a, b]);
        assert_eq!(lib.albums.len(), 1);
        assert_eq!(lib.albums[0].track_count, 2);
        assert_eq!(lib.artists[0].name, "Ar");
        assert_eq!(lib.search("ar 2").len(), 1);
    }
}
