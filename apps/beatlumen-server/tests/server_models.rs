//! Speech models from the server: its own model (with the timing head made
//! for it) offered to a signed-in app, downloaded, checked and used there;
//! nothing offered when the admin turns it off.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn until(app: &mut App, what: &str, mut f: impl FnMut(&mut App) -> bool) {
    let t0 = Instant::now();
    loop {
        app.state();
        if f(app) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn models(app: &mut App) -> Vec<Value> {
    app.cmd(Value::object().with("cmd", "speech.list"))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn find(list: &[Value], id: &str) -> Option<Value> {
    list.iter().find(|m| m.str_or("id", "") == id).cloned()
}

#[test]
fn own_models_reach_the_apps() {
    let root = std::env::temp_dir().join(format!("bl-server-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("data").join("models");
    std::fs::create_dir_all(&dir).unwrap();
    let weights: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(dir.join("ggml-mine-q5_1.bin"), &weights).unwrap();
    std::fs::write(dir.join("ggml-mine.head"), b"head made for it").unwrap();
    std::fs::write(
        dir.join("ggml-mine-q5_1.json"),
        r#"{"note": "trained on singing", "rank": 1, "memoryMb": 250}"#,
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), b"not a model").unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        password: Some("pw".into()),
        rescan_interval: 0,
        auto_labels: false,
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://{}", server.addr);
    let get = |path: &str| {
        bl_net::client::request(
            "GET",
            &format!("{base}{path}"),
            &[],
            &[],
            &Default::default(),
        )
        .unwrap()
    };
    // Only for signed-in apps.
    assert_eq!(get("/api/models").status, 401);
    assert_eq!(get("/api/models/mine-q5_1/ggml-mine-q5_1.bin").status, 401);

    let mut app = App::new("server-models");
    app.cmd(
        Value::object()
            .with("cmd", "server.connect")
            .with("url", base.as_str()),
    );
    app.wait("login", |s| s.str_or("server", "") == "login");
    app.cmd(
        Value::object()
            .with("cmd", "server.login")
            .with("password", "pw"),
    );
    app.wait("server", |s| s.str_or("server", "") == "connected");

    // The offer arrives with the list.
    let mut offered = None;
    until(&mut app, "offer", |app| {
        offered = find(&models(app), "mine-q5_1");
        offered.is_some()
    });
    let m = offered.unwrap();
    assert!(m.bool_or("server", false) && m.bool_or("own", false));
    assert!(!m.bool_or("downloaded", true));
    assert_eq!(m.str_or("note", ""), "trained on singing");
    assert_eq!(m.get("memoryMb").as_u64(), Some(250));
    assert_eq!(m.get("size").as_u64(), Some(300_000 + 16));

    // Picked: both files come from the server, checked, and the model is in use.
    let r = app.cmd(
        Value::object()
            .with("cmd", "speech.pick")
            .with("id", "mine-q5_1"),
    );
    assert!(r.get("error").is_null(), "{}", r.to_json());
    until(&mut app, "in use", |app| {
        let s = app.state();
        s.get("settings").str_or("speechModel", "") == "mine-q5_1"
    });
    let models_dir = app.dir.join("models");
    assert_eq!(
        std::fs::read(models_dir.join("ggml-mine-q5_1.bin")).unwrap(),
        weights
    );
    assert_eq!(
        std::fs::read(models_dir.join("ggml-mine.head")).unwrap(),
        b"head made for it"
    );
    let desc =
        bl_json::parse_bytes(&std::fs::read(models_dir.join("ggml-mine-q5_1.json")).unwrap())
            .unwrap();
    assert_eq!(desc.get("rank").as_u64(), Some(1));
    let s = app.state();
    assert!(
        s.get("speechLocal").bool_or("ready", false),
        "{}",
        s.get("speechLocal").to_json()
    );
    let m = find(&models(&mut app), "mine-q5_1").unwrap();
    assert!(m.bool_or("downloaded", false) && m.bool_or("current", false));

    // A file of no offered model is not served, nor anything else in the folder.
    let login = bl_net::client::request(
        "POST",
        &format!("{base}/api/login"),
        &[("Content-Type".to_string(), "application/json".to_string())],
        br#"{"password": "pw"}"#,
        &Default::default(),
    )
    .unwrap();
    let token = bl_json::parse_bytes(&login.body)
        .unwrap()
        .str_or("token", "")
        .to_string();
    assert!(!token.is_empty());
    let with_token = |p: &str| get(&format!("{p}?token={token}"));
    let file = with_token("/api/models/mine-q5_1/ggml-mine.head");
    assert_eq!(
        (file.status, file.body.as_slice()),
        (200, &b"head made for it"[..])
    );
    for p in [
        "/api/models/mine-q5_1/notes.txt",
        "/api/models/mine-q5_1/ggml-mine-q5_1.json",
        "/api/models/mine-q5_1/..%2Fsettings.json",
        "/api/models/small/ggml-small.bin",
        "/api/models/other/ggml-mine.head",
    ] {
        assert_eq!(with_token(p).status, 404, "{p}");
    }

    // Deleted on the device: gone there, offered again.
    app.cmd(
        Value::object()
            .with("cmd", "speech.delete")
            .with("id", "mine-q5_1"),
    );
    assert!(!models_dir.join("ggml-mine-q5_1.bin").exists());
    assert!(!models_dir.join("ggml-mine.head").exists());
    let m = find(&models(&mut app), "mine-q5_1").unwrap();
    assert!(!m.bool_or("downloaded", true) && m.bool_or("server", false));
    assert_eq!(app.state().get("settings").str_or("speechModel", "x"), "");

    // Turned off by the admin: nothing offered.
    let mut cfg = server.state.config().as_ref().clone();
    cfg.offer_models = false;
    server.state.apply_config(cfg, false);
    assert_eq!(
        with_token("/api/models/mine-q5_1/ggml-mine.head").status,
        404
    );
    let v = bl_json::parse_bytes(&with_token("/api/models").body).unwrap();
    assert_eq!(v.get("models").as_array().map(Vec::len), Some(0));
    drop(app);
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}
