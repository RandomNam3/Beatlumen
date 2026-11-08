//! Background image folders that stay in sync with the list, and the check
//! that keeps an image from being added twice.
//!
//! The core remembers, per folder, every image file it has seen (with size
//! and modification time). A scan compares the folder with that memory:
//! files it has not seen are added to the list (unless the list already
//! holds the same image), files that are gone leave the list, and changed
//! files are shown anew. Images the user removed from the list stay seen,
//! so they do not come back. Scans run on a worker thread: on return to
//! the app, every [`RESCAN_SECS`] while it is on screen, and when a folder
//! is added.
//!
//! Duplicates are found by content, not by name: two files are the same
//! image only if they have the same size, and only then are their bytes
//! compared (in chunks, so even a 16K image needs little memory).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bl_json::Value;

use crate::host::Host;
use crate::library::LOCAL_PREFIX;

/// Seconds between scans while the app is on screen.
pub const RESCAN_SECS: f64 = 20.0;

/// Where the folders and what was seen in them are kept.
pub const STORAGE_KEY: &str = "background-folders.json";

/// Size and modification time (ns since 1970) of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
}

impl Stamp {
    pub fn of(path: &str) -> std::io::Result<Stamp> {
        let md = std::fs::metadata(path)?;
        if !md.is_file() {
            return Err(std::io::Error::other("not a file"));
        }
        let modified = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        Ok(Stamp {
            size: md.len(),
            modified,
        })
    }
}

/// A synced folder and the image files seen in it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Folder {
    pub path: String,
    pub seen: HashMap<String, Stamp>,
}

/// The synced folders, loaded from and saved to [`STORAGE_KEY`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Folders {
    pub list: Vec<Folder>,
}

