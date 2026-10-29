//! Public links end to end: an app creates links to an encrypted file
//! (with and without a password); Chromium opens them, the page decrypts
//! the file with vault.wasm. Needs Playwright and the vault WebAssembly
//! module (`cargo build --release -p bl-vault-wasm --target
//! wasm32-unknown-unknown`); run with `cargo test --test share_page --
//! --ignored`.

mod common;

use bl_json::Value;
use common::App;
use std::path::Path;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

#[test]
#[ignore]
fn share_page_decrypts_in_the_browser() {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = ws.join("target/wasm32-unknown-unknown/release/bl_vault_wasm.wasm");
    if !wasm.exists() {
        eprintln!("{} missing", wasm.display());
        return;
    }
    let root = std::env::temp_dir().join(format!("bl-share-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // A web folder with the sign-in and share pages.
    let web = root.join("web");
    copy_dir(&ws.join("apps/beatlumen/web/auth"), &web.join("auth"));
    copy_dir(&ws.join("apps/beatlumen/web/share"), &web.join("share"));
    std::fs::copy(&wasm, web.join("auth/vault.wasm")).unwrap();
    std::fs::copy(
        ws.join("apps/beatlumen/assets/icon.png"),
        web.join("icon.png"),
    )
    .unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html><title>x</title>").unwrap();
    beatlumen_server::release::write_manifest(&web, "test").unwrap();
    let data = root.join("data");
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        web_dir: Some(web),
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);

    let mut a = App::new("share-page");
    a.connect(&url);
    a.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with(
                "invite",
                beatlumen_server::vault::create_invite(&data, 1).unwrap(),
            )
            .with("user", "alice")
            .with("password", "alice password 1"),
    );
    a.ready();
    // Not a valid audio file: the page offers it for download anyway.
    let plain: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let song = a.dir.join("Song.mp3");
    std::fs::write(&song, &plain).unwrap();
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
    let id = a.files()[0].str_or("id", "").to_string();
    let mut link = |password: &str| -> String {
        a.cmd(
            Value::object()
                .with("cmd", "vault.link.create")
                .with("id", id.as_str())
                .with("password", password)
                .with("days", 7u64),
        );
        let s = a.wait("link", |s| {
            !s.get("vault").get("links").get("url").is_null()
        });
        let u = s.get("vault").get("links").str_or("url", "").to_string();
        a.cmd(
            Value::object()
                .with("cmd", "vault.links")
                .with("id", id.as_str()),
        );
        u
    };
    let plain_url = link("");
    let pw_url = link("link password");
    assert!(
        plain_url.contains("/share/") && plain_url.contains('#'),
        "{plain_url}"
    );
    let s = a.wait("links listed", |s| {
        s.get("vault")
            .get("links")
            .get("list")
            .as_array()
            .is_some_and(|l| l.len() == 2)
    });
    let list = s
        .get("vault")
        .get("links")
        .get("list")
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        list.iter().filter(|l| l.bool_or("password", false)).count(),
        1
    );
    assert!(list
        .iter()
        .all(|l| l.get("expires").as_u64().unwrap_or(0) > 0));

    let expected: String = bl_vault::sha256(&[&plain])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/share_page.js");
    let out = std::process::Command::new("node")
        .args([script, &plain_url, &pw_url, "link password", &expected])
        .output()
        .expect("node");
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!("{text}{}", String::from_utf8_lossy(&out.stderr));
    let r = bl_json::parse(text.trim().lines().last().unwrap_or("")).unwrap();
    assert!(r.get("fatal").is_null(), "{text}");
    for k in [
        "plainOk",
        "pwOk",
        "noFileBeforePassword",
        "wrongRefused",
        "incomplete",
    ] {
        assert!(r.bool_or(k, false), "{k}: {text}");
    }
    assert_eq!(r.str_or("plainName", ""), "Song.mp3");
    assert_eq!(
        r.get("errors").as_array().map_or(0, |e| e.len()),
        0,
        "{text}"
    );

    // Revoked: the page reports it.
    let first = list[0].str_or("id", "").to_string();
    a.cmd(
        Value::object()
            .with("cmd", "vault.link.delete")
            .with("id", first.as_str()),
    );
    a.wait("revoked", |s| {
        s.get("vault")
            .get("links")
            .get("list")
            .as_array()
            .is_some_and(|l| l.len() == 1)
    });
    let r = bl_net::client::request(
        "GET",
        &format!("{url}/api/share/{first}"),
        &[],
        &[],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(r.status, 404);
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}
