//! Source extensions, protocol 2: what the manifest offers, rows of
//! suggestions, collections, radio, songs and collections side by side in the
//! search, songs waiting in the app's queue until their turn, and songs from
//! extensions going away when nobody plays them.

mod common;

use bl_json::Value;
use bl_net::client;
use common::fake_extension::{extension, media, Asked};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DAY: u64 = 24 * 3600;

fn get(base: &str, path: &str) -> client::Response {
    client::request(
        "GET",
        &format!("{base}{path}"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap()
}

fn json(r: &client::Response) -> Value {
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    bl_json::parse_bytes(&r.body).unwrap()
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bl-ext2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    dir
}

fn server(
    dir: &Path,
    ext: &str,
    tune: impl FnOnce(&mut beatlumen_server::Config),
) -> beatlumen_server::Server {
    let mut config = beatlumen_server::Config {
        data_dir: dir.join("data"),
        music_dirs: vec![dir.join("music")],
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        extensions: vec![("tube".into(), ext.to_string())],
        ..Default::default()
    };
    tune(&mut config);
    let s = beatlumen_server::start(config).unwrap();
    assert!(s.wait_for_scan(Duration::from_secs(10)));
    s
}

#[test]
fn the_manifest_says_what_is_offered() {
    let dir = folder("manifest");
    let asked = Arc::new(Asked::default());
    let ext = extension(2, None, asked);
    let s = server(&dir, &ext, |_| {});
    let list = json(&get(&format!("http://{}", s.addr), "/api/extensions"));
    let e = &list.get("extensions").as_array().unwrap()[0];
    assert_eq!(
        e.str_or("name", ""),
        "Tube",
        "the name comes from the manifest"
    );
    for what in ["search", "shelves", "collections", "radio"] {
        assert!(e.bool_or(what, false), "{what}: {}", list.to_json());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_extension_of_protocol_1_only_searches() {
    let dir = folder("v1");
    let asked = Arc::new(Asked::default());
    let ext = extension(1, None, asked.clone());
    let s = server(&dir, &ext, |_| {});
    let base = format!("http://{}", s.addr);
    let list = json(&get(&base, "/api/extensions"));
    let e = &list.get("extensions").as_array().unwrap()[0];
    assert!(e.bool_or("search", false));
    assert!(!e.bool_or("shelves", true));
    assert_eq!(get(&base, "/api/extensions/tube/shelves").status, 404);
    assert_eq!(
        get(&base, "/api/extensions/tube/collections/a:1").status,
        404
    );
    assert_eq!(get(&base, "/api/extensions/tube/radio/s:1").status, 404);
    assert_eq!(
        asked.shelves.load(Ordering::Relaxed),
        0,
        "it is not even asked"
    );
    // No collections from it either.
    let found = json(&get(&base, "/api/extensions/tube/search?q=x"));
    let items = found.get("items").as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", found.to_json());
    assert_eq!(items[0].str_or("kind", ""), "song");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_mixes_songs_and_collections_and_drops_the_rest() {
    let dir = folder("search");
    let asked = Arc::new(Asked::default());
    let ext = extension(2, None, asked.clone());
    let s = server(&dir, &ext, |_| {});
    let base = format!("http://{}", s.addr);
    let found = json(&get(&base, "/api/extensions/tube/search?q=x"));
    let items = found.get("items").as_array().unwrap();
    let kinds: Vec<&str> = items.iter().map(|i| i.str_or("kind", "")).collect();
    assert_eq!(kinds, ["song", "collection"], "{}", found.to_json());
    assert_eq!(
        items[0].str_or("image", ""),
        "/api/extensions/tube/image/s:1"
    );
    assert_eq!(
        items[1].str_or("image", ""),
        "/api/extensions/tube/image/a:1"
    );
    assert_eq!(items[1].get("count").as_u64(), Some(2));

    // The same search again stays here.
    json(&get(&base, "/api/extensions/tube/search?q=x"));
    assert_eq!(asked.search.load(Ordering::Relaxed), 1);
    json(&get(&base, "/api/extensions/tube/search?q=other"));
    assert_eq!(asked.search.load(Ordering::Relaxed), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shelves_arrive_clean_and_are_not_asked_for_every_time() {
    let dir = folder("shelves");
    let asked = Arc::new(Asked::default());
    let ext = extension(2, None, asked.clone());
    let s = server(&dir, &ext, |_| {});
    let base = format!("http://{}", s.addr);
    let v = json(&get(&base, "/api/extensions/tube/shelves"));
    let shelves = v.get("shelves").as_array().unwrap();
    assert_eq!(
        shelves.len(),
        1,
        "an empty row is left out: {}",
        v.to_json()
    );
    assert_eq!(
        shelves[0].str_or("title", ""),
        "TopSongs",
        "no control characters"
    );
    assert_eq!(shelves[0].get("items").as_array().unwrap().len(), 2);
    json(&get(&base, "/api/extensions/tube/shelves"));
    assert_eq!(asked.shelves.load(Ordering::Relaxed), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_collection_comes_in_pages_and_holds_only_songs() {
    let dir = folder("collection");
    let asked = Arc::new(Asked::default());
    let ext = extension(2, None, asked);
    let s = server(&dir, &ext, |_| {});
    let base = format!("http://{}", s.addr);
    let first = json(&get(&base, "/api/extensions/tube/collections/a:1"));
    assert_eq!(first.str_or("title", ""), "Record");
    assert_eq!(first.str_or("image", ""), "/api/extensions/tube/image/a:1");
    let items = first.get("items").as_array().unwrap();
    assert_eq!(items.len(), 2, "{}", first.to_json());
    let next = first.str_or("next", "");
    assert_eq!(next, "page/2+=");

    let second = json(&get(
        &base,
        "/api/extensions/tube/collections/a:1?after=page%2F2%2B%3D",
    ));
    assert_eq!(
        second.get("items").as_array().unwrap()[0].str_or("id", ""),
        "s:3"
    );
    assert!(second.get("next").is_null());

    assert_ne!(
        get(&base, "/api/extensions/tube/collections/..%2Fetc").status,
        200
    );
    assert_eq!(
        get(&base, "/api/extensions/tube/collections/a:1?after=a%0Ab").status,
        400,
        "a position with a line break"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_radio_is_asked_every_time() {
    let dir = folder("radio");
    let asked = Arc::new(Asked::default());
    let ext = extension(2, None, asked.clone());
    let s = server(&dir, &ext, |_| {});
    let base = format!("http://{}", s.addr);
    let v = json(&get(&base, "/api/extensions/tube/radio/s:1"));
    assert_eq!(v.get("items").as_array().unwrap().len(), 2);
    json(&get(&base, "/api/extensions/tube/radio/s:1"));
    assert_eq!(asked.radio.load(Ordering::Relaxed), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

fn post(base: &str, path: &str, body: &Value) -> client::Response {
    client::request(
        "POST",
        &format!("{base}{path}"),
        &[("Content-Type".into(), "application/json".into())],
        body.to_json().as_bytes(),
        &client::Options::default(),
    )
    .unwrap()
}

/// Imports a song into the library and returns its track id and file.
fn import(server: &beatlumen_server::Server, item: &str) -> (String, PathBuf) {
    let base = &format!("http://{}", server.addr);
    let job = json(&post(
        base,
        "/api/extensions/tube/import",
        &Value::object().with("id", item).with("keep", true),
    ));
    let id = job.str_or("job", "").to_string();
    let t0 = Instant::now();
    let track = loop {
        let s = json(&get(base, &format!("/api/extensions/jobs/{id}")));
        if let Some(t) = s.get("track").as_str() {
            break t.to_string();
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{}", s.to_json());
        std::thread::sleep(Duration::from_millis(50));
    };
    let idx = server.state.library();
    let entry = idx
        .entries
        .iter()
        .find(|e| e.track.id == track)
        .expect("in the library");
    let file = entry.abs.clone();
    (track, file)
}

fn in_library(base: &str, track: &str) -> bool {
    json(&get(base, "/api/library"))
        .get("tracks")
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t.str_or("id", "") == track)
}

/// An old song and a new one from the extension, next to a song of the
/// library's own that nobody played either.
struct Cleanup {
    dir: PathBuf,
    server: beatlumen_server::Server,
    base: String,
    own: PathBuf,
}

impl Cleanup {
    fn new(name: &str, tune: impl FnOnce(&mut beatlumen_server::Config)) -> Option<Cleanup> {
        let dir = folder(name);
        let (webm, _) = media(&dir)?;
        let own = dir.join("music").join("own.wav");
        std::fs::write(&own, common::wav(1)).unwrap();
        let ext = extension(2, Some(webm), Arc::new(Asked::default()));
        let server = server(&dir, &ext, tune);
        let base = format!("http://{}", server.addr);
        Some(Cleanup {
            dir,
            server,
            base,
            own,
        })
    }

    /// Runs the cleanup as if `days` had gone by.
    fn after(&self, days: u64) -> usize {
        let state = &self.server.state;
        let now = beatlumen_server::util::now_secs() + days * DAY;
        state.extensions.cleanup(state, now)
    }

    fn rescanned(&self, track: &str, wanted: bool) {
        let t0 = Instant::now();
        while in_library(&self.base, track) != wanted {
            assert!(t0.elapsed() < Duration::from_secs(20), "{track} {wanted}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn songs_nobody_played_go_and_the_librarys_own_never_do() {
    let Some(c) = Cleanup::new("cleanup", |_| {}) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let (track, file) = import(&c.server, "s:1");
    assert!(file.is_file());
    assert_eq!(c.after(89), 0, "not yet");
    assert!(file.is_file());
    assert_eq!(c.after(91), 1);
    assert!(!file.exists(), "the file is gone");
    c.rescanned(&track, false);
    assert!(c.own.is_file(), "the library's own song stays");

    // It can come back: a new import, a new file.
    let (again, file) = import(&c.server, "s:1");
    assert_eq!(again, track);
    assert!(file.is_file());
    assert_eq!(c.after(1), 0);
}

#[test]
fn playing_a_song_keeps_it() {
    let Some(c) = Cleanup::new("played", |_| {}) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let (track, file) = import(&c.server, "s:1");
    // Pretend the import is old, by rewriting what the server keeps.
    c.server.state.extensions.backdate_for_tests(100 * DAY);
    let stream = get(&c.base, &format!("/api/tracks/{track}/stream"));
    assert_eq!(stream.status, 200);
    assert_eq!(c.after(0), 0, "just played");
    assert!(file.is_file());
    assert_eq!(c.after(91), 1, "and then not for 91 days");
}

#[test]
fn a_seek_is_not_a_new_play() {
    let Some(c) = Cleanup::new("seek", |_| {}) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let (track, file) = import(&c.server, "s:1");
    c.server.state.extensions.backdate_for_tests(100 * DAY);
    let r = client::request(
        "GET",
        &format!("{}/api/tracks/{track}/stream", c.base),
        &[("Range".into(), "bytes=100-".into())],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(r.status, 206);
    assert_eq!(c.after(0), 1);
    assert!(!file.exists());
}

#[test]
fn songs_in_playlists_and_favorites_stay_unless_the_admin_says_otherwise() {
    let Some(c) = Cleanup::new("listed", |_| {}) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let (in_playlist, file_a) = import(&c.server, "s:1");
    let (favorite, file_b) = import(&c.server, "s:2");
    let (neither, file_c) = import(&c.server, "s:3");
    let made = post(
        &c.base,
        "/api/playlists",
        &Value::object()
            .with("name", "List")
            .with("kind", "track")
            .with("items", vec![in_playlist.as_str()]),
    );
    assert!(
        (200..300).contains(&made.status),
        "{}",
        String::from_utf8_lossy(&made.body)
    );
    let r = client::request(
        "PUT",
        &format!("{}/api/favorites/{favorite}", c.base),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert!(
        (200..300).contains(&r.status),
        "{}",
        String::from_utf8_lossy(&r.body)
    );

    assert_eq!(c.after(91), 1, "only the third");
    assert!(file_a.is_file(), "in a playlist");
    assert!(file_b.is_file(), "a favorite");
    assert!(!file_c.exists());
    c.rescanned(&neither, false);

    let state = &c.server.state;
    let mut cfg = (*state.config()).clone();
    cfg.extension_cleanup_keep_listed = false;
    state.apply_config(cfg, false);
    assert_eq!(c.after(91), 2);
    assert!(!file_a.exists());
    assert!(!file_b.exists());
}

#[test]
fn zero_days_means_never() {
    let Some(c) = Cleanup::new("never", |cfg| cfg.extension_cleanup_days = 0) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let (_, file) = import(&c.server, "s:1");
    assert_eq!(c.after(100_000), 0);
    assert!(file.is_file());
}

#[test]
fn the_old_list_of_imports_is_read_and_gets_the_full_time() {
    let dir = folder("old-list");
    let imports = dir
        .join("data")
        .join("extensions")
        .join("library")
        .join("tube");
    std::fs::create_dir_all(&imports).unwrap();
    let file = imports.join("a.wav");
    std::fs::write(&file, common::wav(1)).unwrap();
    std::fs::write(
        dir.join("data").join("extensions").join("imported.json"),
        r#"{"tube/s:1":"tube/a.wav"}"#,
    )
    .unwrap();
    let ext = extension(2, None, Arc::new(Asked::default()));
    let s = server(&dir, &ext, |_| {});
    let now = beatlumen_server::util::now_secs();
    assert_eq!(s.state.extensions.cleanup(&s.state, now + 89 * DAY), 0);
    assert!(file.is_file());
    assert_eq!(s.state.extensions.cleanup(&s.state, now + 91 * DAY), 1);
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_list_entry_pointing_out_of_the_folder_deletes_nothing() {
    let dir = folder("escape");
    let outside = dir.join("music").join("precious.wav");
    std::fs::write(&outside, common::wav(1)).unwrap();
    std::fs::create_dir_all(dir.join("data").join("extensions")).unwrap();
    std::fs::write(
        dir.join("data").join("extensions").join("imported.json"),
        r#"{"tube/s:1":{"path":"../../../music/precious.wav","added":1,"played":1}}"#,
    )
    .unwrap();
    let ext = extension(2, None, Arc::new(Asked::default()));
    let s = server(&dir, &ext, |_| {});
    let later = beatlumen_server::util::now_secs() + 1000 * DAY;
    assert_eq!(s.state.extensions.cleanup(&s.state, later), 0);
    assert!(outside.is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_link_among_the_imports_is_left_alone() {
    let dir = folder("link");
    let outside = dir.join("music").join("precious.wav");
    std::fs::write(&outside, common::wav(1)).unwrap();
    let imports = dir
        .join("data")
        .join("extensions")
        .join("library")
        .join("tube");
    std::fs::create_dir_all(&imports).unwrap();
    std::os::unix::fs::symlink(&outside, imports.join("a.wav")).unwrap();
    std::fs::write(
        dir.join("data").join("extensions").join("imported.json"),
        r#"{"tube/s:1":{"path":"tube/a.wav","added":1,"played":1}}"#,
    )
    .unwrap();
    let ext = extension(2, None, Arc::new(Asked::default()));
    let s = server(&dir, &ext, |_| {});
    let later = beatlumen_server::util::now_secs() + 1000 * DAY;
    assert_eq!(s.state.extensions.cleanup(&s.state, later), 0);
    assert!(outside.is_file());
    assert!(imports.join("a.wav").symlink_metadata().is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_admin_can_set_all_of_it() {
    let c = beatlumen_server::Config::default();
    assert_eq!(c.extension_cleanup_days, 90);
    assert!(c.extension_cleanup_keep_listed);
    let keys: Vec<&str> = beatlumen_server::config::ADMIN_SETTINGS
        .iter()
        .map(|s| s.key)
        .collect();
    assert!(keys.contains(&"extensionCleanupDays"));
    assert!(keys.contains(&"extensionCleanupKeepListed"));
    let mut cfg = c.clone();
    beatlumen_server::config::apply_json(
        &mut cfg,
        &bl_json::parse(r#"{"extensionCleanupDays":30,"extensionCleanupKeepListed":false}"#)
            .unwrap(),
        Path::new("."),
    )
    .unwrap();
    assert_eq!(cfg.extension_cleanup_days, 30);
    assert!(!cfg.extension_cleanup_keep_listed);
    assert!(beatlumen_server::config::apply_json(
        &mut cfg,
        &bl_json::parse(r#"{"extensionCleanupDays":"soon"}"#).unwrap(),
        Path::new("."),
    )
    .is_err());
}

// ---- in the app -------------------------------------------------------------

fn until(
    app: &mut common::App,
    what: &str,
    pred: impl Fn(&mut common::App, &Value) -> bool,
) -> Value {
    let t0 = Instant::now();
    loop {
        for _ in 0..4 {
            app.core.tick(&mut app.host);
            std::thread::sleep(Duration::from_millis(10));
        }
        let s = app.core.state(&app.host);
        if pred(app, &s) {
            return s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(40),
            "timeout waiting for {what}: {}",
            s.get("current").to_json()
        );
    }
}

fn queue(app: &mut common::App) -> Vec<String> {
    app.cmd(Value::object().with("cmd", "queue.list"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect()
}

/// An app connected to a server with the fake extension.
fn app_with_extension(
    name: &str,
    fail: &[&str],
) -> Option<(common::App, Arc<Asked>, PathBuf, beatlumen_server::Server)> {
    let dir = folder(name);
    let (webm, _) = media(&dir)?;
    let asked = Arc::new(Asked::default());
    for item in fail {
        asked.failing.lock().unwrap().push(item.to_string());
    }
    let ext = extension(2, Some(webm), asked.clone());
    let s = server(&dir, &ext, |_| {});
    let mut app = common::App::new(&format!("ext2-{name}"));
    app.connect(&format!("http://{}", s.addr));
    until(&mut app, "the extension", |_, s| {
        s.get("extensions")
            .get("list")
            .as_array()
            .is_some_and(|l| !l.is_empty())
    });
    Some((app, asked, dir, s))
}

fn search(app: &mut common::App) -> Vec<Value> {
    app.cmd(
        Value::object()
            .with("cmd", "extensions.search")
            .with("query", "anything"),
    );
    until(app, "the results", |_, s| {
        !s.get("extensions").bool_or("searching", true)
    });
    app.cmd(Value::object().with("cmd", "extensions.results"))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn play_ids(app: &mut common::App, ids: &[&str]) {
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", ids.to_vec())
            .with("start", 0u64),
    );
}

#[test]
fn songs_wait_in_the_queue_and_are_imported_when_their_turn_comes() {
    let Some((mut app, asked, dir, _server)) = app_with_extension("queue", &[]) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let results = search(&mut app);
    let kinds: Vec<&str> = results.iter().map(|r| r.str_or("kind", "")).collect();
    assert_eq!(kinds, ["song", "collection"], "side by side");
    let first = results[0].str_or("songId", "").to_string();
    assert!(first.starts_with("extitem:tube:"), "{first}");
    assert!(
        results[1].get("songId").is_null(),
        "a collection is not a song"
    );

    // Its menu works on the id like on any other song.
    let meta = app.cmd(
        Value::object()
            .with("cmd", "library.tracks")
            .with("ids", vec![first.as_str()]),
    );
    let meta = &meta.as_array().unwrap()[0];
    assert_eq!(meta.str_or("title", ""), "First");
    assert_eq!(meta.str_or("artist", ""), "Band");
    assert!(
        meta.str_or("cover", "").contains("image"),
        "{}",
        meta.to_json()
    );

    // Added to the queue: nothing is fetched yet.
    let second = "extitem:tube:s:2";
    app.cmd(
        Value::object()
            .with("cmd", "extensions.collection")
            .with("ext", "tube")
            .with("id", "a:1"),
    );
    let data = until(&mut app, "the collection", |app, _| {
        let d = app.cmd(Value::object().with("cmd", "extensions.collectionData"));
        !d.bool_or("loading", true)
    });
    let _ = data;
    let d = app.cmd(Value::object().with("cmd", "extensions.collectionData"));
    assert_eq!(d.str_or("title", ""), "Record");
    let items = d.get("items").as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i.str_or("id", "")).collect();
    assert_eq!(
        ids,
        ["s:1", "s:2", "s:3"],
        "all pages, songs only: {}",
        d.to_json()
    );
    assert_eq!(
        asked.audio.load(Ordering::Relaxed),
        0,
        "nothing imported yet"
    );

    // Playing the list asks first where songs go, and nothing starts meanwhile.
    play_ids(&mut app, &[&first, second, "extitem:tube:s:3"]);
    let s = until(&mut app, "the question", |_, s| {
        s.get("extensions").bool_or("asking", false)
    });
    assert_eq!(s.get("current").str_or("title", ""), "First");
    assert_eq!(asked.audio.load(Ordering::Relaxed), 0);
    app.cmd(
        Value::object()
            .with("cmd", "extensions.target")
            .with("target", "library"),
    );
    let s = until(&mut app, "the first song", |_, s| {
        s.str_or("play", "") == "playing"
    });
    assert_eq!(s.get("extensions").str_or("target", ""), "library");
    assert!(!s.get("extensions").bool_or("asking", true));
    assert_eq!(s.get("current").str_or("title", ""), "First");

    // Only the one playing was imported; the others still wait.
    assert_eq!(asked.audio.load(Ordering::Relaxed), 1);
    let q = queue(&mut app);
    assert_eq!(q.len(), 3, "{q:?}");
    assert!(!q[0].starts_with("extitem:"), "{q:?}");
    assert_eq!(q[1], second);

    // Next: its turn.
    app.cmd(Value::object().with("cmd", "next"));
    until(&mut app, "the second song", |_, s| {
        s.get("current").str_or("title", "") == "Second" && s.str_or("play", "") == "playing"
    });
    assert_eq!(asked.audio.load(Ordering::Relaxed), 2);
    assert_eq!(queue(&mut app).len(), 3);

    // Once in the library, the track takes over in the queue.
    until(&mut app, "the library tracks", |app, _| {
        queue(app).iter().take(2).all(|i| !i.contains(':'))
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_song_that_cannot_be_fetched_leaves_the_queue_and_the_next_one_plays() {
    let Some((mut app, _asked, dir, _server)) = app_with_extension("fail", &["s:1"]) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    search(&mut app);
    app.cmd(
        Value::object()
            .with("cmd", "extensions.collection")
            .with("ext", "tube")
            .with("id", "a:1"),
    );
    until(&mut app, "the collection", |app, _| {
        !app.cmd(Value::object().with("cmd", "extensions.collectionData"))
            .bool_or("loading", true)
    });
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "extensionTarget")
            .with("value", "library"),
    );
    play_ids(&mut app, &["extitem:tube:s:1", "extitem:tube:s:2"]);
    until(&mut app, "the second song", |_, s| {
        s.get("current").str_or("title", "") == "Second" && s.str_or("play", "") == "playing"
    });
    assert_eq!(queue(&mut app).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_of_suggestions_reach_the_start_page() {
    let Some((mut app, asked, dir, _server)) = app_with_extension("rows", &[]) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    app.cmd(Value::object().with("cmd", "extensions.shelves"));
    until(&mut app, "the rows", |app, _| {
        app.cmd(Value::object().with("cmd", "extensions.shelves"))
            .as_array()
            .is_some_and(|a| !a.is_empty())
    });
    let shelves = app.cmd(Value::object().with("cmd", "extensions.shelves"));
    let shelf = &shelves.as_array().unwrap()[0];
    assert_eq!(
        shelf.str_or("extName", ""),
        "Tube",
        "the name from the manifest"
    );
    assert_eq!(shelf.str_or("title", ""), "TopSongs");
    let items = shelf.get("items").as_array().unwrap();
    assert_eq!(items[0].str_or("songId", ""), "extitem:tube:s:1");
    assert_eq!(items[1].str_or("kind", ""), "collection");
    // Asked again and again by the page: one request.
    for _ in 0..5 {
        app.cmd(Value::object().with("cmd", "extensions.shelves"));
    }
    assert_eq!(asked.shelves.load(Ordering::Relaxed), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_radio_of_an_extension_goes_on_by_itself() {
    let Some((mut app, asked, dir, _server)) = app_with_extension("radio-app", &[]) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let results = search(&mut app);
    let first = results[0].str_or("songId", "").to_string();
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "extensionTarget")
            .with("value", "library"),
    );
    app.cmd(
        Value::object()
            .with("cmd", "songs.radio")
            .with("seed", Value::object().with("tracks", vec![first.as_str()]))
            .with("name", ""),
    );
    let s = until(&mut app, "the radio", |_, s| {
        s.str_or("play", "") == "playing"
    });
    assert!(
        s.get("radio").bool_or("active", false),
        "{}",
        s.get("radio").to_json()
    );
    assert_eq!(
        s.get("radio").str_or("name", ""),
        "First",
        "named after the song"
    );
    assert_eq!(s.get("current").str_or("title", ""), "Second");
    let q = queue(&mut app);
    assert_eq!(q[1], "extitem:tube:s:3", "{q:?}");
    assert_eq!(
        asked.audio.load(Ordering::Relaxed),
        1,
        "only the first of them"
    );

    // Without a radio on offer: says so.
    app.cmd(Value::object().with("cmd", "songs.radioStop"));
    let _ = std::fs::remove_dir_all(&dir);
}
