//! Other apps' playback (Android): the platform records it and reports
//! the other app's media session; the app visualises the audio without
//! playing it again and sends its transport controls to the other app.

mod common;

use bl_json::Value;
use common::App;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Plays the audio device for `d`, returns the loudest sample it got.
fn tick(app: &mut App, d: Duration) -> f32 {
    let t0 = Instant::now();
    let mut peak = 0f32;
    while t0.elapsed() < d {
        app.state();
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            let mut buf = vec![0f32; 960];
            src.render(&mut buf, 0);
            peak = buf.iter().fold(peak, |p, v| p.max(v.abs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    peak
}

fn cmd(app: &mut App, name: &str) -> Value {
    app.cmd(Value::object().with("cmd", name))
}

fn requests(app: &App) -> Vec<(String, String)> {
    std::mem::take(&mut *app.host.capture.lock().unwrap())
}

#[test]
fn other_apps_playback_is_visualised_and_controlled() {
    let mut app = App::new("capture");
    assert!(!app.state().get("capture").bool_or("available", true));
    assert!(cmd(&mut app, "capture.start")
        .get("error")
        .as_str()
        .is_some());

    app.host.android = true;
    assert!(app.state().get("capture").bool_or("available", false));
    cmd(&mut app, "capture.start");
    assert_eq!(
        requests(&app),
        vec![("start".to_string(), "{}".to_string())]
    );
    assert!(app.state().get("capture").bool_or("requested", false));
    // Audio before the platform reports recording is not used.
    bl_core::capture::push_pcm(&[1000; 960], 2, 48000);

    // Recording runs: a 440 Hz tone, 10 ms at a time, in real time.
    // Access to media sessions is reported while the user still decides.
    app.cmd(
        Value::object()
            .with("cmd", "capture.access")
            .with("granted", true),
    );
    assert!(app.state().get("capture").bool_or("access", false));
    assert!(app.state().get("capture").bool_or("requested", false));
    app.cmd(
        Value::object()
            .with("cmd", "capture.state")
            .with("running", true),
    );
    let s = app.state();
    assert!(s.get("capture").bool_or("active", false), "{}", s.to_json());
    assert!(s
        .get("current")
        .str_or("id", "")
        .starts_with(bl_core::capture::ID_PREFIX));
    assert_eq!(s.get("current").str_or("title", ""), "Other apps");
    let running = Arc::new(AtomicBool::new(true));
    let recorder = {
        let running = running.clone();
        std::thread::spawn(move || {
            let mut n = 0u64;
            let t0 = Instant::now();
            while running.load(Ordering::Relaxed) {
                let mut pcm = Vec::with_capacity(960);
                for _ in 0..480 {
                    let v = ((n as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 16000.0)
                        as i16;
                    pcm.push(v);
                    pcm.push(v);
                    n += 1;
                }
                bl_core::capture::push_pcm(&pcm, 2, 48000);
                let due = Duration::from_micros(n * 1_000_000 / 48000);
                if let Some(w) = due.checked_sub(t0.elapsed()) {
                    std::thread::sleep(w);
                }
            }
        })
    };

    // The device gets silence (the other app is heard already), but the
    // playback runs with the recorded audio.
    let peak = tick(&mut app, Duration::from_millis(1500));
    assert_eq!(peak, 0.0, "the captured audio must not be played again");
    let s = app.state();
    assert_eq!(s.str_or("play", ""), "playing");
    let pos = app.core.position(&app.host);
    assert!(pos > 0.8, "position {pos}");
    // Recorded audio reached the player: the visualiser's input is not
    // silent (samples played since the start).
    let mut recent = vec![0f32; 4800];
    app.core.recent_samples(&mut recent);
    let level = recent.iter().fold(0f32, |p, v| p.max(v.abs()));
    assert!(level > 0.3, "visualised audio level {level}");

    // The other app's media session: title, artist, cover, position.
    let cover = app.dir.join("cover.png");
    std::fs::create_dir_all(&app.dir).unwrap();
    std::fs::write(&cover, b"\x89PNG\r\n\x1a\n").unwrap();
    app.cmd(
        Value::object().with("cmd", "capture.session").with(
            "session",
            Value::object()
                .with("app", "Musik-App")
                .with("title", "Fremdes Lied")
                .with("artist", "Fremde Band")
                .with("album", "Album")
                .with("cover", cover.to_string_lossy().as_ref())
                .with("duration", 200.0)
                .with("position", 50.0)
                .with("playing", true)
                .with("canSeek", true),
        ),
    );
    let s = app.state();
    let cur = s.get("current");
    assert_eq!(cur.str_or("title", ""), "Fremdes Lied");
    assert_eq!(cur.str_or("artist", ""), "Fremde Band");
    assert!(
        cur.str_or("cover", "").contains("cover.png"),
        "{}",
        cur.to_json()
    );
    assert_eq!(s.f64_or("duration", 0.0), 200.0);
    assert_eq!(s.get("capture").str_or("app", ""), "Musik-App");
    tick(&mut app, Duration::from_millis(300));
    let pos = app.core.position(&app.host);
    assert!((50.2..51.0).contains(&pos), "session position {pos}");

    // The transport controls go to the other app.
    requests(&app);
    cmd(&mut app, "toggle");
    cmd(&mut app, "next");
    cmd(&mut app, "prev");
    app.cmd(Value::object().with("cmd", "seek").with("position", 120.0));
    let sent: Vec<String> = requests(&app)
        .into_iter()
        .map(|(k, j)| {
            assert_eq!(k, "control");
            let v = bl_json::parse(&j).unwrap();
            format!("{}@{}", v.str_or("action", ""), v.f64_or("position", -1.0))
        })
        .collect();
    assert_eq!(sent, ["pause@0", "next@0", "prev@0", "seek@120"]);
    // The playback here is not paused by that, the other app pauses.
    app.cmd(
        Value::object().with("cmd", "capture.session").with(
            "session",
            Value::object()
                .with("app", "Musik-App")
                .with("title", "Fremdes Lied")
                .with("duration", 200.0)
                .with("position", 120.0)
                .with("playing", false),
        ),
    );
    let s = app.state();
    assert_eq!(s.str_or("play", ""), "paused");
    assert!(s.get("current").str_or("cover", "").is_empty());
    cmd(&mut app, "toggle");
    assert_eq!(requests(&app)[0].1.contains("\"play\""), true);

    // Playing a track of the app's own ends capture.
    requests(&app);
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![Value::from("local:/nonexistent.mp3")]),
    );
    assert_eq!(requests(&app), vec![("stop".to_string(), "{}".to_string())]);
    let s = app.state();
    assert!(!s.get("capture").bool_or("active", true));
    assert!(!s
        .get("current")
        .str_or("id", "")
        .starts_with(bl_core::capture::ID_PREFIX));

    // Started again; the platform ends it (the user stopped it there).
    cmd(&mut app, "capture.start");
    app.cmd(
        Value::object()
            .with("cmd", "capture.state")
            .with("running", true),
    );
    assert!(app.state().get("capture").bool_or("active", false));
    app.cmd(
        Value::object()
            .with("cmd", "capture.state")
            .with("running", false),
    );
    let s = app.state();
    assert!(!s.get("capture").bool_or("active", true));
    assert!(s.get("current").is_null(), "{}", s.get("current").to_json());
    // The device plays normally again.
    running.store(false, Ordering::Relaxed);
    recorder.join().unwrap();
}
