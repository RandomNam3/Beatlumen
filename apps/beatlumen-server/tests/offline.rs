//! Offline copies: an app keeps a server song (with its song text) in a
//! container file; after a restart without the server the song shows up
//! in the library and plays with its text.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn tracks(app: &mut App) -> Vec<Value> {
    app.cmd(Value::object().with("cmd", "library.tracks"))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn songs_play_offline_with_their_text() {
    let root = std::env::temp_dir().join(format!("bl-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("Sine Song.wav"), common::wav(4)).unwrap();
    std::fs::write(music.join("Sine Song.lrc"), "[00:00.50]Offline line\n").unwrap();
    std::fs::write(music.join("Other.wav"), common::wav(3)).unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music.clone()],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let url = format!("http://{}", server.addr);

    let mut app = App::new("offline");
    app.connect(&url);
    let t0 = Instant::now();
    let list = loop {
        let l = tracks(&mut app);
        if l.len() == 2 || t0.elapsed() > Duration::from_secs(20) {
            break l;
        }
        app.state();
        std::thread::sleep(Duration::from_millis(50));
    };
    let song = list
        .iter()
        .find(|t| t.str_or("title", "").contains("Sine"))
        .expect("song in the library");
    let id = song.str_or("id", "").to_string();
    assert!(!song.bool_or("offline", true));

    let r = app.cmd(
        Value::object()
            .with("cmd", "offline.save")
            .with("ids", vec![id.clone()]),
    );
    assert_eq!(r.get("started").as_u64(), Some(1), "{}", r.to_json());
    let s = app.wait("saved", |s| {
        s.get("offline").get("count").as_u64() == Some(1)
            && s.get("offline")
                .get("saving")
                .as_array()
                .is_some_and(|a| a.is_empty())
    });
    assert!(s.get("offline").get("bytes").as_u64().unwrap() > 600_000);
    assert!(tracks(&mut app)
        .iter()
        .any(|t| t.str_or("id", "") == id && t.bool_or("offline", false)));
    // The container on disk.
    let dir = app.dir.join("offline");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "blo"))
        .expect("container written");
    let h = bl_offline::read_header(&file).unwrap();
    assert_eq!(h.id(), id);
    assert_eq!(
        h.lyrics.as_ref().map(|l| l.0.as_str()),
        Some("[00:00.50]Offline line")
    );
    assert!(bl_offline::verify(&file).unwrap());
    // Saving again does nothing.
    let r = app.cmd(
        Value::object()
            .with("cmd", "offline.save")
            .with("ids", vec![id.clone()]),
    );
    assert_eq!(r.get("started").as_u64(), Some(0));
    // The overview counts it under its album.
    let o = app.cmd(Value::object().with("cmd", "offline.overview"));
    assert_eq!(o.at(0).get("songs").as_u64(), Some(1), "{}", o.to_json());
    // A limit below what another song needs: not saved.
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "offlineLimitMb")
            .with("value", 1u64),
    );
    let other = tracks(&mut app)
        .iter()
        .find(|t| t.str_or("title", "").contains("Other"))
        .map(|t| t.str_or("id", "").to_string())
        .unwrap();
    let r = app.cmd(
        Value::object()
            .with("cmd", "offline.save")
            .with("ids", vec![other]),
    );
    assert_eq!(r.get("skipped").as_u64(), Some(1), "{}", r.to_json());

    // The server goes away; the app starts again on its data.
    drop(server);
    let mut app = App::with_dir(app.close());
    let t0 = Instant::now();
    let list = loop {
        app.state();
        let l = tracks(&mut app);
        if !l.is_empty() || t0.elapsed() > Duration::from_secs(20) {
            break l;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let st = app.state();
    assert_eq!(
        list.len(),
        1,
        "only the offline song: {list:?} offline {} server {} url {}",
        st.get("offline").to_json(),
        st.str_or("server", ""),
        st.get("settings").str_or("serverUrl", "")
    );
    assert_eq!(list[0].str_or("id", ""), id);
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let src = app
        .host
        .audio
        .lock()
        .unwrap()
        .clone()
        .expect("audio started");
    let mut loud = 0.0f32;
    let t0 = Instant::now();
    while loud < 0.05 && t0.elapsed() < Duration::from_secs(20) {
        app.state();
        let mut buf = vec![0f32; 4800];
        src.render(&mut buf, 0);
        loud = loud.max(buf.iter().fold(0.0f32, |m, x| m.max(x.abs())));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loud > 0.05, "offline audio is heard");
    let s = app.wait("analysis", |s| {
        matches!(s.str_or("analysis", ""), "ready" | "failed")
    });
    assert_eq!(s.str_or("analysis", ""), "ready");
    let t0 = Instant::now();
    let lyrics = loop {
        app.state();
        let l = app.cmd(Value::object().with("cmd", "lyrics.get"));
        if l.str_or("status", "") != "loading" || t0.elapsed() > Duration::from_secs(10) {
            break l;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(lyrics.str_or("status", ""), "ready", "{}", lyrics.to_json());
    assert_eq!(lyrics.get("lines").at(0).str_or("text", ""), "Offline line");

    // Removing the copy deletes the file.
    app.cmd(
        Value::object()
            .with("cmd", "offline.remove")
            .with("ids", vec![id.clone()]),
    );
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(&root);
}
