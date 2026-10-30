//! Settings that follow an account: two apps unlocked with the same
//! account take each other's changes (newest wins); device settings stay;
//! the server stores only ciphertext; a device of another account or the
//! shared login sees neither the settings nor the account's devices.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn set(app: &mut App, key: &str, value: Value) {
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", key)
            .with("value", value),
    );
}

fn setting(app: &mut App, key: &str) -> Value {
    app.state().get("settings").get(key).clone()
}

fn near(v: Value, x: f64) -> bool {
    v.as_f64().is_some_and(|v| (v - x).abs() < 1e-4)
}

fn until(apps: &mut [&mut App], what: &str, mut f: impl FnMut(&mut [&mut App]) -> bool) {
    let t0 = Instant::now();
    loop {
        for a in apps.iter_mut() {
            a.state();
        }
        if f(apps) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
fn settings_follow_the_account() {
    let data = std::env::temp_dir().join(format!("bl-account-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);
    let invite = || beatlumen_server::vault::create_invite(&data, 1).unwrap();

    // Device A: an account with its own look.
    let mut a = App::new("acc-a");
    a.connect(&url);
    set(&mut a, "intensity", 1.6.into());
    set(&mut a, "ui.theme", "light".into());
    set(&mut a, "deviceName", "Wohnzimmer".into());
    a.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite())
            .with("user", "carla")
            .with("password", "carla password 1"),
    );
    assert_eq!(a.ready().get("vault").str_or("error", ""), "");
    a.cmd(Value::object().with("cmd", "vault.dismissCode"));
    let stored = data.join("vault").join("userdata");
    let t0 = Instant::now();
    let blob = loop {
        a.state();
        let found = walk(&stored)
            .into_iter()
            .find(|p| p.ends_with("settings.bin"));
        if let Some(p) = found {
            break std::fs::read(p).unwrap();
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "settings stored");
        std::thread::sleep(Duration::from_millis(40));
    };
    // Only ciphertext on the server.
    let text = String::from_utf8_lossy(&blob);
    assert!(
        !text.contains("intensity") && !text.contains("light"),
        "{text}"
    );

    // Device B: unlocks the same account and takes the look, not the name.
    let mut b = App::new("acc-b");
    b.connect(&url);
    set(&mut b, "deviceName", "Küche".into());
    b.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "carla")
            .with("password", "carla password 1"),
    );
    assert_eq!(b.ready().get("vault").str_or("error", ""), "");
    until(&mut [&mut a, &mut b], "b takes a's settings", |apps| {
        near(setting(apps[1], "intensity"), 1.6)
            && setting(apps[1], "ui").get("theme").as_str() == Some("light")
    });
    assert_eq!(setting(&mut b, "deviceName").as_str(), Some("Küche"));

    // A change on B reaches A (through the event channel), and back.
    set(&mut b, "intensity", 0.4.into());
    until(&mut [&mut a, &mut b], "a takes b's change", |apps| {
        near(setting(apps[0], "intensity"), 0.4)
    });
    set(&mut a, "ui.accent", "#ff8800".into());
    until(&mut [&mut a, &mut b], "b takes a's change", |apps| {
        setting(apps[1], "ui").get("accent").as_str() == Some("#ff8800")
    });
    assert_eq!(setting(&mut a, "deviceName").as_str(), Some("Wohnzimmer"));

    // The account's devices are among themselves: a device without the
    // account does not see them.
    let mut c = App::new("acc-c");
    c.connect(&url);
    std::thread::sleep(Duration::from_secs(1));
    c.state();
    let others = c.cmd(Value::object().with("cmd", "devices.list"));
    assert!(
        others
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d.bool_or("self", false)),
        "{}",
        others.to_json()
    );
    until(&mut [&mut a, &mut b], "a sees b", |apps| {
        apps[0]
            .cmd(Value::object().with("cmd", "devices.list"))
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.str_or("name", "") == "Küche")
    });
    drop((a, b, c));
    let _ = std::fs::remove_dir_all(&data);
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
