//! Playing on a DLNA renderer. Needs a renderer in the network that writes
//! what it plays to a file (gmrender-resurrect with a filesink), so it is
//! not run by default:
//!
//!   BL_DLNA_RENDERER=http://…/description.xml BL_DLNA_OUT=/path/out.raw \
//!     cargo test -p beatlumen-server --test dlna_send -- --ignored

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn tone(seconds: u32) -> Vec<u8> {
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
    for i in 0..n {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16;
        d.extend_from_slice(&s.to_le_bytes());
        d.extend_from_slice(&s.to_le_bytes());
    }
    d
}

fn peak_since(path: &str, offset: u64) -> (u64, i16) {
    let data = std::fs::read(path).unwrap_or_default();
    let new = data.get(offset as usize..).unwrap_or(&[]);
    let peak = new
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]).saturating_abs())
        .max()
        .unwrap_or(0);
    (data.len() as u64, peak)
}

#[test]
#[ignore]
fn plays_on_a_dlna_renderer() {
    let renderer = std::env::var("BL_DLNA_RENDERER").expect("BL_DLNA_RENDERER");
    let out = std::env::var("BL_DLNA_OUT").expect("BL_DLNA_OUT");
    let mut app = App::new("dlna-send");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("tone.wav");
    std::fs::write(&song, tone(60)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "volume").with("value", 1.0));
    let start = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let id = format!("dlna:{renderer}");
    app.cmd(
        Value::object()
            .with("cmd", "outputs.select")
            .with("id", id.as_str()),
    );
    let s = app.wait("connected", |s| {
        s.get("outputs").str_or("status", "") == "playing"
            || s.get("outputs").get("error").as_str().is_some()
    });
    assert_eq!(
        s.get("outputs").str_or("status", ""),
        "playing",
        "{}",
        s.get("outputs").to_json()
    );

    // The renderer plays the tone, in real time.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    // The renderer may start a new file for the new stream.
    let start = if std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < start {
        0
    } else {
        start
    };
    let (len, peak) = peak_since(&out, start);
    let secs = (len - start) as f64 / 44100.0 / 4.0;
    assert!(peak > 10000, "peak {peak}");
    assert!((4.0..9.0).contains(&secs), "{secs} s played in 8 s");
    // The visuals follow the renderer's delay.
    let pos = app.core.position(&app.host);
    let played_by_app = t0.elapsed().as_secs_f64();
    eprintln!("position {pos:.2}, renderer played {secs:.2} s");
    assert!(
        (pos - secs).abs() < 1.0,
        "visuals follow the renderer: {pos:.2} vs {secs:.2}"
    );

    // Pause: the renderer stops; play: it continues.
    app.cmd(Value::object().with("cmd", "pause"));
    std::thread::sleep(Duration::from_millis(1500));
    for _ in 0..10 {
        app.state();
    }
    let (mark, _) = peak_since(&out, 0);
    std::thread::sleep(Duration::from_secs(3));
    let (after, _) = peak_since(&out, 0);
    assert!(
        after - mark < 44100 * 4 / 2,
        "paused: {} bytes in 3 s",
        after - mark
    );
    app.cmd(Value::object().with("cmd", "play"));
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(5) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, peak) = peak_since(&out, after);
    assert!(peak > 10000, "playing again: peak {peak}");
    let _ = played_by_app;

    app.cmd(
        Value::object()
            .with("cmd", "outputs.select")
            .with("id", "local"),
    );
    assert_eq!(app.state().get("outputs").str_or("status", ""), "local");
}
