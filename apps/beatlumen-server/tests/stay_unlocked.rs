//! "Stay unlocked on this device": the account comes back after a restart
//! from the platform's protected store, without the password.

mod common;

use bl_core::{Core, LaunchArgs};
use bl_json::Value;
use common::App;
use std::sync::{Arc, Mutex};

fn restart(app: &mut App) {
    app.core.save_settings(&mut app.host);
    app.core = Core::new(&mut app.host, LaunchArgs::default(), "en");
}

#[test]
fn stays_unlocked_until_locked() {
    let data = std::env::temp_dir().join(format!("bl-stay-unlocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);
    let invite = beatlumen_server::vault::create_invite(&data, 1).unwrap();

    let store = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let mut app = App::new("stay");
    app.host.secrets = Some(store.clone());
    app.connect(&url);
    assert!(app.state().get("vault").bool_or("canRemember", false));
    let password = "correct horse battery";
    app.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite)
            .with("user", "Dana")
            .with("password", password),
    );
    let s = app.ready();
    assert_eq!(s.get("vault").str_or("error", ""), "");
    let fingerprint = s.get("vault").str_or("fingerprint", "").to_string();
    assert_eq!(store.lock().unwrap().len(), 1, "kept after signing in");

    // A restart unlocks without the password.
    restart(&mut app);
    app.connect(&url);
    let s = app.ready();
    assert_eq!(
        s.get("vault").str_or("state", ""),
        "ready",
        "{}",
        s.get("vault").to_json()
    );
    assert_eq!(s.get("vault").str_or("fingerprint", ""), fingerprint);
    assert_eq!(s.get("vault").str_or("user", ""), "dana");
    // The session works: the user list loads.
    app.wait("contacts", |s| s.get("vault").str_or("error", "") == "");
    assert!(app
        .cmd(Value::object().with("cmd", "vault.contacts"))
        .as_array()
        .is_some());

    // Switched off: forgotten; a restart needs the password.
    app.cmd(
        Value::object()
            .with("cmd", "vault.remember")
            .with("value", false),
    );
    assert!(store.lock().unwrap().is_empty());
    restart(&mut app);
    app.connect(&url);
    for _ in 0..20 {
        app.state();
    }
    assert_eq!(app.state().get("vault").str_or("state", ""), "locked");

    // On again after signing in; "Lock" forgets it.
    app.cmd(
        Value::object()
            .with("cmd", "vault.remember")
            .with("value", true),
    );
    app.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "dana")
            .with("password", password),
    );
    app.ready();
    assert_eq!(store.lock().unwrap().len(), 1);
    app.cmd(Value::object().with("cmd", "vault.logout"));
    assert!(store.lock().unwrap().is_empty(), "lock forgets");

    // A password change elsewhere ends this device's session: the kept
    // account is dropped and the user signs in again.
    app.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "dana")
            .with("password", password),
    );
    app.ready();
    let mut other = App::new("stay-other");
    other.connect(&url);
    other.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "dana")
            .with("password", password),
    );
    other.ready();
    other.cmd(
        Value::object()
            .with("cmd", "vault.changePassword")
            .with("current", password)
            .with("password", "another long password"),
    );
    other.wait("changed", |s| {
        let v = s.get("vault");
        v.str_or("state", "") == "ready" && v.get("busy").is_null()
    });
    restart(&mut app);
    app.connect(&url);
    let s = app.wait("session refused", |s| {
        let v = s.get("vault");
        v.str_or("state", "") == "locked" && !v.str_or("error", "").is_empty()
    });
    assert!(
        s.get("vault").str_or("error", "").contains("session"),
        "{}",
        s.get("vault").to_json()
    );
    assert!(store.lock().unwrap().is_empty(), "dropped");

    drop(server);
    let _ = std::fs::remove_dir_all(&data);
}
