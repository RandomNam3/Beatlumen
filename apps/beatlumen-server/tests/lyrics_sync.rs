//! Timing song texts with speech recognition on the server. Needs a
//! Whisper model and a speech sample (16 kHz WAV):
//! `BL_SPEECH_MODEL=ggml-tiny.bin BL_SPEECH_SAMPLE=jfk.wav cargo test
//! --release --test lyrics_sync -- --ignored`.

mod common;

use beatlumen_server::Config;
use bl_json::Value;
use bl_net::client;
use common::App;
use std::time::{Duration, Instant};

fn lyrics(app: &mut App) -> Value {
    app.state();
    app.cmd(Value::object().with("cmd", "lyrics.get"))
}

/// Without a model the app offers no timing.
#[test]
fn no_timing_without_a_model() {
    let root = std::env::temp_dir().join(format!("bl-lyrics-nosync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("Song.wav"), common::wav(1)).unwrap();
    std::fs::write(music.join("Song.txt"), "Plain line").unwrap();
    let server = beatlumen_server::start(Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let base = format!("http://{}", server.addr);
    let mut app = App::new("nosync");
    app.connect(&base);
    let id = common::wait_tracks(&mut app, 1)[0]
        .str_or("id", "")
        .to_string();
    let r = client::request(
        "POST",
        &format!("{base}/api/tracks/{id}/lyrics/sync"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(r.status, 501);
    app.cmd(Value::object().with("cmd", "play").with("ids", vec![id]));
    let t0 = Instant::now();
    let l = loop {
        let l = lyrics(&mut app);
        if l.str_or("status", "") == "ready" || t0.elapsed() > Duration::from_secs(10) {
            break l;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(l.str_or("status", ""), "ready", "{}", l.to_json());
    assert!(!l.bool_or("canSync", true));
    assert!(app
        .cmd(Value::object().with("cmd", "lyrics.sync"))
        .get("error")
        .as_str()
        .is_some());
    drop(app);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore]
fn plain_text_gets_timed_and_speech_gets_transcribed() {
    let (Ok(model), Ok(sample)) = (
        std::env::var("BL_SPEECH_MODEL"),
        std::env::var("BL_SPEECH_SAMPLE"),
    ) else {
        eprintln!("BL_SPEECH_MODEL / BL_SPEECH_SAMPLE not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("bl-lyrics-sync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::copy(&sample, music.join("Speech.wav")).unwrap();
    std::fs::copy(&sample, music.join("Other.wav")).unwrap();
    std::fs::write(
        music.join("Speech.txt"),
        "And so, my fellow Americans,\nask not what your country can do for you,\n\nask what you can do for your country.",
    )
    .unwrap();
    let server = beatlumen_server::start(Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        speech_model: Some(model.into()),
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let base = format!("http://{}", server.addr);
    let req = |m: &str, p: &str| {
        client::request(
            m,
            &format!("{base}{p}"),
            &[],
            &[],
            &client::Options::default(),
        )
        .unwrap()
    };
    let json = |m: &str, p: &str| bl_json::parse_bytes(&req(m, p).body).unwrap();
    assert!(json("GET", "/api/info").bool_or("speech", false));
    let lib = json("GET", "/api/library");
    let id_of = |title: &str| {
        lib.get("tracks")
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.str_or("title", "") == title)
            .unwrap()
            .str_or("id", "")
            .to_string()
    };
    let wait_done = |id: &str| {
        assert_eq!(
            req("POST", &format!("/api/tracks/{id}/lyrics/sync")).status,
            202
        );
        let t0 = Instant::now();
        loop {
            let s = json("GET", &format!("/api/tracks/{id}/lyrics/sync"));
            match s.str_or("state", "") {
                "done" => break,
                "failed" => panic!("failed: {}", s.to_json()),
                _ => assert!(t0.elapsed() < Duration::from_secs(300), "timeout"),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        json("GET", &format!("/api/tracks/{id}/lyrics"))
    };

    // A plain text: its lines get times.
    let l = wait_done(&id_of("Speech"));
    assert_eq!(l.str_or("source", ""), "synced");
    let text = l.str_or("text", "");
    eprintln!("{text}");
    // (After the tag naming the speech model; each word has its times.)
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with("[re:")).collect();
    assert_eq!(lines.len(), 3, "{text}");
    let words = |l: &str| {
        let mut out = String::new();
        let mut tag = false;
        for c in l[10..].chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    assert!(lines[0].contains("<00:0"), "{text}");
    assert!(
        lines[0].starts_with("[00:00.") && words(lines[0]) == "And so, my fellow Americans,",
        "{text}"
    );
    assert!(words(lines[1]).contains("ask not what your country can do for you,"));
    let t = |l: &str| l[1..3].parse::<f64>().unwrap() * 60.0 + l[4..9].parse::<f64>().unwrap();
    assert!(
        t(lines[0]) < t(lines[1]) && t(lines[1]) < t(lines[2]) && t(lines[2]) < 11.0,
        "{text}"
    );

    // No text: what is said becomes the text.
    let l = wait_done(&id_of("Other"));
    assert_eq!(l.str_or("source", ""), "recognized");
    let said: Vec<String> = l
        .str_or("text", "")
        .lines()
        .filter(|l| l.starts_with("[0"))
        .map(words)
        .collect();
    assert!(
        said.join(" ").to_lowercase().contains("fellow americans"),
        "{said:?}"
    );

    // An imported text replaces the computed timings.
    let id = id_of("Speech");
    let r = client::request(
        "PUT",
        &format!("{base}/api/tracks/{id}/lyrics"),
        &[],
        b"[00:01.00]Mine",
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(r.status, 204);
    assert_eq!(
        json("GET", &format!("/api/tracks/{id}/lyrics")).str_or("source", ""),
        "imported"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The app asks the server to time the playing song's text and shows the
/// timed text when it is done.
#[test]
#[ignore]
fn app_times_the_playing_song() {
    let (Ok(model), Ok(sample)) = (
        std::env::var("BL_SPEECH_MODEL"),
        std::env::var("BL_SPEECH_SAMPLE"),
    ) else {
        eprintln!("BL_SPEECH_MODEL / BL_SPEECH_SAMPLE not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("bl-lyrics-appsync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::copy(&sample, music.join("Speech.wav")).unwrap();
    std::fs::write(
        music.join("Speech.txt"),
        "And so, my fellow Americans,\nask not what your country can do for you,\nask what you can do for your country.",
    )
    .unwrap();
    let server = beatlumen_server::start(Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        speech_model: Some(model.into()),
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let mut app = App::new("appsync");
    app.connect(&format!("http://{}", server.addr));
    let id = common::wait_tracks(&mut app, 1)[0]
        .str_or("id", "")
        .to_string();
    app.cmd(Value::object().with("cmd", "play").with("ids", vec![id]));
    let t0 = Instant::now();
    let l = loop {
        let l = lyrics(&mut app);
        if l.str_or("status", "") == "ready" || t0.elapsed() > Duration::from_secs(10) {
            break l;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!l.bool_or("synced", true));
    assert!(l.bool_or("canSync", false), "{}", l.to_json());
    let r = app.cmd(Value::object().with("cmd", "lyrics.sync"));
    assert!(r.bool_or("ok", false), "{}", r.to_json());
    assert!(!lyrics(&mut app).get("sync").is_null());
    let t0 = Instant::now();
    let l = loop {
        let l = lyrics(&mut app);
        if l.bool_or("synced", false) {
            break l;
        }
        assert!(t0.elapsed() < Duration::from_secs(300), "{}", l.to_json());
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(l.str_or("source", ""), "synced");
    assert_eq!(l.get("lines").as_array().unwrap().len(), 3);
    assert!(!l.bool_or("canSync", true));
    assert!(l.get("sync").is_null());
    drop(app);
    let _ = std::fs::remove_dir_all(&root);
}
