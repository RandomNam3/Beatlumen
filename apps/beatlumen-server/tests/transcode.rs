//! Smaller streams: the server converts a song to Ogg Opus while sending
//! it (from a start position for seeking), and an app on mobile data plays
//! server songs that way when its setting asks for it.

mod common;

use bl_json::Value;
use bl_net::client;
use common::App;
use std::time::{Duration, Instant};

fn get(url: &str) -> client::Response {
    client::request("GET", url, &[], &[], &client::Options::default()).expect("request")
}

/// Decoded length in seconds.
fn seconds(ogg: Vec<u8>) -> f64 {
    let mut d = bl_media::open(Box::new(std::io::Cursor::new(ogg)), None).expect("decodes");
    assert_eq!(d.codec_name(), "opus");
    let rate = d.spec().sample_rate as f64;
    let channels = d.spec().channels as usize;
    let mut out = Vec::new();
    let mut frames = 0usize;
    loop {
        out.clear();
        if !d.next_chunk(&mut out).unwrap() {
            break;
        }
        frames += out.len() / channels;
    }
    frames as f64 / rate
}

/// The playback position, pulling audio like a sound card would.
fn position(app: &mut App) -> f64 {
    app.state();
    if let Some(src) = app.host.audio.lock().unwrap().clone() {
        let mut buf = vec![0f32; 1920];
        src.render(&mut buf, 0);
    }
    app.core.position(&app.host)
}

#[test]
fn songs_are_converted_while_sent() {
    let root = std::env::temp_dir().join(format!("bl-transcode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    let wav = common::wav(12);
    std::fs::write(music.join("Sine Song.wav"), &wav).unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music.clone()],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let base = format!("http://{}", server.addr);
    let lib = bl_json::parse_bytes(&get(&format!("{base}/api/library")).body).unwrap();
    let id = lib.get("tracks").as_array().unwrap()[0]
        .str_or("id", "")
        .to_string();

    // The whole song, much smaller than the original.
    let r = get(&format!(
        "{base}/api/tracks/{id}/stream?format=opus&bitrate=64"
    ));
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("content-type"), Some("audio/ogg"));
    assert!(r.body.starts_with(b"OggS"));
    assert!(r.body.len() < wav.len() / 8, "{} bytes", r.body.len());
    let len = seconds(r.body);
    assert!((len - 12.0).abs() < 0.05, "{len} s");

    // From a start position.
    let r = get(&format!(
        "{base}/api/tracks/{id}/stream?format=opus&bitrate=64&start=8000"
    ));
    assert_eq!(r.status, 200);
    let len = seconds(r.body);
    assert!((len - 4.0).abs() < 0.05, "{len} s");

    // An app on mobile data: plays the converted stream and seeks in it.
    let mut app = App::new("transcode");
    app.connect(&base);
    common::wait_tracks(&mut app, 1);
    for (key, value) in [("streamKbps", 0), ("streamKbpsMetered", 96)] {
        app.cmd(
            Value::object()
                .with("cmd", "settings.set")
                .with("key", key)
                .with("value", value),
        );
    }
    app.cmd(
        Value::object()
            .with("cmd", "net.metered")
            .with("metered", true),
    );
    assert!(app.state().bool_or("metered", false));
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    let s = app.wait("playing", |s| s.str_or("play", "") == "playing");
    // The length comes from the library (the stream has none).
    assert!(
        (s.f64_or("duration", 0.0) - 12.0).abs() < 0.1,
        "{}",
        s.to_json()
    );
    app.cmd(Value::object().with("cmd", "seek").with("position", 9.0));
    let t0 = Instant::now();
    // Plays on from there (not just the asked position shown).
    while position(&mut app) < 9.5 {
        assert!(t0.elapsed() < Duration::from_secs(20), "seek");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(position(&mut app) < 11.0);
    let _ = std::fs::remove_dir_all(&root);
}
