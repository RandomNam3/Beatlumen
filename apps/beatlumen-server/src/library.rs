//! The music library: index types, album grouping, the on-disk index cache
//! (`data/library.json`) and the incremental scanner.
//!
//! Track ids are 16 hex characters of the 64-bit FNV-1a hash of
//! `<music root index>/<relative path with forward slashes>`, album ids the
//! hash of the lower-cased `(album artist, album)` key, so ids are stable
//! across rescans and restarts.
//!
//! Scans are incremental: files whose size and modification time match the
//! previous index are not read again. New or changed files are read with
//! `bl_media::read_info` on a few worker threads. Embedded pictures are only
//! looked for in the first audio file of each directory that has no folder
//! image (`cover.jpg`, `folder.png`, ...); other tracks of that directory fall
//! back to that picture.

use crate::util::{self, id_of};
use bl_json::Value;
use bl_net::api::{Album, Track};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Version of the `library.json` cache layout.
const CACHE_FORMAT: u64 = 1;

/// Folder image base names in order of preference (matched case-insensitively,
/// `.jpg`, `.jpeg` and `.png`).
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album"];

/// Directory names never descended into (NAS thumbnail/recycle folders).
const SKIP_DIRS: &[&str] = &["@eaDir", "#recycle", "$RECYCLE.BIN", "System Volume Information"];

/// Maximum directory depth below a music folder.
const MAX_DEPTH: usize = 64;

/// One track of the index together with server-side bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub track: Track,
    /// Index of the music folder.
    pub root: usize,
    /// Path relative to the music folder ('/' separated).
    pub rel: String,
    pub abs: PathBuf,
    pub mtime_ns: u64,
    /// Whether the file has an embedded picture (`None` = not checked).
    pub embedded_cover: Option<bool>,
    /// Folder image of the track's directory (relative to the music folder).
    pub dir_cover_file: Option<String>,
    /// Track of the same directory whose embedded picture serves as fallback.
    pub dir_cover_track: Option<String>,
}

impl Entry {
    pub fn ext(&self) -> String {
        util::ext_lower(&self.abs)
    }
}

/// An immutable library snapshot. A new one replaces the old one atomically
/// after each scan that changed anything.
pub struct Index {
    pub revision: u64,
    pub roots: Vec<PathBuf>,
    pub entries: Vec<Entry>,
    pub albums: Vec<Album>,
    by_id: HashMap<String, usize>,
    album_by_id: HashMap<String, usize>,
    json: Vec<u8>,
}

impl Index {
    /// Builds an index: assigns album ids and cover flags, groups albums and
    /// serializes the `Library` JSON once.
    pub fn new(revision: u64, roots: Vec<PathBuf>, mut entries: Vec<Entry>) -> Index {
        entries.sort_by(|a, b| (a.root, &a.rel).cmp(&(b.root, &b.rel)));
        entries.dedup_by(|a, b| a.track.id == b.track.id);
        for e in &mut entries {
            e.track.has_cover = e.embedded_cover == Some(true) || e.dir_cover_file.is_some() || e.dir_cover_track.is_some();
        }
        let albums = build_albums(&mut entries);
        let by_id = entries.iter().enumerate().map(|(i, e)| (e.track.id.clone(), i)).collect();
        let album_by_id = albums.iter().enumerate().map(|(i, a)| (a.id.clone(), i)).collect();
        let json = Value::object()
            .with("revision", revision)
            .with("tracks", Value::Array(entries.iter().map(|e| e.track.to_json()).collect()))
            .with("albums", Value::Array(albums.iter().map(Album::to_json).collect()))
            .to_json()
            .into_bytes();
        Index { revision, roots, entries, albums, by_id, album_by_id, json }
    }

    pub fn track(&self, id: &str) -> Option<&Entry> {
        self.by_id.get(id).map(|&i| &self.entries[i])
    }

    pub fn album(&self, id: &str) -> Option<&Album> {
        self.album_by_id.get(id).map(|&i| &self.albums[i])
    }

