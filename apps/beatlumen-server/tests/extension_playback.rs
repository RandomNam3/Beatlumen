//! Provider feedback comes from the headless player's observations, never
//! from imports. PCM is rendered to memory; no audio device is opened.
mod common;
use bl_json::Value;
use bl_net::{
    client,
    server::{Request, Response, ServerConfig},
};
use common::App;
use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const TOKEN: &str = "test-extension-playback-secret";
fn request(base: &str, path: &str, body: Option<Value>) -> client::Response {
    client::request(
        if body.is_some() { "POST" } else { "GET" },
        &format!("{base}{path}"),
        &[("Content-Type".into(), "application/json".into())],
        body.map(|v| v.to_json().into_bytes())
            .as_deref()
            .unwrap_or(&[]),
        &client::Options::default(),
    )
    .unwrap()
}
fn tick(app: &mut App) {
    app.state();
    if let Some(src) = app.host.audio.lock().unwrap().clone() {
        src.render(&mut [0f32; 960], 0);
    }
    std::thread::sleep(Duration::from_millis(10));
}
#[test]
fn cached_import_reports_real_player_progress_and_pause_but_imports_do_not() {
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let events = received.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ext = format!("http://{}", listener.local_addr().unwrap());
    let wav = common::wav(12);
    let handler: Arc<bl_net::server::Handler> = Arc::new(move |req: Request| {
        assert_eq!(
            req.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        match req.path.as_str() {
            "/manifest" => Response::json(
                200,
                &Value::object().with("protocol", 2u64).with(
                    "capabilities",
                    Value::Array(vec![Value::from("search"), Value::from("playback")]),
                ),
            ),
            "/search" => Response::json(
                200,
                &Value::object().with(
                    "items",
                    Value::Array(vec![Value::object()
                        .with("id", "song-1")
                        .with("title", "Synthetic feedback test")]),
                ),
            ),
            // Content probing recognizes the generated WAV. The import format
            // hint is irrelevant to this test of the generic feedback bridge.
            "/item/song-1" => Response::json(
                200,
                &Value::object()
                    .with("id", "song-1")
                    .with("title", "Synthetic feedback test")
                    .with("format", "flac")
                    .with("duration", 12.0),
            ),
            "/audio/song-1" => Response::bytes(200, "audio/wav", wav.clone()),
            "/playback" => {
                assert_eq!(req.method, "POST");
                events
                    .lock()
                    .unwrap()
                    .push(bl_json::parse_bytes(&req.body).unwrap());
                Response::json(200, &Value::object().with("ok", true))
            }
            _ => Response::error(404, "not found"),
        }
    });
    std::thread::spawn(move || {
        let _ = bl_net::server::serve(listener, handler, ServerConfig::default());
    });
    let dir = std::env::temp_dir().join(format!("bl-feedback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("extension-token");
    std::fs::write(&token, TOKEN).unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: dir.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        extensions: vec![("fake".into(), ext)],
        extension_token_file: Some(token),
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://{}", server.addr);
    let imported = request(
        &base,
        "/api/extensions/fake/import",
        Some(Value::object().with("id", "song-1")),
    );
    assert_eq!(imported.status, 200);
    let job = bl_json::parse_bytes(&imported.body).unwrap();
    let started = Instant::now();
    loop {
        let res = request(
            &base,
            &format!("/api/extensions/jobs/{}", job.str_or("job", "")),
            None,
        );
        let status = bl_json::parse_bytes(&res.body).unwrap();
        if !status.str_or("track", "").is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{}",
            status.to_json()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        received.lock().unwrap().is_empty(),
        "import must not count as playback"
    );
    let raw = "client-session-feedback-test";
    let invalid = request(
        &base,
        "/api/extensions/playback",
        Some(
            Value::object()
                .with("session", raw)
                .with("track", "unrelated")
                .with("seq", 0u64)
                .with("position", -1.0)
                .with("state", "start"),
        ),
    );
    assert_eq!(invalid.status, 400);
    let mut app = App::new("provider-feedback");
    app.connect(&base);
    let until = Instant::now();
    while app
        .core
        .state(&app.host)
        .get("extensions")
        .get("list")
        .as_array()
        .is_none_or(|l| l.is_empty())
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(20));
    }
    app.cmd(
        Value::object()
            .with("cmd", "extensions.search")
            .with("query", "feedback"),
    );
    let until = Instant::now();
    while app
        .core
        .state(&app.host)
        .get("extensions")
        .bool_or("searching", true)
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(20));
    }
    app.cmd(
        Value::object()
            .with("cmd", "extensions.play")
            .with("ext", "fake")
            .with("id", "song-1")
            .with("target", "library"),
    );
    let until = Instant::now();
    while !received
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.str_or("state", "") == "playing")
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(20));
    }
    app.cmd(Value::object().with("cmd", "pause"));
    let until = Instant::now();
    while !received
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.str_or("state", "") == "paused")
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(10));
    }
    app.cmd(Value::object().with("cmd", "seek").with("position", 9.0));
    let until = Instant::now();
    while !received
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.str_or("state", "") == "seeking")
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(10));
    }
    app.cmd(Value::object().with("cmd", "stop"));
    let until = Instant::now();
    while !received
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.str_or("state", "") == "ended")
    {
        tick(&mut app);
        assert!(until.elapsed() < Duration::from_secs(10));
    }
    let got = received.lock().unwrap();
    assert_eq!(
        got.iter()
            .filter(|e| e.str_or("state", "") == "start")
            .count(),
        1
    );
    let first = &got[0];
    assert_eq!(first.str_or("id", ""), "song-1");
    assert_eq!(first.str_or("session", "").len(), 64);
    assert!(got
        .iter()
        .all(|e| e.str_or("session", "") == first.str_or("session", "")
            && e.get("observed_at").as_u64().is_some()));
    assert!(got
        .iter()
        .any(|e| e.str_or("state", "") == "playing" && e.f64_or("position", 0.0) > 4.0));
    drop(got);
    let ignored = request(
        &base,
        "/api/extensions/playback",
        Some(
            Value::object()
                .with("session", raw)
                .with("track", "unrelated")
                .with("seq", 0u64)
                .with("position", 0.0)
                .with("state", "start")
                .with("observed_at", 1u64),
        ),
    );
    assert_eq!(ignored.status, 200);
    assert!(bl_json::parse_bytes(&ignored.body)
        .unwrap()
        .bool_or("ignored", false));
}
