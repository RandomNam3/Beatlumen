//! Devices of one server login: two apps see each other with what they
//! play, one controls the other, takes its playback over and both start
//! together; remote control can be turned off.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn devices(app: &mut App) -> Vec<Value> {
    app.state();
    app.cmd(Value::object().with("cmd", "devices.list"))
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn other(app: &mut App) -> Option<Value> {
    devices(app)
        .into_iter()
        .find(|d| !d.bool_or("self", true) && d.bool_or("online", false))
}

fn until(app: &mut App, what: &str, secs: u64, mut f: impl FnMut(&mut App) -> bool) {
    let t0 = Instant::now();
    while !f(app) {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Both apps tick until `f` holds for `a`.
fn until2(
    a: &mut App,
    b: &mut App,
    what: &str,
    secs: u64,
    mut f: impl FnMut(&mut App, &mut App) -> bool,
) {
    let t0 = Instant::now();
    loop {
        a.state();
        b.state();
        if f(a, b) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn position(app: &mut App) -> f64 {
    app.state();
    app.core.position(&app.host)
}

#[test]
fn devices_control_and_take_over_each_other() {
    let root = std::env::temp_dir().join(format!("bl-devices-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("Long Song.wav"), common::wav(30)).unwrap();
    std::fs::write(music.join("Other Song.wav"), common::wav(30)).unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let url = format!("http://{}", server.addr);

    let mut a = App::new("dev-a");
    let mut b = App::new("dev-b");
    for (app, name) in [(&mut a, "Wohnzimmer"), (&mut b, "Küche")] {
        app.cmd(
            Value::object()
                .with("cmd", "settings.set")
                .with("key", "deviceName")
                .with("value", name),
        );
        app.connect(&url);
    }
    let tracks = common::wait_tracks(&mut a, 2);
    let ids: Vec<String> = tracks
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect();

    // They see each other.
    until2(&mut a, &mut b, "devices listed", 20, |a, b| {
        other(a).is_some() && other(b).is_some()
    });
    let a_id = other(&mut b).unwrap().str_or("id", "").to_string();
    let b_id = other(&mut a).unwrap().str_or("id", "").to_string();
    assert_eq!(other(&mut b).unwrap().str_or("name", ""), "Wohnzimmer");

    // A plays; B sees it.
    a.cmd(Value::object().with("cmd", "play").with("ids", ids.clone()));
    a.wait("a plays", |s| s.str_or("play", "") == "playing");
    until2(&mut a, &mut b, "b sees a playing", 20, |_, b| {
        other(b).is_some_and(|d| {
            d.get("state").str_or("play", "") == "playing"
                && d.get("state").get("track").str_or("id", "") == ids[0]
        })
    });

    // B pauses A, then seeks it.
    b.cmd(
        Value::object()
            .with("cmd", "devices.command")
            .with("id", a_id.as_str())
            .with("command", Value::object().with("cmd", "pause")),
    );
    until2(&mut a, &mut b, "a paused", 20, |a, _| {
        a.state().str_or("play", "") == "paused"
    });
    b.cmd(
        Value::object()
            .with("cmd", "devices.command")
            .with("id", a_id.as_str())
            .with(
                "command",
                Value::object().with("cmd", "seek").with("position", 12.0),
            ),
    );
    until2(&mut a, &mut b, "a seeked", 20, |a, _| {
        (position(a) - 12.0).abs() < 0.5
    });
    // Paths of a device are never played by remote command.
    b.cmd(
        Value::object()
            .with("cmd", "devices.command")
            .with("id", a_id.as_str())
            .with(
                "command",
                Value::object()
                    .with("cmd", "play")
                    .with("ids", vec!["/etc/passwd"]),
            ),
    );
    std::thread::sleep(Duration::from_millis(500));
    let cur = a.state().get("current").str_or("id", "").to_string();
    assert_eq!(cur, ids[0], "still the server song");

    // B takes A's playback over (at A's position, A pauses).
    a.cmd(Value::object().with("cmd", "play"));
    a.wait("a plays again", |s| s.str_or("play", "") == "playing");
    b.cmd(
        Value::object()
            .with("cmd", "devices.pull")
            .with("id", a_id.as_str()),
    );
    until2(&mut a, &mut b, "b took over", 20, |a, b| {
        let sb = b.state();
        sb.str_or("play", "") == "playing"
            && sb.get("current").str_or("id", "") == ids[0]
            && a.state().str_or("play", "") == "paused"
    });
    let pb = position(&mut b);
    assert!(pb >= 11.5, "b continues where a was: {pb}");

    // Together: B starts its queue on A at a common time.
    b.cmd(
        Value::object()
            .with("cmd", "devices.group")
            .with("ids", vec![a_id.clone()]),
    );
    until2(&mut a, &mut b, "both play", 20, |a, b| {
        a.state().str_or("play", "") == "playing" && b.state().str_or("play", "") == "playing"
    });
    std::thread::sleep(Duration::from_millis(500));
    let (pa, pb) = (position(&mut a), position(&mut b));
    assert!((pa - pb).abs() < 0.35, "in step: {pa} / {pb}");

    // Remote control off: commands are refused.
    a.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "remoteControl")
            .with("value", false),
    );
    until2(&mut a, &mut b, "b sees it", 20, |_, b| {
        other(b).is_some_and(|d| !d.bool_or("remote", true))
    });
    b.cmd(
        Value::object()
            .with("cmd", "devices.command")
            .with("id", a_id.as_str())
            .with("command", Value::object().with("cmd", "pause")),
    );
    std::thread::sleep(Duration::from_millis(800));
    until(&mut a, "a keeps playing", 5, |a| {
        a.state().str_or("play", "") == "playing"
    });
    let _ = b_id;
    drop(a);
    drop(b);
    let _ = std::fs::remove_dir_all(&root);
}