    /// The serialized `bl_net::api::Library` of this revision.
    pub fn json(&self) -> &[u8] {
        &self.json
    }

    /// Absolute path of the folder image of an entry's directory.
    pub fn dir_cover_path(&self, e: &Entry) -> Option<PathBuf> {
        let rel = e.dir_cover_file.as_ref()?;
        util::join_rel(self.roots.get(e.root)?, rel)
    }

    /// True when `entries` (as produced by a scan) differ from this index.
    pub fn differs(&self, entries: &[Entry]) -> bool {
        if entries.len() != self.entries.len() {
            return true;
        }
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        sorted.sort_by(|a, b| (a.root, &a.rel).cmp(&(b.root, &b.rel)));
        sorted.iter().zip(&self.entries).any(|(a, b)| {
            // Derived fields (album id, cover flag) are recomputed by `new`.
            a.rel != b.rel
                || a.root != b.root
                || a.mtime_ns != b.mtime_ns
                || a.embedded_cover != b.embedded_cover
                || a.dir_cover_file != b.dir_cover_file
                || a.dir_cover_track != b.dir_cover_track
                || Track { album_id: String::new(), has_cover: false, ..a.track.clone() } != Track { album_id: String::new(), has_cover: false, ..b.track.clone() }
        })
    }
}

fn album_key(t: &Track) -> String {
    format!("album\u{0}{}\u{0}{}", t.album_artist.trim().to_lowercase(), t.album.trim().to_lowercase())
}

fn build_albums(entries: &mut [Entry]) -> Vec<Album> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter_mut().enumerate() {
        let id = id_of(&album_key(&e.track));
        e.track.album_id = id.clone();
        groups
            .entry(id)
            .or_insert_with_key(|id| {
                order.push(id.clone());
                Vec::new()
            })
            .push(i);
    }
    let mut albums: Vec<Album> = order
        .into_iter()
        .map(|id| {
            let mut members = groups.remove(&id).unwrap_or_default();
            members.sort_by(|&a, &b| {
                let (ta, tb) = (&entries[a].track, &entries[b].track);
                (ta.disc.unwrap_or(1), ta.track.unwrap_or(u32::MAX), &entries[a].rel).cmp(&(tb.disc.unwrap_or(1), tb.track.unwrap_or(u32::MAX), &entries[b].rel))
            });
            let first = &entries[members[0]].track;
            Album {
                id,
                name: first.album.clone(),
                artist: first.album_artist.clone(),
                year: members.iter().find_map(|&m| entries[m].track.year),
                track_count: members.len() as u32,
                duration_ms: members.iter().map(|&m| entries[m].track.duration_ms).sum(),
                cover_track: members.iter().find(|&&m| entries[m].track.has_cover).map(|&m| entries[m].track.id.clone()),
            }
        })
        .collect();
    albums.sort_by(|a, b| (a.artist.to_lowercase(), a.name.to_lowercase(), &a.id).cmp(&(b.artist.to_lowercase(), b.name.to_lowercase(), &b.id)));
    albums
}

/// Splits an artist tag into individual artists (`;`, ` / `, ` feat. `,
/// ` ft. ` and NUL separated ID3v2.4 multi values; case-insensitive).
pub fn split_artists(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = vec![s.to_string()];
    for sep in ["\u{0}", ";", " / ", " feat. ", " ft. "] {
        let mut next = Vec::new();
        for p in parts {
            // ASCII lower-casing keeps byte offsets intact.
            let lower = p.to_ascii_lowercase();
            let mut start = 0;
            let mut search = 0;
            while let Some(i) = lower[search..].find(sep) {
                let at = search + i;
                next.push(p[start..at].to_string());
                start = at + sep.len();
                search = start;
            }
            next.push(p[start..].to_string());
        }
        parts = next;
    }
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        let p = p.trim();
        if !p.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(p)) {
            out.push(p.to_string());
        }
    }
    out
}

