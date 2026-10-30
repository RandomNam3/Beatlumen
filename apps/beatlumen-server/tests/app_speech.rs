//! Speech recognition in the app (no server): a plain song text of a local
//! song gets line and word times on the device. Needs a model and a speech
//! sample: `BL_SPEECH_MODEL=ggml-tiny.bin BL_SPEECH_SAMPLE=jfk.wav cargo
//! test --release --test app_speech -- --ignored`.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

#[test]
#[ignore]
fn song_text_timed_on_the_device() {
    let (Ok(model), Ok(sample)) = (
        std::env::var("BL_SPEECH_MODEL"),
        std::env::var("BL_SPEECH_SAMPLE"),
    ) else {
        eprintln!("BL_SPEECH_MODEL / BL_SPEECH_SAMPLE not set");
        return;
    };
    let mut app = App::new("speech");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    let song = dir.join("Speech.wav");
    std::fs::copy(&sample, &song).unwrap();
    std::fs::write(
        dir.join("Speech.txt"),
        "And so, my fellow Americans,\nask not what your country can do for you,\n\nask what you can do for your country.",
    )
    .unwrap();
    // The model as if downloaded (same file name as in the list).
    let models = app.dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::copy(&model, models.join("ggml-tiny.bin")).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "speechModel")
            .with("value", "tiny"),
    );
    let s = app.state();
    assert!(
        s.get("speechLocal").bool_or("ready", false),
        "{}",
        s.get("speechLocal").to_json()
    );

    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    // The plain text is loaded first.
    let t0 = Instant::now();
    loop {
        app.state();
        let l = app.cmd(Value::object().with("cmd", "lyrics.get"));
        if l.str_or("status", "") == "ready" {
            assert!(!l.bool_or("synced", true));
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    let r = app.cmd(Value::object().with("cmd", "speech.run"));
    assert!(r.get("error").is_null(), "{r:?}");
    let t0 = Instant::now();
    let l = loop {
        app.state();
        let l = app.cmd(Value::object().with("cmd", "lyrics.get"));
        if l.bool_or("synced", false) {
            break l;
        }
        let s = app.state();
        assert!(
            s.get("speechLocal").get("error").is_null(),
            "{}",
            s.get("speechLocal").to_json()
        );
        assert!(t0.elapsed() < Duration::from_secs(300), "too slow");
        std::thread::sleep(Duration::from_millis(100));
    };
    let lines = l.get("lines").as_array().unwrap();
    assert!(
        lines[0].str_or("text", "").starts_with("And so"),
        "{}",
        l.to_json()
    );
    // Word times (enhanced LRC).
    assert!(
        lines[0]
            .get("words")
            .as_array()
            .is_some_and(|w| w.len() >= 5),
        "{}",
        l.to_json()
    );
}
