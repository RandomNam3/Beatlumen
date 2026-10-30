//! Music sources used by the app itself (no Beatlumen server): a fake
//! Subsonic server's song joins the library, plays as a stream, gets
//! analysed and a cover, is reported as played; the login stays in the
//! protected store; after a restart the song is there from the cache.

mod common;

use bl_json::Value;
use common::App;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn source_track(app: &mut App) -> Option<Value> {
    app.state();
    app.cmd(Value::object().with("cmd", "library.tracks"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|t| t.str_or("id", "").starts_with("src:"))
}

fn position(app: &mut App) -> f64 {
    app.state();
    if let Some(src) = app.host.audio.lock().unwrap().clone() {
        let mut buf = vec![0f32; 4800];
        src.render(&mut buf, 0);
    }
    app.core.position(&app.host)
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn subsonic_in_the_app() {
    let song = Arc::new(common::wav(8));
    let mut img = bl_image::Image::new(48, 48);
    for p in img.pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[20, 140, 220, 255]);
    }
    let cover = Arc::new(bl_image::encode_jpeg(&img, 90));
    let (url, calls) = common::fake_subsonic::fake_subsonic(song, cover);

    let secrets: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>> = Default::default();
    let mut app = App::new("app-sources");
    app.host.secrets = Some(secrets.clone());
    app.state();

    let r = app.cmd(
        Value::object()
            .with("cmd", "sources.save")
            .with("kind", "subsonic")
            .with("name", "Navi")
            .with("url", url.as_str())
            .with("user", "anna")
            .with("secret", "geheim"),
    );
    assert!(r.get("error").is_null(), "{r:?}");
    let t = wait_for("song listed", || source_track(&mut app));
    assert_eq!(t.str_or("title", ""), "Remote One");
    assert!(t.bool_or("source", false));
    let id = t.str_or("id", "").to_string();

    let s = app.state();
    let src = s.get("sources").get("list").at(0).clone();
    assert_eq!(src.get("songs").as_u64(), Some(1));
    assert!(src.bool_or("hasSecret", false));
    // The login is in the protected store only.
    let stored =
        String::from_utf8(secrets.lock().unwrap().get("sources").cloned().unwrap()).unwrap();
    assert!(stored.contains("geheim"));
    for e in walk(&app.dir) {
        let data = std::fs::read(&e).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&data).contains("geheim"),
            "password in {}",
            e.display()
        );
    }

    // Plays as a stream; the visuals get their analysis.
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let t0 = Instant::now();
    while position(&mut app) < 1.0 {
        assert!(t0.elapsed() < Duration::from_secs(20), "no progress");
        std::thread::sleep(Duration::from_millis(10));
    }
    app.wait("analysis", |s| s.str_or("analysis", "") == "ready");
    wait_for("scrobbled", || {
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "scrobble")
            .then_some(())
    });

    // The cover comes from the server.
    app.cmd(
        Value::object()
            .with("cmd", "covers")
            .with("ids", vec![id.clone()]),
    );
    let c = wait_for("cover", || {
        source_track(&mut app)
            .map(|t| t.str_or("cover", "").to_string())
            .filter(|c| !c.is_empty())
    });
    assert!(c.starts_with("file:"), "{c}");

    // After a restart: there right away (cached listing), login restored.
    let dir = app.close();
    let mut app = App::with_dir(dir);
    app.host.secrets = Some(secrets.clone());
    let t = source_track(&mut app).expect("song from the cache");
    assert_eq!(t.str_or("id", ""), id);
    let s = app.wait("listed again", |s| {
        let l = s.get("sources").get("list").at(0);
        l.str_or("error", "").is_empty()
            && !l.bool_or("syncing", true)
            && l.get("synced").as_f64().unwrap_or(0.0) > 0.0
    });
    assert!(s
        .get("sources")
        .get("list")
        .at(0)
        .bool_or("hasSecret", false));

    // Removed: the songs and the login leave.
    let sid = s
        .get("sources")
        .get("list")
        .at(0)
        .str_or("id", "")
        .to_string();
    app.cmd(
        Value::object()
            .with("cmd", "sources.remove")
            .with("id", sid),
    );
    assert!(source_track(&mut app).is_none());
    let stored =
        String::from_utf8(secrets.lock().unwrap().get("sources").cloned().unwrap()).unwrap();
    assert!(!stored.contains("geheim"));
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
