//! The app with a server extension: search, choose where songs go (asked
//! once, remembered), play while the server imports, continue with the
//! library track; or keep the song in the user's encrypted files.

mod common;

use bl_json::Value;
use common::fake_extension::{fake_extension, media, TOKEN};
use common::App;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Plays the audio device for `d`; returns the loudest sample.
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

fn until(app: &mut App, what: &str, secs: u64, pred: impl Fn(&Value) -> bool) -> Value {
    let t0 = Instant::now();
    loop {
        tick(app, Duration::from_millis(50));
        let s = app.core.state(&app.host);
        if pred(&s) {
            return s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timeout waiting for {what}: {}",
            s.get("current").to_json()
        );
    }
}

fn search(app: &mut App, q: &str) -> Vec<Value> {
    app.cmd(
        Value::object()
            .with("cmd", "extensions.search")
            .with("query", q),
    );
    until(app, "search results", 20, |s| {
        !s.get("extensions").bool_or("searching", true)
    });
    app.cmd(Value::object().with("cmd", "extensions.results"))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn search_play_and_keep() {
    let dir: PathBuf = std::env::temp_dir().join(format!("bl-ext-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some((webm, jpeg)) = media(&dir) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let ext = fake_extension(webm, jpeg, Arc::new(AtomicUsize::new(0)));
    let token = dir.join("token");
    std::fs::write(&token, TOKEN).unwrap();
    let data = dir.join("data");
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        extensions: vec![("fake".into(), ext)],
        extension_token_file: Some(token),
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);

    let mut a = App::new("ext-alice");
    a.connect(&url);
    let s = until(&mut a, "extension list", 20, |s| {
        s.get("extensions")
            .get("list")
            .as_array()
            .is_some_and(|l| !l.is_empty())
    });
    assert_eq!(
        s.get("extensions").get("list").as_array().unwrap()[0].str_or("name", ""),
        "Fake Tube"
    );

    // Search through the server; pictures come from the server.
    let results = search(&mut a, "fremdes lied & co");
    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert_eq!(r.str_or("ext", ""), "fake");
    assert_eq!(r.str_or("title", ""), "Fremdes Lied");
    assert!(
        r.str_or("image", "").contains("extensions%2Ffake%2Fimage"),
        "{}",
        r.to_json()
    );
    assert!(!r.bool_or("inLibrary", true));

    // First time: the app asks where songs go.
    let play = |a: &mut App, extra: &[(&str, Value)]| {
        let mut c = Value::object()
            .with("cmd", "extensions.play")
            .with("ext", "fake")
            .with("id", "song:1");
        for (k, v) in extra {
            c = c.with(*k, v.clone());
        }
        a.cmd(c)
    };
    assert!(play(&mut a, &[]).bool_or("ask", false));
    // The library, remembered.
    play(
        &mut a,
        &[
            ("target", Value::from("library")),
            ("remember", Value::from(true)),
        ],
    );
    let s = until(&mut a, "playing the import", 15, |s| {
        s.str_or("play", "") == "playing"
    });
    assert_eq!(s.get("extensions").str_or("target", ""), "library");
    let cur = s.get("current");
    assert!(
        cur.str_or("id", "").starts_with("ext:"),
        "{}",
        cur.to_json()
    );
    assert_eq!(cur.str_or("title", ""), "Fremdes Lied");
    assert_eq!(cur.str_or("artist", ""), "Fremde Band");
    assert!(!cur.str_or("cover", "").is_empty());
    // Plays while it is imported: the tone (the silent intro is gone).
    let peak = tick(&mut a, Duration::from_millis(1500));
    assert!(peak > 0.01, "audible ({peak})");

    // The import finishes: the library track takes over, with analysis.
    let s = until(&mut a, "the library track", 60, |s| {
        let id = s.get("current").str_or("id", "").to_string();
        !id.is_empty() && !id.starts_with("ext:")
    });
    let track = s.get("current").str_or("id", "").to_string();
    assert_eq!(s.str_or("play", ""), "playing");
    until(&mut a, "library refresh", 20, |s| {
        s.f64_or("libraryTracks", 0.0) >= 1.0
    });
    until(&mut a, "analysis", 30, |s| {
        s.str_or("analysis", "") == "ready"
    });

    // Searching again finds it in the library; playing it needs no import.
    let results = search(&mut a, "fremdes lied & co");
    assert!(results[0].bool_or("inLibrary", false));
    assert_eq!(results[0].str_or("track", ""), track);
    play(&mut a, &[]);
    let s = until(&mut a, "library track", 10, |s| {
        s.get("current").str_or("id", "") == track
    });
    assert_eq!(s.get("extensions").f64_or("importing", 1.0), 0.0);

    // Another user keeps it in the encrypted files instead.
    let mut b = App::new("ext-bob");
    b.connect(&url);
    b.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with(
                "invite",
                beatlumen_server::vault::create_invite(&data, 1).unwrap(),
            )
            .with("user", "bob")
            .with("password", "bob password 12"),
    );
    assert_eq!(b.ready().get("vault").str_or("error", ""), "");
    b.cmd(Value::object().with("cmd", "vault.dismissCode"));
    until(&mut b, "extension list", 20, |s| {
        s.get("extensions")
            .get("list")
            .as_array()
            .is_some_and(|l| !l.is_empty())
    });
    let results = search(&mut b, "fremdes lied & co");
    // Bob's choice: his files (the song is in the library already, but he
    // wants it for himself: a temporary import).
    assert!(results[0].bool_or("inLibrary", false));
    b.core.command(
        &mut b.host,
        &Value::object()
            .with("cmd", "settings.set")
            .with("key", "extensionTarget")
            .with("value", "vault"),
    );
    // Results already in the library play from there.
    play(&mut b, &[]);
    until(&mut b, "library track", 10, |s| {
        s.get("current").str_or("id", "") == track
    });

    // A fresh import into the encrypted files.
    std::fs::remove_file(data.join("extensions/library/fake/Fremde Band - Fremdes Lied.opus"))
        .unwrap();
    // Gone from the library after a rescan.
    server.state.request_scan();
    assert!(server.wait_for_scan(Duration::from_secs(10)));
    let results = search(&mut b, "fremdes lied & co");
    assert!(
        !results[0].bool_or("inLibrary", true),
        "{}",
        results[0].to_json()
    );
    play(&mut b, &[]);
    until(&mut b, "playing the import", 15, |s| {
        s.get("current").str_or("id", "").starts_with("ext:")
    });
    let t0 = Instant::now();
    loop {
        tick(&mut b, Duration::from_millis(100));
        let files = b.files();
        if files
            .iter()
            .any(|f| f.str_or("name", "") == "Fremde Band - Fremdes Lied.opus")
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "saved in the encrypted files"
        );
    }
    // Not in the shared library.
    assert!(!data
        .join("extensions/library/fake/Fremde Band - Fremdes Lied.opus")
        .exists());
    let _ = std::fs::remove_dir_all(&dir);
}
