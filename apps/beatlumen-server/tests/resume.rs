//! The app continues the track it played when it was closed, at the same
//! position (Android's media controls offer that after the app is gone).

mod common;

use bl_core::{Core, LaunchArgs};
use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn wav(seconds: u32) -> Vec<u8> {
    let rate = 44100u32;
    let n = rate * seconds;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + n * 4).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&rate.to_le_bytes());
    d.extend_from_slice(&(rate * 4).to_le_bytes());
    d.extend_from_slice(&4u16.to_le_bytes());
    d.extend_from_slice(&16u16.to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(n * 4).to_le_bytes());
    d.resize(d.len() + (n * 4) as usize, 0);
    d
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
fn continues_where_it_stopped() {
    let mut app = App::new("resume");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("song.wav");
    std::fs::write(&song, wav(60)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "seek").with("position", 23.0));
    let t0 = Instant::now();
    while (position(&mut app) - 23.0).abs() > 1.0 {
        assert!(t0.elapsed() < Duration::from_secs(20), "seek");
        std::thread::sleep(Duration::from_millis(20));
    }
    app.core.save_settings(&mut app.host);

    // A new start: nothing plays until asked, then the same track continues.
    app.core = Core::new(&mut app.host, LaunchArgs::default(), "en");
    let s = app.state();
    assert_ne!(s.str_or("play", ""), "playing");
    assert!(
        s.get("current").get("id").as_str().is_some(),
        "{}",
        s.to_json()
    );
    app.cmd(Value::object().with("cmd", "play"));
    app.wait("playing again", |s| s.str_or("play", "") == "playing");
    let t0 = Instant::now();
    loop {
        let p = position(&mut app);
        if (p - 23.0).abs() < 2.0 {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "position {p}");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Another track chosen by the user starts at the beginning.
    app.core.save_settings(&mut app.host);
    app.core = Core::new(&mut app.host, LaunchArgs::default(), "en");
    app.state();
    app.cmd(Value::object().with("cmd", "queue.play").with("pos", 0));
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let p = position(&mut app);
    assert!(p < 5.0, "position {p}");
}