impl Folders {
    pub fn parse(data: &[u8]) -> Folders {
        let Ok(v) = bl_json::parse_bytes(data) else {
            return Folders::default();
        };
        let list = v
            .get("folders")
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let path = f.get("path").as_str()?.to_string();
                        let seen = f
                            .get("seen")
                            .as_array()
                            .map(|s| {
                                s.iter()
                                    .filter_map(|e| {
                                        Some((
                                            e.at(0).as_str()?.to_string(),
                                            Stamp {
                                                size: e.at(1).as_f64()? as u64,
                                                modified: e
                                                    .at(2)
                                                    .as_str()
                                                    .and_then(|m| m.parse().ok())
                                                    .unwrap_or(0),
                                            },
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        Some(Folder { path, seen })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Folders { list }
    }

    pub fn to_json(&self) -> String {
        let folders: Vec<Value> = self
            .list
            .iter()
            .map(|f| {
                let mut seen: Vec<_> = f.seen.iter().collect();
                seen.sort_by(|a, b| a.0.cmp(b.0));
                Value::object().with("path", f.path.as_str()).with(
                    "seen",
                    Value::Array(
                        seen.into_iter()
                            .map(|(p, s)| {
                                // The time as text: ns do not fit a double.
                                Value::Array(vec![
                                    p.as_str().into(),
                                    (s.size as f64).into(),
                                    s.modified.to_string().into(),
                                ])
                            })
                            .collect(),
                    ),
                )
            })
            .collect();
        Value::object()
            .with("folders", Value::Array(folders))
            .to_string()
    }

    #[cfg(test)]
    pub fn paths(&self) -> Vec<String> {
        self.list.iter().map(|f| f.path.clone()).collect()
    }

    /// The synced folder that holds `path`, if any.
    #[cfg(test)]
    pub fn holding(&self, path: &str) -> Option<&Folder> {
        self.list.iter().find(|f| within(path, &f.path))
    }

    /// Adds a folder (or starts it over, if it is already synced: all its
    /// images count as new again). Synced folders inside it are merged into
    /// it; inside a synced folder, that folder starts over.
    pub fn add(&mut self, path: &str) {
        let path = trim_separator(path);
        if let Some(f) = self.list.iter_mut().find(|f| within(&path, &f.path)) {
            f.seen.clear();
            return;
        }
        self.list.retain(|f| !within(&f.path, &path));
        self.list.push(Folder {
            path,
            seen: HashMap::new(),
        });
    }

    /// Stops syncing a folder; returns whether it was synced.
    pub fn remove(&mut self, path: &str) -> bool {
        let path = trim_separator(path);
        let before = self.list.len();
        self.list.retain(|f| f.path != path);
        self.list.len() != before
    }
}

fn trim_separator(path: &str) -> String {
    let t = path.trim_end_matches(['/', '\\']);
    if t.is_empty() || t.ends_with(':') {
        path.to_string()
    } else {
        t.to_string()
    }
}

/// Whether `path` is `dir` or lies below it.
pub fn within(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches(['/', '\\']);
    path == dir
        || (path.len() > dir.len()
            && path.starts_with(dir)
            && matches!(path.as_bytes()[dir.len()], b'/' | b'\\'))
}

/// What a scan works on: a snapshot taken by the core.
#[derive(Debug, Clone, Default)]
pub struct Job {
    pub folders: Vec<Folder>,
    /// Local image paths in the background list.
    pub listed: Vec<String>,
    /// Images the user just added (in this order), checked for duplicates.
    pub added: Vec<String>,
}

/// What a scan found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// The folders with what is now seen in them; `None` for a folder that
    /// could not be read (its images stay as they are).
    pub folders: Vec<(String, Option<HashMap<String, Stamp>>)>,
    /// Paths to append to the list, in order.
    pub add: Vec<String>,
    /// Listed paths whose files are gone from a readable folder.
    pub gone: Vec<String>,
    /// Listed paths whose files changed since they were last seen.
    pub changed: Vec<String>,
    /// For each image the user added that the list already holds: the path
    /// and the listed (or just added) image it equals.
    pub duplicates: Vec<(String, String)>,
}

impl Outcome {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.add.is_empty()
            && self.gone.is_empty()
            && self.changed.is_empty()
            && self.duplicates.is_empty()
    }
}

/// Image files below `dir` with their stamps (sorted by path); `None` if
/// `dir` cannot be read.
fn list_folder(dir: &str) -> Option<Vec<(String, Stamp)>> {
    std::fs::read_dir(dir).ok()?;
    let mut out = Vec::new();
    for p in crate::library::list_images(dir) {
        if let Ok(s) = Stamp::of(&p) {
            out.push((p, s));
        }
    }
    Some(out)
}

/// Whether two files of the same size have the same bytes.
fn same_content(a: &str, b: &str) -> bool {
    use std::io::Read;
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let mut ba = vec![0u8; 256 << 10];
    let mut bb = vec![0u8; 256 << 10];
    loop {
        let n = match read_full(&mut fa, &mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        match read_full(&mut fb, &mut bb[..n.max(1)]) {
            Ok(m) if m == n => {}
            _ => return false,
        }
        if n == 0 {
            return true;
        }
        if ba[..n] != bb[..n] {
            return false;
        }
    }
    fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
}

/// Listed images by size, to find duplicates.
#[derive(Default)]
struct Known {
    by_size: HashMap<u64, Vec<String>>,
    paths: HashSet<String>,
}

impl Known {
    fn insert(&mut self, path: &str, size: u64) {
        if self.paths.insert(path.to_string()) {
            self.by_size.entry(size).or_default().push(path.to_string());
        }
    }

    /// The known image `path` (of `size` bytes) equals, if any.
    fn duplicate_of(&self, path: &str, size: u64) -> Option<String> {
        if self.paths.contains(path) {
            return Some(path.to_string());
        }
        self.by_size
            .get(&size)?
            .iter()
            .find(|k| same_content(k, path))
            .cloned()
    }
}

/// Runs a scan (blocking: file system only).
pub fn scan(job: Job) -> Outcome {
    let mut out = Outcome::default();
    let mut known = Known::default();
    let mut stamps: HashMap<&str, Stamp> = HashMap::new();
    let mut missing: HashSet<&str> = HashSet::new();
    for p in &job.listed {
        match Stamp::of(p) {
            Ok(s) => {
                known.insert(p, s.size);
                stamps.insert(p, s);
            }
            Err(_) => {
                missing.insert(p);
            }
        }
    }

    // What the user added comes first (they see the result at once).
    for p in &job.added {
        let Ok(s) = Stamp::of(p) else { continue };
        match known.duplicate_of(p, s.size) {
            Some(d) => out.duplicates.push((p.clone(), d)),
            None => {
                known.insert(p, s.size);
                out.add.push(p.clone());
            }
        }
    }

    for f in &job.folders {
        let Some(files) = list_folder(&f.path) else {
            out.folders.push((f.path.clone(), None));
            continue;
        };
        let mut seen = HashMap::with_capacity(files.len());
        for (p, s) in files {
            match f.seen.get(&p) {
                Some(old) if *old == s => {}
                Some(_) => {
                    if stamps.contains_key(p.as_str()) {
                        out.changed.push(p.clone());
                    }
                }
                None => {
                    if known.duplicate_of(&p, s.size).is_none() {
                        known.insert(&p, s.size);
                        out.add.push(p.clone());
                    }
                }
            }
            seen.insert(p, s);
        }
        // Gone from a folder that could be read: the file is deleted.
        for p in &job.listed {
            if within(p, &f.path) && missing.contains(p.as_str()) && !seen.contains_key(p) {
                out.gone.push(p.clone());
            }
        }
        out.folders.push((f.path.clone(), Some(seen)));
    }
    out
}

/// A scan running on a worker thread.
#[derive(Default)]
pub struct Sync {
    pub folders: Folders,
    /// When the last scan started (core time), `None` before the first.
    pub last: Option<f64>,
    running: Option<Arc<Mutex<Option<Outcome>>>>,
    /// Images added while a scan ran, and whether to show the first.
    pub queued: Vec<String>,
    pub queued_show: bool,
    /// A scan was asked for while one ran.
    pub again: bool,
    /// What the scan that runs was started for: images the user added
    /// (and whether to show them), and a folder the user added.
    pub running_added: Vec<String>,
    pub running_show: bool,
    pub running_folder: Option<String>,
    pub queued_folder: Option<String>,
    epoch: u64,
    running_epoch: u64,
}

impl Sync {
    pub fn new(folders: Folders) -> Sync {
        Sync {
            folders,
            ..Default::default()
        }
    }

    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Starts a scan on a new thread.
    pub fn start(&mut self, job: Job, now: f64) {
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        self.last = Some(now);
        self.running = Some(slot);
        std::thread::Builder::new()
            .name("bg-folders".into())
            .spawn(move || {
                let r = scan(job);
                if let Ok(mut o) = out.lock() {
                    *o = Some(r);
                }
            })
            .ok();
    }

    /// The outcome of the running scan, once it is done.
    pub fn take(&mut self) -> Option<Outcome> {
        let done = self.running.as_ref()?.lock().ok()?.take();
        if done.is_some() {
            self.running = None;
        }
        done
    }
}

/// Epoch of the folder list: a scan started before a folder was added or
/// removed is not applied.
impl Sync {
    fn changed_folders(&mut self) {
        self.epoch += 1;
    }
}

impl crate::core::Core {
    /// Local images the user adds go through the duplicate check (on the
    /// scan thread); the outcome adds them.
    pub(crate) fn queue_local_backgrounds(&mut self, paths: Vec<String>, show: bool) {
        if paths.is_empty() {
            return;
        }
        self.bg_sync.queued.extend(paths);
        self.bg_sync.queued_show |= show;
    }

    pub(crate) fn add_background_folder(&mut self, host: &mut dyn Host, path: &str) {
        if path.is_empty() {
            return;
        }
        self.bg_sync.folders.add(path);
        self.bg_sync.changed_folders();
        self.bg_sync.queued_folder = Some(trim_separator(path));
        self.save_background_folders(host);
        self.revs.backgrounds += 1;
    }

    /// Stops syncing a folder and takes its images off the list (the files
    /// stay).
    pub(crate) fn remove_background_folder(&mut self, host: &mut dyn Host, path: &str) {
        let path = trim_separator(path);
        if !self.bg_sync.folders.remove(&path) {
            return;
        }
        self.bg_sync.changed_folders();
        self.save_background_folders(host);
        let gone: HashSet<String> = self
            .bg_queue
            .items
            .iter()
            .filter(|s| {
                s.strip_prefix(LOCAL_PREFIX)
                    .is_some_and(|p| within(p, &path))
            })
            .cloned()
            .collect();
        self.remove_backgrounds(host, &gone);
        self.revs.backgrounds += 1;
    }

    /// Scans soon (when the app returns, or the list is opened).
    pub(crate) fn sync_background_folders_soon(&mut self) {
        if !self.bg_sync.running() {
            self.bg_sync.last = None;
        }
    }

    pub(crate) fn background_folders_json(&self) -> Value {
        Value::Array(
            self.bg_sync
                .folders
                .list
                .iter()
                .map(|f| {
                    let name = std::path::Path::new(&f.path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| f.path.clone());
                    Value::object()
                        .with("path", f.path.as_str())
                        .with("name", name)
                })
                .collect(),
        )
    }

    fn save_background_folders(&mut self, host: &mut dyn Host) {
        host.storage_write(STORAGE_KEY, self.bg_sync.folders.to_json().as_bytes());
    }

    /// Applies a finished scan and starts the next one when it is due.
    pub(crate) fn poll_background_folders(&mut self, host: &mut dyn Host) {
        if let Some(out) = self.bg_sync.take() {
            if self.bg_sync.running_epoch == self.bg_sync.epoch {
                self.apply_background_scan(host, out);
            } else {
                // The folders changed meanwhile: what the user added is
                // checked again, the rest is found by the next scan.
                let added = std::mem::take(&mut self.bg_sync.running_added);
                let mut queued = std::mem::take(&mut self.bg_sync.queued);
                self.bg_sync.queued = added;
                self.bg_sync.queued.append(&mut queued);
                self.bg_sync.queued_show |= self.bg_sync.running_show;
                self.bg_sync.again = true;
            }
        }
        if self.bg_sync.running() {
            return;
        }
        let asked = !self.bg_sync.queued.is_empty()
            || self.bg_sync.queued_folder.is_some()
            || self.bg_sync.again;
        let due = self.visible
            && !self.bg_sync.folders.list.is_empty()
            && self
                .bg_sync
                .last
                .is_none_or(|t| self.time - t >= RESCAN_SECS);
        if !asked && !due {
            return;
        }
        let job = Job {
            folders: self.bg_sync.folders.list.clone(),
            listed: self
                .bg_queue
                .items
                .iter()
                .filter_map(|s| s.strip_prefix(LOCAL_PREFIX).map(str::to_string))
                .collect(),
            added: self.bg_sync.queued.clone(),
        };
        let s = &mut self.bg_sync;
        s.running_added = std::mem::take(&mut s.queued);
        s.running_show = std::mem::take(&mut s.queued_show);
        s.running_folder = s.queued_folder.take();
        s.running_epoch = s.epoch;
        s.again = false;
        s.start(job, self.time);
    }

    fn apply_background_scan(&mut self, host: &mut dyn Host, out: Outcome) {
        let added = std::mem::take(&mut self.bg_sync.running_added);
        let show = std::mem::take(&mut self.bg_sync.running_show);
        let folder = self.bg_sync.running_folder.take();

        let mut seen_changed = false;
        for (path, seen) in &out.folders {
            let Some(seen) = seen else { continue };
            if let Some(f) = self
                .bg_sync
                .folders
                .list
                .iter_mut()
                .find(|f| f.path == *path)
            {
                if f.seen != *seen {
                    f.seen = seen.clone();
                    seen_changed = true;
                }
            }
        }
        if seen_changed {
            self.save_background_folders(host);
        }

        if !out.gone.is_empty() {
            let gone: HashSet<String> = out
                .gone
                .iter()
                .map(|p| format!("{LOCAL_PREFIX}{p}"))
                .collect();
            self.remove_backgrounds(host, &gone);
        }

        // The image to show: the first one the user added, or the listed
        // image it equals.
        let shown = if show {
            added.first().map(|p| {
                let same = out.duplicates.iter().find(|(a, _)| a == p).map(|(_, d)| d);
                format!("{LOCAL_PREFIX}{}", same.unwrap_or(p))
            })
        } else {
            None
        };
        let new: Vec<String> = out
            .add
            .iter()
            .map(|p| format!("{LOCAL_PREFIX}{p}"))
            .collect();
        self.push_backgrounds(host, new, shown);

        if !out.changed.is_empty() {
            let current = self.bg_source.clone().unwrap_or_default();
            if out
                .changed
                .iter()
                .any(|p| current.strip_prefix(LOCAL_PREFIX) == Some(p.as_str()))
            {
                self.reload_background(host);
            }
            self.revs.backgrounds += 1;
        }

        if let Some(folder) = folder {
            let n = out.add.iter().filter(|p| within(p, &folder)).count();
            self.toast(format!("{n} {}", self.tr("images added")), false);
        }
        let dupes = out.duplicates.len();
        if dupes == 1 && added.len() == 1 {
            self.toast(self.tr("This image is already in the list"), false);
        } else if dupes > 0 {
            self.toast(
                format!("{dupes} {}", self.tr("images were already in the list")),
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(name: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("bl-bgf-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
        fn file(&self, name: &str, data: &[u8]) -> String {
            let p = self.0.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, data).unwrap();
            p.to_string_lossy().into_owned()
        }
        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn apply(f: &mut Folder, out: &Outcome) {
        if let Some((_, Some(seen))) = out.folders.iter().find(|(p, _)| *p == f.path) {
            f.seen = seen.clone();
        }
    }

    #[test]
    fn new_gone_and_changed_files() {
        let d = Dir::new("sync");
        let a = d.file("a.png", b"aaaa");
        let b = d.file("sub/b.jpg", b"bbbbb");
        d.file("notes.txt", b"not an image");
        let mut folder = Folder {
            path: d.path(),
            ..Default::default()
        };
        // First scan: everything is new.
        let out = scan(Job {
            folders: vec![folder.clone()],
            ..Default::default()
        });
        assert_eq!(out.add, vec![a.clone(), b.clone()]);
        apply(&mut folder, &out);
        let mut listed = out.add.clone();

        // Nothing changed: nothing to do.
        let out = scan(Job {
            folders: vec![folder.clone()],
            listed: listed.clone(),
            ..Default::default()
        });
        assert!(out.is_empty(), "{out:?}");

        // A new file, a deleted one, a changed one.
        let c = d.file("c.webp", b"cccccc");
        std::fs::remove_file(&b).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&a, b"aaaa2").unwrap();
        let out = scan(Job {
            folders: vec![folder.clone()],
            listed: listed.clone(),
            ..Default::default()
        });
        assert_eq!(out.add, vec![c.clone()]);
        assert_eq!(out.gone, vec![b.clone()]);
        assert_eq!(out.changed, vec![a.clone()]);
        apply(&mut folder, &out);
        listed.retain(|p| *p != b);
        listed.push(c.clone());

        // Removed from the list by the user: stays out.
        listed.retain(|p| *p != c);
        let out = scan(Job {
            folders: vec![folder.clone()],
            listed: listed.clone(),
            ..Default::default()
        });
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn an_unreadable_folder_keeps_its_images() {
        let d = Dir::new("gone");
        let a = d.file("a.png", b"aaaa");
        let folder = Folder {
            path: d.path(),
            seen: [(a.clone(), Stamp::of(&a).unwrap())].into(),
        };
        drop(d);
        let out = scan(Job {
            folders: vec![folder],
            listed: vec![a],
            ..Default::default()
        });
        assert!(out.gone.is_empty());
        assert_eq!(out.folders.len(), 1);
        assert!(out.folders[0].1.is_none());
    }

    #[test]
    fn duplicates_by_content() {
        let d = Dir::new("dupes");
        let one = d.file("one.png", b"same bytes");
        let copy = d.file("copy of one.png", b"same bytes");
        let other = d.file("other.png", b"diff bytes");
        let single = d.file("elsewhere/single.jpg", b"single image");
        let single_copy = d.file("folder/single again.jpg", b"single image");

        // Added by the user: the copy and the listed path are duplicates.
        let out = scan(Job {
            listed: vec![one.clone()],
            added: vec![copy.clone(), other.clone(), one.clone(), other.clone()],
            ..Default::default()
        });
        assert_eq!(out.add, vec![other.clone()]);
        assert_eq!(
            out.duplicates,
            vec![
                (copy.clone(), one.clone()),
                (one.clone(), one.clone()),
                (other.clone(), other.clone())
            ]
        );

        // A folder holding a copy of a single image already listed.
        let out = scan(Job {
            folders: vec![Folder {
                path: d.0.join("folder").to_string_lossy().into_owned(),
                ..Default::default()
            }],
            listed: vec![single.clone()],
            ..Default::default()
        });
        assert!(out.add.is_empty(), "{out:?}");
        // Seen anyway, so it is not compared again.
        assert!(out.folders[0]
            .1
            .as_ref()
            .unwrap()
            .contains_key(&single_copy));
    }

    #[test]
    fn large_files_compare_in_chunks() {
        let d = Dir::new("large");
        let mut data = vec![7u8; 3 << 20];
        let a = d.file("a.bmp", &data);
        let b = d.file("b.bmp", &data);
        *data.last_mut().unwrap() = 8;
        let c = d.file("c.bmp", &data);
        assert!(same_content(&a, &b));
        assert!(!same_content(&a, &c));
    }

    /// The whole way through the core: folder, duplicates, changes,
    /// removal, a restart.
    #[test]
    fn the_list_follows_a_folder() {
        use crate::artists::tests::TestHost;
        use crate::core::{Core, LaunchArgs};
        fn cmd(core: &mut Core, host: &mut TestHost, json: Value) -> Value {
            core.command(host, &json)
        }
        fn settle(core: &mut Core, host: &mut TestHost) {
            for _ in 0..1000 {
                core.tick(host);
                let s = &core.bg_sync;
                if !s.running() && s.queued.is_empty() && s.queued_folder.is_none() && !s.again {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the scan did not finish");
        }
        fn listed(core: &mut Core, host: &mut TestHost) -> Vec<String> {
            let v = cmd(core, host, Value::object().with("cmd", "backgrounds.list"));
            v.as_array()
                .unwrap()
                .iter()
                .map(|i| i.get("source").as_str().unwrap().to_string())
                .collect()
        }
        let file = |p: &str| format!("{LOCAL_PREFIX}{p}");

        let d = Dir::new("core");
        let a = d.file("pics/a.png", b"image a");
        let b = d.file("pics/b.png", b"image b");
        let copy = d.file("copy of a.png", b"image a");
        let pics = d.0.join("pics").to_string_lossy().into_owned();
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "en");
        settle(&mut core, &mut host);

        cmd(
            &mut core,
            &mut host,
            Value::object()
                .with("cmd", "backgrounds.addFolder")
                .with("path", pics.as_str()),
        );
        settle(&mut core, &mut host);
        assert_eq!(listed(&mut core, &mut host), vec![file(&a), file(&b)]);

        // A copy under another name: not added, the original is shown.
        cmd(
            &mut core,
            &mut host,
            Value::object()
                .with("cmd", "backgrounds.add")
                .with("sources", vec![copy.as_str()])
                .with("show", true),
        );
        settle(&mut core, &mut host);
        assert_eq!(listed(&mut core, &mut host), vec![file(&a), file(&b)]);
        assert_eq!(core.bg_queue.current(), Some(&file(&a)));

        // New and deleted files, found after a restart too.
        let c = d.file("pics/new/c.png", b"image c");
        std::fs::remove_file(&b).unwrap();
        core.save_settings(&mut host);
        let mut core = Core::new(&mut host, LaunchArgs::default(), "en");
        settle(&mut core, &mut host);
        assert_eq!(listed(&mut core, &mut host), vec![file(&a), file(&c)]);

        // Taken off the list: stays off.
        cmd(
            &mut core,
            &mut host,
            Value::object()
                .with("cmd", "backgrounds.remove")
                .with("sources", vec![file(&a)]),
        );
        cmd(
            &mut core,
            &mut host,
            Value::object().with("cmd", "backgrounds.sync"),
        );
        settle(&mut core, &mut host);
        assert_eq!(listed(&mut core, &mut host), vec![file(&c)]);
        assert!(std::path::Path::new(&a).exists(), "the file stays");

        // The folder removed: its images leave the list, the files stay.
        cmd(
            &mut core,
            &mut host,
            Value::object()
                .with("cmd", "backgrounds.removeFolder")
                .with("path", pics.as_str()),
        );
        assert!(listed(&mut core, &mut host).is_empty());
        assert!(std::path::Path::new(&c).exists());
        let saved = Folders::parse(&host.storage_read(STORAGE_KEY).unwrap());
        assert!(saved.list.is_empty());
    }

    #[test]
    fn folders_nest_and_persist() {
        let mut f = Folders::default();
        f.add("/m/pics/");
        f.add("/m/pics/holiday");
        assert_eq!(f.paths(), vec!["/m/pics"]);
        f.add("/m/other");
        f.list[1].seen.insert(
            "/m/other/a.png".into(),
            Stamp {
                size: 3,
                modified: 1_700_000_000_123_456_789,
            },
        );
        f.add("/m");
        assert_eq!(f.paths(), vec!["/m"]);
        f.add("/n");
        f.list[1].seen.insert(
            "/n/a.png".into(),
            Stamp {
                size: 3,
                modified: 1_700_000_000_123_456_789,
            },
        );
        assert_eq!(Folders::parse(f.to_json().as_bytes()), f);
        assert!(f.holding("/n/x/y.png").is_some());
        assert!(f.holding("/nn/y.png").is_none());
        assert!(f.remove("/n/"));
        assert!(!f.remove("/n"));
        assert!(within("C:\\pics\\a.png", "C:\\pics"));
        assert!(!within("/pics2/a.png", "/pics"));
    }
}
