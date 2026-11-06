//! Radio, endless playback, mixes and labels in the app: from the server when
//! connected, worked out on the device without one.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

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

fn queue(app: &mut App) -> Vec<String> {
    app.cmd(Value::object().with("cmd", "queue.list"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect()
}

fn library_folder(app: &mut App, songs: usize) -> Vec<Value> {
    let music = app.dir.join("music");
    std::fs::create_dir_all(&music).unwrap();
    for i in 0..songs {
        std::fs::write(music.join(format!("song {i}.wav")), common::wav(1)).unwrap();
    }
    app.state();
    app.cmd(
        Value::object()
            .with("cmd", "library.addFolder")
            .with("path", music.to_string_lossy().to_string()),
    );
    common::wait_tracks(app, songs)
}

#[test]
fn without_a_server_the_device_decides() {
    let mut app = App::new("radio-local");
    let tracks = library_folder(&mut app, 8);
    let first = tracks[0].str_or("id", "").to_string();

    // A radio from a song: plays right away, the seed itself is left for later.
    app.cmd(
        Value::object()
            .with("cmd", "songs.radio")
            .with("seed", Value::object().with("tracks", vec![first.clone()]))
            .with("name", "Test"),
    );
    let s = app.state();
    assert!(s.get("radio").bool_or("active", false));
    assert_eq!(s.get("radio").str_or("name", ""), "Test");
    let q = queue(&mut app);
    assert_eq!(q.len(), 8, "{q:?}");
    let unique: std::collections::HashSet<_> = q.iter().collect();
    assert_eq!(unique.len(), 8);

    // Choosing songs ends the radio.
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![first.clone()])
            .with("start", 0u64),
    );
    assert!(!app.state().get("radio").bool_or("active", true));
    assert_eq!(queue(&mut app), [first.clone()]);

    // Endless playback: the queue fills up when it runs low.
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "endless")
            .with("value", true),
    );
    until(&mut app, "queue extended", |a| queue(a).len() > 1);
    let q = queue(&mut app);
    assert_eq!(q[0], first);
    let settings = app.cmd(Value::object().with("cmd", "settings.get"));
    assert!(settings.bool_or("endless", false));

    // Something to discover without any history.
    let home = app.cmd(Value::object().with("cmd", "home.overview"));
    assert_eq!(home.get("shelves").at(0).str_or("key", ""), "discover");
    assert!(!home
        .get("shelves")
        .at(0)
        .get("items")
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn labels_given_on_the_device() {
    let mut app = App::new("labels-local");
    let tracks = library_folder(&mut app, 2);
    let id = tracks[0].str_or("id", "").to_string();
    app.cmd(
        Value::object()
            .with("cmd", "labels.change")
            .with("id", id.clone())
            .with("add", vec!["Sport".to_string(), "a:b".to_string()]),
    );
    let of = app.cmd(
        Value::object()
            .with("cmd", "labels.of")
            .with("id", id.clone()),
    );
    let given: Vec<&str> = of
        .get("given")
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(given.len(), 2);
    assert!(
        given.contains(&"Sport") && given.contains(&"a b"),
        "{given:?}"
    );
    let list = app.cmd(Value::object().with("cmd", "labels.list"));
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.str_or("name", "") == "Sport" && l.bool_or("given", false)));
    let songs = app.cmd(
        Value::object()
            .with("cmd", "labels.tracks")
            .with("name", "Sport"),
    );
    assert_eq!(songs.at(0).str_or("id", ""), id);

    // Kept over a restart.
    let dir = app.close();
    let mut app = App::with_dir(dir);
    common::wait_tracks(&mut app, 2);
    let of = app.cmd(
        Value::object()
            .with("cmd", "labels.of")
            .with("id", id.clone()),
    );
    assert_eq!(of.get("given").as_array().unwrap().len(), 2);
}

#[test]
fn with_a_server_it_decides() {
    let root = std::env::temp_dir().join(format!("bl-app-radio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    for i in 0..6 {
        std::fs::write(music.join(format!("server song {i}.wav")), common::wav(1)).unwrap();
    }
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        auto_labels: false,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let url = format!("http://{}", server.addr);

    let mut app = App::new("radio-server");
    app.connect(&url);
    let tracks = common::wait_tracks(&mut app, 6);
    let first = tracks[0].str_or("id", "").to_string();

    // The server's suggestions reach the start page.
    until(&mut app, "suggestions", |a| {
        let home = a.cmd(Value::object().with("cmd", "home.overview"));
        home.get("shelves").at(0).str_or("key", "") == "discover"
    });

    // A label given here reaches the server, and the radio follows it.
    app.cmd(
        Value::object()
            .with("cmd", "labels.change")
            .with("id", first.clone())
            .with("add", vec!["Morgens".to_string()]),
    );
    until(&mut app, "label on the server", |_| {
        server.state.labels.of(&first) == ["Morgens"]
    });
    app.cmd(
        Value::object()
            .with("cmd", "songs.radio")
            .with("seed", Value::object().with("label", "Morgens"))
            .with("name", "Morgens"),
    );
    until(&mut app, "radio plays", |a| !queue(a).is_empty());
    assert!(queue(&mut app).contains(&first));

    // A radio that runs low asks for more.
    let before = queue(&mut app).len();
    app.cmd(Value::object().with("cmd", "next"));
    let mut grew = before >= 4;
    let t0 = Instant::now();
    while !grew && t0.elapsed() < Duration::from_secs(10) {
        app.state();
        grew = queue(&mut app).len() > before;
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(grew, "queue stayed at {before}");
    let _ = std::fs::remove_dir_all(&root);
}
