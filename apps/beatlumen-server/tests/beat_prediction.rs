//! Beat prediction while playing: the tempo of a kick pattern is found
//! from the audio as it plays, and tapping along measures the sync offset.

mod common;

use bl_json::Value;
use common::App;

/// A stereo 44.1 kHz WAV with a kick drum at `bpm`.
fn kicks(bpm: f64, seconds: u32) -> Vec<u8> {
    let rate = 44100u32;
    let n = rate * seconds;
    let period = 60.0 / bpm;
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
        let t = i as f64 / rate as f64;
        let since = t % period;
        let v = (std::f64::consts::TAU * 55.0 * t).sin() * (-since * 25.0).exp() * 0.8;
        let s = (v * 32767.0) as i16;
        d.extend_from_slice(&s.to_le_bytes());
        d.extend_from_slice(&s.to_le_bytes());
    }
    d
}

/// Plays on like a sound card, analysing as it goes; the position after.
/// Paced so the decoder keeps up (gaps of silence would shift the heard
/// beats against the position, as on a slow test machine).
fn play_for(app: &mut App, seconds: f64) -> f64 {
    let start = app.core.position(&app.host);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(2));
        app.state();
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            let mut buf = vec![0f32; 1024];
            src.render(&mut buf, 0);
        }
        app.core.update_live(&mut app.host);
        let p = app.core.position(&app.host);
        if p - start >= seconds {
            return p;
        }
    }
}

#[test]
fn tempo_and_tap_sync() {
    let mut app = App::new("beats");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("kicks.wav");
    std::fs::write(&song, kicks(126.0, 30)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    // The decoder fills its buffer first.
    std::thread::sleep(std::time::Duration::from_millis(300));
    play_for(&mut app, 8.0);
    let s = app.state();
    let bpm = s.get("tempo").get("bpm").as_f64().expect("a tempo");
    assert!((bpm - 126.0).abs() < 3.0, "{bpm}");

    // Tapping 120 ms after each beat (as with a slow speaker).
    let period = 60.0 / 126.0;
    for _ in 0..8 {
        let pos = app.core.position(&app.host);
        let k = (pos / period).ceil() + 1.0;
        let target = k * period + 0.12;
        play_for(&mut app, target - pos);
        let r = app.cmd(Value::object().with("cmd", "sync.tap"));
        assert!(r.get("error").is_null(), "{r:?}");
    }
    let s = app.state();
    assert_eq!(
        s.get("underruns").as_u64(),
        Some(0),
        "the decoder fell behind"
    );
    let measured = s.get("tempo").get("measuredMs").as_f64().unwrap();
    assert!((measured - 120.0).abs() < 40.0, "{measured}");
    let r = app.cmd(Value::object().with("cmd", "sync.apply"));
    let ms = r.get("syncOffsetMs").as_f64().unwrap();
    assert!((ms + 120.0).abs() < 40.0, "{ms}");
    assert_eq!(app.state().get("settings").f64_or("syncOffsetMs", 0.0), ms);
}
