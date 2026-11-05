//! Favorites and the listening history between app and server: favorites of
//! server songs reach other devices of the login, also after being offline;
//! the favorites work as a playlist; plays only reach the server with the
//! user's agreement and never for songs outside the server's library.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn server(root: &std::path::Path, music: &std::path::Path) -> (String, beatlumen_server::Server) {
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music.to_path_buf()],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    (format!("http://{}", server.addr), server)
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect()
}

fn until(app: &mut App, what: &str, mut f: impl FnMut(&mut App) -> bool) {
    let t0 = Instant::now();
    loop {
        app.state();
        if f(app) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn set(app: &mut App, id: &str, on: bool) {
    app.cmd(
        Value::object()
            .with("cmd", "favorites.set")
            .with("ids", vec![id.to_string()])
            .with("value", on),
    );
}

fn favorites(app: &mut App) -> Vec<String> {
    app.cmd(Value::object().with("cmd", "favorites.ids"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.as_str().map(str::to_string))
        .collect()
}

#[test]
fn favorites_follow_the_login() {
    let root = std::env::temp_dir().join(format!("bl-fav-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("a.wav"), common::wav(1)).unwrap();
    std::fs::write(music.join("b.wav"), common::wav(1)).unwrap();
    let (url, srv) = server(&root, &music);

    let mut one = App::new("fav-one");
    let mut two = App::new("fav-two");
    one.connect(&url);
    two.connect(&url);
    let tracks = common::wait_tracks(&mut one, 2);
    common::wait_tracks(&mut two, 2);
    let a = tracks[0].str_or("id", "").to_string();
    let b = tracks[1].str_or("id", "").to_string();

    // A local file stays on the device; a server song reaches the server.
    let local = "file:/somewhere/private.mp3";
    set(&mut one, local, true);
    set(&mut one, &a, true);
    until(&mut one, "favorite on the server", |_| {
        srv.state.favorites.ids().iter().any(|f| f.id == a)
    });
    assert_eq!(srv.state.favorites.ids().len(), 1, "only the server song");

    // The other device sees it the next time it looks.
    let mut two = App::with_dir(two.close());
    two.connect(&url);
    until(&mut two, "favorite on the other device", |app| {
        favorites(app).contains(&a)
    });

    // Works as a playlist, and adding to the playlist marks the song.
    let lists = one.cmd(Value::object().with("cmd", "playlists.list"));
    let fav = lists
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.bool_or("fixed", false))
        .expect("favorites playlist")
        .clone();
    assert_eq!(fav.str_or("id", ""), "favorites");
    assert_eq!(fav.get("count").as_u64(), Some(2));
    one.cmd(
        Value::object()
            .with("cmd", "playlist.add")
            .with("id", "favorites")
            .with("items", vec![b.clone()]),
    );
    assert!(favorites(&mut one).contains(&b));
    // A fixed list cannot be deleted.
    one.cmd(
        Value::object()
            .with("cmd", "playlist.delete")
            .with("id", "favorites"),
    );
    assert!(favorites(&mut one).contains(&b));
    until(&mut one, "second favorite on the server", |_| {
        srv.state.favorites.ids().len() == 2
    });

    // Marks made while the server is away are sent later.
    one.cmd(Value::object().with("cmd", "server.disconnect"));
    one.state();
    set(&mut one, &a, false);
    one.state();
    assert_eq!(srv.state.favorites.ids().len(), 2);
    let mut one = App::with_dir(one.close());
    one.connect(&url);
    until(&mut one, "offline change on the server", |_| {
        let f = srv.state.favorites.ids();
        !f.iter().any(|x| x.id == a) && f.iter().any(|x| x.id == b)
    });
    assert!(!favorites(&mut one).contains(&a));

    // The song row knows.
    common::wait_tracks(&mut one, 2);
    let row = one.cmd(
        Value::object()
            .with("cmd", "library.tracks")
            .with("ids", vec![b.clone()]),
    );
    assert!(row.at(0).bool_or("favorite", false));
    assert!(
        favorites(&mut one).iter().any(|x| x == local),
        "local mark kept"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Plays `id` for `secs` of real time, then stops.
fn play_for(app: &mut App, id: &str, secs: f64) {
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.to_string()])
            .with("start", 0u64),
    );
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs_f64(secs) {
        app.state();
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            let mut buf = vec![0f32; 1920];
            src.render(&mut buf, 0);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    app.cmd(Value::object().with("cmd", "stop"));
    app.state();
}

#[test]
fn plays_reach_the_server_only_after_agreeing() {
    let root = std::env::temp_dir().join(format!("bl-hist-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("a.wav"), common::wav(4)).unwrap();
    std::fs::write(music.join("b.wav"), common::wav(4)).unwrap();
    let (url, srv) = server(&root, &music);

    let mut app = App::new("hist");
    app.connect(&url);
    let tracks = common::wait_tracks(&mut app, 2);
    let a = tracks[0].str_or("id", "").to_string();
    let b = tracks[1].str_or("id", "").to_string();

    // Not agreed: recorded on the device only.
    play_for(&mut app, &a, 2.6);
    let recent = app.cmd(
        Value::object()
            .with("cmd", "history.recent")
            .with("limit", 10u64),
    );
    assert_eq!(ids(&recent), [a.clone()], "half of a 4 s song counts");
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(300) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(srv.state.history.events().is_empty());

    // Agreed: from now on, not what was before.
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "historySync")
            .with("value", true),
    );
    play_for(&mut app, &b, 2.6);
    until(&mut app, "play on the server", |_| {
        srv.state.history.events().iter().any(|e| e.id == b)
    });
    let ev = srv.state.history.events();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert!(!ev[0].skip);
    assert!(ev[0].ms >= 1000);

    // Both show in the device's statistics.
    let st = app.cmd(
        Value::object()
            .with("cmd", "stats.overview")
            .with("range", "week")
            .with("utcOffset", 0u64),
    );
    assert_eq!(st.get("plays").as_u64(), Some(2));
    assert_eq!(st.get("streak").as_u64(), Some(1));
    assert_eq!(st.get("topSongs").as_array().unwrap().len(), 2);
    let lib = app.cmd(Value::object().with("cmd", "stats.library"));
    assert_eq!(lib.get("tracks").as_u64(), Some(2));

    // A song left early counts as skipped.
    let mut app = App::with_dir(app.close());
    app.connect(&url);
    common::wait_tracks(&mut app, 2);
    let home = app.cmd(Value::object().with("cmd", "home.overview"));
    assert_eq!(
        ids(home.get("recent")),
        [b.clone(), a.clone()],
        "kept over a restart, newest first"
    );
    play_for(&mut app, &a, 1.3);
    until(&mut app, "skip on the server", |_| {
        srv.state.history.events().len() == 2
    });
    assert!(srv
        .state
        .history
        .events()
        .iter()
        .any(|e| e.skip && e.id == a));

    // Deleting clears the device and the server.
    app.cmd(
        Value::object()
            .with("cmd", "history.clear")
            .with("server", true),
    );
    until(&mut app, "history deleted", |_| {
        srv.state.history.events().is_empty()
    });
    assert!(ids(&app.cmd(
        Value::object()
            .with("cmd", "history.recent")
            .with("limit", 10u64)
    ))
    .is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plays_of_local_files_stay_on_the_device() {
    let root = std::env::temp_dir().join(format!("bl-hist-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("server.wav"), common::wav(1)).unwrap();
    let (url, srv) = server(&root, &music);

    let mut app = App::new("hist-local");
    let mine = app.dir.join("mine");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::write(mine.join("private.wav"), common::wav(4)).unwrap();
    app.connect(&url);
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "historySync")
            .with("value", true),
    );
    app.cmd(
        Value::object()
            .with("cmd", "library.addFolder")
            .with("path", mine.to_string_lossy().to_string()),
    );
    let tracks = common::wait_tracks(&mut app, 2);
    let local = tracks
        .iter()
        .find(|t| t.bool_or("local", false))
        .expect("local song")
        .str_or("id", "")
        .to_string();
    play_for(&mut app, &local, 2.6);
    let recent = app.cmd(
        Value::object()
            .with("cmd", "history.recent")
            .with("limit", 10u64),
    );
    assert_eq!(ids(&recent), [local]);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(500) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(srv.state.history.events().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn start_page_lists_the_newest_albums_and_songs() {
    let mut app = App::new("home");
    let music = app.dir.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("loose.wav"), common::wav(1)).unwrap();
    app.state();
    app.cmd(
        Value::object()
            .with("cmd", "library.addFolder")
            .with("path", music.to_string_lossy().to_string()),
    );
    let t = common::wait_tracks(&mut app, 1);
    let home = app.cmd(Value::object().with("cmd", "home.overview"));
    let new = home.get("new").as_array().unwrap();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].str_or("type", ""), "track");
    assert_eq!(new[0].str_or("id", ""), t[0].str_or("id", ""));
    assert!(home.get("recent").as_array().unwrap().is_empty());
    assert!(home.get("favorites").as_array().unwrap().is_empty());
}