/// Trims a tag value and turns NUL separated multi values into "a; b".
fn clean(s: Option<String>) -> Option<String> {
    let s = s?;
    let joined = s.split('\u{0}').map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("; ");
    (!joined.is_empty()).then_some(joined)
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Writes the index cache atomically.
pub fn save_cache(path: &Path, idx: &Index) -> std::io::Result<()> {
    let entries: Vec<Value> = idx
        .entries
        .iter()
        .map(|e| {
            Value::object()
                .with("root", e.root)
                .with("rel", e.rel.as_str())
                .with("mtimeNs", e.mtime_ns.to_string())
                .with("embeddedCover", e.embedded_cover)
                .with("dirCoverFile", e.dir_cover_file.clone())
                .with("dirCoverTrack", e.dir_cover_track.clone())
                .with("track", e.track.to_json())
        })
        .collect();
    let roots: Vec<Value> = idx.roots.iter().map(|r| Value::from(r.to_string_lossy().into_owned())).collect();
    let v = Value::object().with("format", CACHE_FORMAT).with("revision", idx.revision).with("roots", Value::Array(roots)).with("entries", Value::Array(entries));
    util::write_atomic(path, v.to_json().as_bytes())
}

/// Loads the index cache. Entries of music folders that are no longer
/// configured at the same position are dropped.
pub fn load_cache(path: &Path, roots: &[PathBuf]) -> Option<(u64, Vec<Entry>)> {
    let data = std::fs::read(path).ok()?;
    let v = match bl_json::parse_bytes(&data) {
        Ok(v) => v,
        Err(e) => {
            crate::warn!("ignoring damaged library cache {}: {e}", path.display());
            return None;
        }
    };
    if v.get("format").as_u64() != Some(CACHE_FORMAT) {
        return None;
    }
    let revision = v.get("revision").as_u64()?;
    let cached_roots: Vec<&str> = v.get("roots").as_array()?.iter().map(|r| r.as_str().unwrap_or("")).collect();
    let mut out = Vec::new();
    for e in v.get("entries").as_array()? {
        let Some(root) = e.get("root").as_u64().map(|r| r as usize) else { continue };
        let Some(root_path) = roots.get(root) else { continue };
        if cached_roots.get(root).copied() != Some(&*root_path.to_string_lossy()) {
            continue;
        }
        let Some(rel) = e.get("rel").as_str() else { continue };
        let Some(abs) = util::join_rel(root_path, rel) else { continue };
        let track = Track::from_json(e.get("track"));
        if track.id != id_of(&format!("{root}/{rel}")) {
            continue;
        }
        out.push(Entry {
            track,
            root,
            rel: rel.to_string(),
            abs,
            mtime_ns: e.get("mtimeNs").as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            embedded_cover: e.get("embeddedCover").as_bool(),
            dir_cover_file: e.get("dirCoverFile").as_str().map(str::to_string),
            dir_cover_track: e.get("dirCoverTrack").as_str().map(str::to_string),
        });
    }
    Some((revision, out))
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// An audio file found while walking the music folders.
struct FileRec {
    root: usize,
    rel: String,
    abs: PathBuf,
    size: u64,
    mtime_ns: u64,
    dir: usize,
    first_in_dir: bool,
}

/// Per-directory information collected while walking.
struct DirRec {
    cover_rel: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ScanStats {
    pub dirs: usize,
    pub files: usize,
    /// Files whose tags were (re)read.
    pub read: usize,
    /// Files whose tags could not be parsed (listed with the file name).
    pub tag_errors: usize,
    /// Files that could not be opened (not listed).
    pub skipped: usize,
}

/// Walks one music folder. Returns false if the folder is missing.
fn walk(root_idx: usize, root: &Path, files: &mut Vec<FileRec>, dirs: &mut Vec<DirRec>, visited: &mut HashSet<PathBuf>) -> bool {
    if !root.is_dir() {
        return false;
    }
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        // Following symlinked directories is allowed, loops are not.
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited.insert(canonical) {
            continue;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => {
                crate::warn!("cannot read directory {}: {e}", dir.display());
                continue;
            }
        };
        let mut here: Vec<(String, PathBuf, std::fs::Metadata)> = Vec::new();
        let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
        for e in rd.flatten() {
            let Ok(name) = e.file_name().into_string() else {
                crate::debug!("skipping non UTF-8 file name in {}", dir.display());
                continue;
            };
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = e.path();
            let meta = match e.file_type() {
                Ok(ft) if ft.is_symlink() => std::fs::metadata(&path),
                Ok(_) => e.metadata(),
                Err(err) => Err(err),
            };
            let Ok(meta) = meta else { continue };
            if meta.is_dir() {
                subdirs.push((name, path));
            } else if meta.is_file() {
                here.push((name, path, meta));
            }
        }
        here.sort_by(|a, b| a.0.cmp(&b.0));
        subdirs.sort_by(|a, b| b.0.cmp(&a.0)); // reversed: popped in order
        let join = |name: &str| if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
        let cover_rel = COVER_NAMES.iter().find_map(|base| {
            ["jpg", "jpeg", "png"].iter().find_map(|ext| {
                here.iter().find(|(n, _, m)| m.len() > 0 && n.eq_ignore_ascii_case(&format!("{base}.{ext}"))).map(|(n, _, _)| join(n))
            })
        });
        let dir_idx = dirs.len();
        dirs.push(DirRec { cover_rel });
        let mut first = true;
        for (name, path, meta) in here {
            let ext = util::ext_lower(Path::new(&name));
            if ext.is_empty() || !bl_media::is_supported_extension(&ext) {
                continue;
            }
            files.push(FileRec { root: root_idx, rel: join(&name), abs: path, size: meta.len(), mtime_ns: util::mtime_ns(&meta), dir: dir_idx, first_in_dir: first });
            first = false;
        }
        if depth < MAX_DEPTH {
            for (name, path) in subdirs {
                stack.push((path, join(&name), depth + 1));
            }
        } else if !subdirs.is_empty() {
            crate::warn!("not descending below {} (maximum depth reached)", dir.display());
        }
    }
    true
}

/// Result of reading one file: the entry (`None` if the file cannot be
/// opened) and whether its tags could not be parsed.
type ReadResult = (Option<Entry>, bool);

/// Reads the tags of one file (the track is listed even when its tags cannot
/// be parsed).
fn read_entry(rec: &FileRec, want_pictures: bool) -> ReadResult {
    let mut file = match std::fs::File::open(&rec.abs) {
        Ok(f) => f,
        Err(e) => {
            crate::warn!("skipping unreadable file {}: {e}", rec.abs.display());
            return (None, false);
        }
    };
    let ext = util::ext_lower(&rec.abs);
    let stem = rec.abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut track = Track {
        id: id_of(&format!("{}/{}", rec.root, rec.rel)),
        title: stem.clone(),
        codec: ext.clone(),
        size: rec.size,
        path: rec.rel.clone(),
        modified: rec.mtime_ns / 1_000_000_000,
        ..Default::default()
    };
    let mut embedded = None;
    let mut failed = false;
    match util::catch("tag reader", || bl_media::read_info(&mut file, Some(&ext), want_pictures)) {
        Ok(Ok(info)) => {
            let t = info.tags;
            track.title = clean(t.title).unwrap_or(stem);
            track.artists = t.artist.as_deref().map(split_artists).unwrap_or_default();
            track.album = clean(t.album).unwrap_or_default();
            track.album_artist = clean(t.album_artist).or_else(|| track.artists.first().cloned()).unwrap_or_default();
            track.genre = clean(t.genre).unwrap_or_default();
            track.year = t.year.filter(|y| (1..=9999).contains(y));
            track.track = t.track.filter(|&n| n > 0);
            track.disc = t.disc.filter(|&n| n > 0);
            track.duration_ms = info.duration_ms.unwrap_or(0);
            track.codec = info.codec.to_string();
            track.sample_rate = info.sample_rate;
            track.channels = info.channels;
            track.bitrate_kbps = info.bitrate_kbps.filter(|&b| b > 0);
            if want_pictures {
                embedded = Some(info.pictures.iter().any(|p| !p.data.is_empty()));
            }
        }
        Ok(Err(e)) => {
            crate::debug!("cannot read tags of {}: {e}", rec.abs.display());
            failed = true;
        }
        Err(_) => failed = true,
    }
    if want_pictures && embedded.is_none() {
        embedded = Some(false);
    }
    let entry = Entry { track, root: rec.root, rel: rec.rel.clone(), abs: rec.abs.clone(), mtime_ns: rec.mtime_ns, embedded_cover: embedded, dir_cover_file: None, dir_cover_track: None };
    (Some(entry), failed)
}

/// Assigns the directory level cover information.
fn assign_dir_covers(files: &[FileRec], dirs: &[DirRec], entries: &mut [Option<Entry>]) {
    let mut first_of_dir: HashMap<usize, (String, bool)> = HashMap::new();
    for (rec, e) in files.iter().zip(entries.iter()) {
        if let Some(e) = e {
            first_of_dir.entry(rec.dir).or_insert_with(|| (e.track.id.clone(), e.embedded_cover == Some(true)));
        }
    }
    for (rec, e) in files.iter().zip(entries.iter_mut()) {
        let Some(e) = e else { continue };
        let d = &dirs[rec.dir];
        e.dir_cover_file = d.cover_rel.clone();
        e.dir_cover_track = match (&d.cover_rel, first_of_dir.get(&rec.dir)) {
            (None, Some((id, true))) if *id != e.track.id => Some(id.clone()),
            _ => None,
        };
    }
}

/// Scans all music folders. `old` provides cached entries for unchanged
/// files; `publish` receives intermediate results every few seconds while
/// many files are read (first scan of a large library).
pub fn scan(roots: &[PathBuf], old: &Index, threads: usize, publish: &mut dyn FnMut(Vec<Entry>)) -> (Vec<Entry>, ScanStats) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut visited = HashSet::new();
    let mut missing = HashSet::new();
    for (i, root) in roots.iter().enumerate() {
        if !walk(i, root, &mut files, &mut dirs, &mut visited) {
            // An unmounted drive must not wipe its part of the library.
            crate::warn!("music folder {} is missing or not a directory; keeping its previous entries", root.display());
            missing.insert(i);
        }
    }
    let mut stats = ScanStats { dirs: dirs.len(), files: files.len(), ..Default::default() };

    let old_map: HashMap<(usize, &str), &Entry> =
        if old.roots == roots { old.entries.iter().map(|e| ((e.root, e.rel.as_str()), e)).collect() } else { HashMap::new() };
    let mut entries: Vec<Option<Entry>> = Vec::with_capacity(files.len());
    let mut to_read: Vec<(usize, bool)> = Vec::new();
    for (i, rec) in files.iter().enumerate() {
        let need_pictures = rec.first_in_dir && dirs[rec.dir].cover_rel.is_none();
        match old_map.get(&(rec.root, rec.rel.as_str())) {
            Some(e) if e.track.size == rec.size && e.mtime_ns == rec.mtime_ns && (!need_pictures || e.embedded_cover.is_some()) => {
                entries.push(Some((*e).clone()));
            }
            _ => {
                entries.push(None);
                to_read.push((i, need_pictures));
            }
        }
    }
    stats.read = to_read.len();

    if !to_read.is_empty() {
        let threads = if threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4) } else { threads.clamp(1, 64) };
        let results: Mutex<Vec<Option<ReadResult>>> = Mutex::new(vec![None; to_read.len()]);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads.min(to_read.len()) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(fi, pics)) = to_read.get(i) else { break };
                    let r = read_entry(&files[fi], pics);
                    results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                    done.fetch_add(1, Ordering::Relaxed);
                });
            }
            // Publish partial results while a large batch is being read.
            let mut last = Instant::now();
            while done.load(Ordering::Relaxed) < to_read.len() {
                std::thread::sleep(Duration::from_millis(100));
                if last.elapsed() >= Duration::from_secs(5) {
                    last = Instant::now();
                    let mut partial = entries.clone();
                    {
                        let res = results.lock().unwrap_or_else(|e| e.into_inner());
                        for (k, &(fi, _)) in to_read.iter().enumerate() {
                            partial[fi] = match &res[k] {
                                Some((e, _)) => e.clone(),
                                None => old_map.get(&(files[fi].root, files[fi].rel.as_str())).map(|e| (*e).clone()),
                            };
                        }
                    }
                    assign_dir_covers(&files, &dirs, &mut partial);
                    let mut partial: Vec<Entry> = partial.into_iter().flatten().collect();
                    if old.roots == roots {
                        partial.extend(old.entries.iter().filter(|e| missing.contains(&e.root)).cloned());
                    }
                    publish(partial);
                }
            }
        });
        let res = results.into_inner().unwrap_or_else(|e| e.into_inner());
        for (k, r) in res.into_iter().enumerate() {
            let (e, failed) = r.unwrap_or((None, false));
            if e.is_none() {
                stats.skipped += 1;
            }
            if failed {
                stats.tag_errors += 1;
            }
            entries[to_read[k].0] = e;
        }
    }
    assign_dir_covers(&files, &dirs, &mut entries);
    let mut out: Vec<Entry> = entries.into_iter().flatten().collect();
    if old.roots == roots {
        out.extend(old.entries.iter().filter(|e| missing.contains(&e.root)).cloned());
    }
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_splitting() {
        assert_eq!(split_artists("A; B;C"), vec!["A", "B", "C"]);
        assert_eq!(split_artists("AC/DC"), vec!["AC/DC"]);
        assert_eq!(split_artists("A / B"), vec!["A", "B"]);
        assert_eq!(split_artists("A feat. B"), vec!["A", "B"]);
        assert_eq!(split_artists("A Feat. B ft. C"), vec!["A", "B", "C"]);
        assert_eq!(split_artists("A\u{0}B\u{0}a"), vec!["A", "B"]);
        assert_eq!(split_artists("Ünïcödé FEAT. Ärtist"), vec!["Ünïcödé", "Ärtist"]);
        assert!(split_artists(" ; ").is_empty());
    }

    fn entry(root: usize, rel: &str, album: &str, artist: &str, track: Option<u32>, cover: bool) -> Entry {
        Entry {
            track: Track {
                id: id_of(&format!("{root}/{rel}")),
                title: rel.into(),
                album: album.into(),
                album_artist: artist.into(),
                artists: vec![artist.into()],
                track,
                duration_ms: 1000,
                ..Default::default()
            },
            root,
            rel: rel.into(),
            abs: PathBuf::from(rel),
            mtime_ns: 1,
            embedded_cover: Some(cover),
            dir_cover_file: None,
            dir_cover_track: None,
        }
    }

    #[test]
    fn albums_group_case_insensitively() {
        let idx = Index::new(
            3,
            vec![PathBuf::from("/m")],
            vec![
                entry(0, "a/2.wav", "My Album", "Band", Some(2), true),
                entry(0, "a/1.wav", "my album", "BAND", Some(1), false),
                entry(0, "b/1.wav", "Other", "Band", None, false),
            ],
        );
        assert_eq!(idx.albums.len(), 2);
        let a = idx.albums.iter().find(|a| a.track_count == 2).unwrap();
        assert_eq!(a.duration_ms, 2000);
        assert_eq!(a.cover_track.as_deref(), Some(id_of("0/a/2.wav").as_str()));
        assert_eq!(idx.track(&id_of("0/a/1.wav")).unwrap().track.album_id, a.id);
        assert!(idx.album(&a.id).is_some());
        let lib = bl_net::api::Library::from_json(&bl_json::parse_bytes(idx.json()).unwrap());
        assert_eq!((lib.revision, lib.tracks.len(), lib.albums.len()), (3, 3, 2));
        assert!(!idx.differs(&idx.entries.clone()));
        let mut changed = idx.entries.clone();
        changed[0].track.title = "x".into();
        assert!(idx.differs(&changed));
    }
}
