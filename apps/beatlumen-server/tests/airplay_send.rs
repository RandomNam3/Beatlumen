//! Playing on an AirPlay speaker: the app core streams what it plays to a
//! receiver (the one of bl-airplay) instead of the sound card.

mod common;

use bl_airplay::{Config, Event, Receiver};
use bl_json::Value;
use bl_media::Decoder;
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

#[test]
fn plays_on_an_airplay_speaker() {
    let rx = Receiver::start(Config {
        name: "Speaker".into(),
        id: [6, 5, 4, 3, 2, 1],
        password: None,
        airplay2: false,
    })
    .unwrap();
    let mut app = App::new("airplay-send");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("tone.wav");
    std::fs::write(&song, tone(30)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    // The app's volume becomes the speaker's volume.
    app.cmd(Value::object().with("cmd", "volume").with("value", 1.0));
    let id = format!("airplay:127.0.0.1:{}", rx.port());
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
        s.get("outputs").str_or("active", ""),
        id,
        "{}",
        s.get("outputs").to_json()
    );

    // The receiver gets the music; the sound card gets silence.
    let device = app
        .host
        .audio
        .lock()
        .unwrap()
        .clone()
        .expect("audio device");
    let mut decoder = None;
    let mut decoded = Vec::new();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(6) {
        app.state();
        let mut buf = vec![1f32; 960];
        device.render(&mut buf, 0);
        assert!(buf.iter().all(|v| *v == 0.0), "the device stays silent");
        if decoder.is_none() && rx.stream().is_active() {
            decoder = Some(rx.stream().decoder());
        }
        if let Some(d) = decoder.as_mut() {
            let mut out = Vec::new();
            let _ = d.next_chunk(&mut out);
            decoded.extend(out);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let loud: Vec<f32> = decoded
        .chunks(2)
        .map(|c| c[0])
        .filter(|v| v.abs() > 0.01)
        .collect();
    assert!(loud.len() > 44100 * 2, "{} loud frames", loud.len());
    let peak = loud.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.25, "peak {peak}");
    let events = rx.poll();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Volume(v) if *v > 0.9)),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Track(t) if t.title == "tone")),
        "{events:?}"
    );
    // The position accounts for the speaker's delay (about 2 s).
    let pos = app.core.position(&app.host);
    assert!(pos > 1.0 && pos < 5.5, "position {pos}");

    // Back to this device: the stream ends, the device plays again.
    app.cmd(
        Value::object()
            .with("cmd", "outputs.select")
            .with("id", "local"),
    );
    assert_eq!(app.state().get("outputs").str_or("status", ""), "local");
    let t0 = Instant::now();
    loop {
        app.state();
        let mut buf = vec![0f32; 960];
        device.render(&mut buf, 0);
        if buf.iter().any(|v| v.abs() > 0.01) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "device plays again");
        std::thread::sleep(Duration::from_millis(5));
    }
    let t0 = Instant::now();
    while !rx.poll().iter().any(|e| matches!(e, Event::Ended)) {
        assert!(t0.elapsed() < Duration::from_secs(5), "stream ended");
        std::thread::sleep(Duration::from_millis(20));
    }
}
