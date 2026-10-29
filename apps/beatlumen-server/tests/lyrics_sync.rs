//! Timing song texts with speech recognition on the server. Needs a
//! Whisper model and a speech sample (16 kHz WAV):
//! `BL_SPEECH_MODEL=ggml-tiny.bin BL_SPEECH_SAMPLE=jfk.wav cargo test
//! --release --test lyrics_sync -- --ignored`.

use beatlumen_server::Config;
use bl_net::client;
use std::time::{Duration, Instant};

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
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].starts_with("[00:00.") && lines[0].ends_with("And so, my fellow Americans,"),
        "{text}"
    );
    assert!(lines[1].contains("ask not what your country can do for you,"));
    let t = |l: &str| l[1..3].parse::<f64>().unwrap() * 60.0 + l[4..9].parse::<f64>().unwrap();
    assert!(
        t(lines[0]) < t(lines[1]) && t(lines[1]) < t(lines[2]) && t(lines[2]) < 11.0,
        "{text}"
    );

    // No text: what is said becomes the text.
    let l = wait_done(&id_of("Other"));
    assert_eq!(l.str_or("source", ""), "recognized");
    assert!(l
        .str_or("text", "")
        .to_lowercase()
        .contains("fellow americans"));

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
