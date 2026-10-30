//! Internet radio in the app: a station is saved, plays live from a fake
//! Icecast server (MP3 with ICY titles) and shows the current song.
//! Needs ffmpeg for the MP3 (skipped without it).

mod common;

use bl_json::Value;
use common::App;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

/// An MP3 tone made with ffmpeg.
fn mp3(dir: &std::path::Path) -> Option<Vec<u8>> {
    let out = dir.join("tone.mp3");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=20",
        ])
        .args(["-ac", "2", "-ar", "44100", "-b:a", "128k"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&out).ok()).flatten()
}

/// An Icecast-like server: the MP3 in a loop with a title every 8 KiB.
fn station(audio: Vec<u8>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/live", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let audio = audio.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"ICY 200 OK\r\nContent-Type: audio/mpeg\r\nicy-name: Testfunk\r\nicy-metaint: 8192\r\n\r\n");
                let meta = b"StreamTitle='Die Band - Das Lied';";
                let mut block = vec![meta.len().div_ceil(16) as u8];
                block.extend_from_slice(meta);
                block.resize(1 + 16 * block[0] as usize, 0);
                let mut at = 0;
                loop {
                    let chunk: Vec<u8> = (0..8192).map(|i| audio[(at + i) % audio.len()]).collect();
                    at += 8192;
                    if s.write_all(&chunk).is_err() || s.write_all(&block).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
        }
    });
    url
}

fn position(app: &mut App) -> f64 {
    app.state();
    if let Some(src) = app.host.audio.lock().unwrap().clone() {
        let mut buf = vec![0f32; 4800];
        src.render(&mut buf, 0);
    }
    app.core.position(&app.host)
}

#[test]
fn radio_station_plays_live() {
    let mut app = App::new("radio");
    std::fs::create_dir_all(&app.dir).unwrap();
    let Some(audio) = mp3(&app.dir) else {
        eprintln!("ffmpeg missing: skipped");
        return;
    };
    let url = station(audio);
    app.state();
    let r = app.cmd(
        Value::object()
            .with("cmd", "radio.add")
            .with("name", "Testfunk")
            .with("url", url.as_str()),
    );
    let id = r.str_or("id", "").to_string();
    assert!(id.starts_with("radio:"), "{r:?}");
    let s = app.state();
    assert_eq!(
        s.get("sources").get("stations").at(0).str_or("name", ""),
        "Testfunk"
    );

    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let t0 = Instant::now();
    while position(&mut app) < 1.0 {
        assert!(t0.elapsed() < Duration::from_secs(20), "no audio");
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = app.wait("title", |s| {
        s.get("current").str_or("title", "") == "Das Lied"
    });
    assert_eq!(s.get("current").str_or("artist", ""), "Die Band");

    app.cmd(
        Value::object()
            .with("cmd", "radio.remove")
            .with("url", url.as_str()),
    );
    let s = app.state();
    assert_eq!(
        s.get("sources").get("stations").as_array().map(Vec::len),
        Some(0)
    );
}
