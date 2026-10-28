//! The app core's encrypted files against a running server: two users
//! register, upload, share and play, as the apps do through the command API.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

fn wav(seconds: u32) -> Vec<u8> {
    let rate = 44100u32;
    let n = rate * seconds;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + n * 4).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&rate.to_le_bytes());
    d.extend_from_slice(&(rate * 4).to_le_bytes());
    d.extend_from_slice(&4u16.to_le_bytes());
    d.extend_from_slice(&16u16.to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(n * 4).to_le_bytes());
    for i in 0..n {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16;
        d.extend_from_slice(&s.to_le_bytes());
        d.extend_from_slice(&s.to_le_bytes());
    }
    d
}

#[test]
fn register_upload_share_and_play() {
    let data = std::env::temp_dir().join(format!("bl-app-vault-srv-{}", std::process::id()));
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

    let mut a = App::new("alice");
    a.connect(&url);
    a.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite())
            .with("user", "Alice")
            .with("password", "short"),
    );
    assert!(
        !a.state().get("vault").str_or("error", "").is_empty(),
        "short password refused"
    );
    a.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite())
            .with("user", "Alice")
            .with("password", "alice password 1"),
    );
    let s = a.ready();
    let v = s.get("vault");
    assert_eq!(v.str_or("error", ""), "");
    assert_eq!(v.str_or("user", ""), "alice");
    let code = v.str_or("recoveryCode", "").to_string();
    assert_eq!(code.len(), 54 + 13, "recovery code shown once");
    a.cmd(Value::object().with("cmd", "vault.dismissCode"));

    // Upload a song.
    let song = a.dir.join("Sine Song.wav");
    std::fs::write(&song, wav(3)).unwrap();
    a.cmd(
        Value::object()
            .with("cmd", "vault.upload")
            .with("paths", vec![song.display().to_string()]),
    );
    a.wait("upload", |s| {
        s.get("vault")
            .get("uploads")
            .as_array()
            .is_some_and(|u| u.is_empty())
            && s.get("vault").get("used").as_u64().unwrap_or(0) > 0
    });
    let files = a.files();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].str_or("name", ""), "Sine Song.wav");
    assert!(files[0].bool_or("audio", false), "{}", files[0].to_json());
    let id = files[0].str_or("id", "").to_string();
    assert!(id.starts_with("vault:"));

    // Bob registers; Alice shares with him.
    let mut b = App::new("bob");
    b.connect(&url);
    b.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite())
            .with("user", "bob")
            .with("password", "bob password 12"),
    );
    assert_eq!(b.ready().get("vault").str_or("error", ""), "");
    assert!(b.files().is_empty());
    a.cmd(Value::object().with("cmd", "vault.refresh"));
    let t0 = Instant::now();
    while a
        .cmd(Value::object().with("cmd", "vault.contacts"))
        .as_array()
        .map_or(0, |c| c.len())
        == 0
    {
        a.state();
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    let r = a.cmd(
        Value::object()
            .with("cmd", "vault.share")
            .with("id", id.trim_start_matches("vault:"))
            .with("user", "bob"),
    );
    assert!(r.get("error").is_null(), "{}", r.to_json());
    let t0 = Instant::now();
    loop {
        a.state();
        if a.files()
            .first()
            .and_then(|f| f.get("recipients").as_array().cloned())
            .is_some_and(|r| r.len() == 1)
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "share did not arrive"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Bob sees and plays it: decrypted while streaming.
    b.cmd(Value::object().with("cmd", "vault.refresh"));
    let t0 = Instant::now();
    while b.files().is_empty() {
        b.state();
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "shared file not listed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let f = &b.files()[0];
    assert_eq!(f.str_or("owner", ""), "alice");
    assert!(!f.bool_or("mine", true));
    assert!(f.bool_or("audio", false), "{}", f.to_json());
    b.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    b.wait("playing", |s| s.str_or("play", "") == "playing");
    let src = b.host.audio.lock().unwrap().clone().expect("audio started");
    let mut loud = 0.0f32;
    let t0 = Instant::now();
    while loud < 0.05 && t0.elapsed() < Duration::from_secs(20) {
        b.state();
        let mut buf = vec![0f32; 4800];
        src.render(&mut buf, 0);
        loud = loud.max(buf.iter().fold(0.0f32, |m, x| m.max(x.abs())));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loud > 0.05, "decrypted audio is heard");
    let s = b.wait("analysis", |s| {
        matches!(s.str_or("analysis", ""), "ready" | "failed")
    });
    assert_eq!(s.str_or("analysis", ""), "ready", "analysed on the device");
    assert_eq!(s.get("current").str_or("title", ""), "Sine Song");

    // Both see the same safety number.
    let sa = a
        .cmd(Value::object().with("cmd", "vault.contacts"))
        .at(0)
        .str_or("safetyNumber", "")
        .to_string();
    let sb = b
        .cmd(Value::object().with("cmd", "vault.contacts"))
        .at(0)
        .str_or("safetyNumber", "")
        .to_string();
    assert_eq!(sa, sb);
    assert_eq!(sa.len(), 71);
    b.cmd(
        Value::object()
            .with("cmd", "vault.verify")
            .with("user", "alice"),
    );
    assert_eq!(
        b.cmd(Value::object().with("cmd", "vault.contacts"))
            .at(0)
            .str_or("status", ""),
        "verified"
    );

    // Lock, then unlock with the password; wrong password first.
    b.cmd(Value::object().with("cmd", "vault.logout"));
    assert_eq!(b.state().get("vault").str_or("state", ""), "locked");
    assert!(b.files().is_empty());
    b.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "bob")
            .with("password", "wrong password"),
    );
    assert!(!b.ready().get("vault").str_or("error", "").is_empty());
    b.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "bob")
            .with("password", "bob password 12"),
    );
    assert_eq!(b.ready().get("vault").str_or("error", ""), "");

    // Revoke: Bob no longer gets it.
    a.cmd(
        Value::object()
            .with("cmd", "vault.revoke")
            .with("id", id.trim_start_matches("vault:"))
            .with("user", "bob"),
    );
    let t0 = Instant::now();
    loop {
        a.state();
        b.cmd(Value::object().with("cmd", "vault.refresh"));
        for _ in 0..10 {
            b.state();
            std::thread::sleep(Duration::from_millis(10));
        }
        if b.files().is_empty() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "revoke did not apply"
        );
    }

    // Password change and recovery code on Alice's side.
    a.cmd(
        Value::object()
            .with("cmd", "vault.changePassword")
            .with("current", "alice password 1")
            .with("password", "alice password 2"),
    );
    assert_eq!(a.ready().get("vault").str_or("error", ""), "");
    a.cmd(Value::object().with("cmd", "vault.logout"));
    a.cmd(
        Value::object()
            .with("cmd", "vault.recover")
            .with("user", "alice")
            .with("code", code.to_lowercase())
            .with("password", "alice password 3"),
    );
    let s = a.ready();
    assert_eq!(s.get("vault").str_or("error", ""), "");
    let code2 = s.get("vault").str_or("recoveryCode", "").to_string();
    assert!(
        !code2.is_empty() && code2 != code,
        "a new recovery code after recovery"
    );
    a.cmd(Value::object().with("cmd", "vault.logout"));
    a.cmd(
        Value::object()
            .with("cmd", "vault.login")
            .with("user", "alice")
            .with("password", "alice password 3"),
    );
    assert_eq!(a.ready().get("vault").str_or("error", ""), "");
    let t0 = Instant::now();
    while a.files().is_empty() {
        a.state();
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "files survive recovery"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(a.files()[0].bool_or("audio", false));

    drop((a, b));
    let _ = std::fs::remove_dir_all(&data);
}
