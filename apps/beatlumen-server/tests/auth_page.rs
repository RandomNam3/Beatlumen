//! The browser sign-in page with passkeys, end to end: Chromium with a
//! virtual authenticator (PRF) adds a passkey, turns the password off and
//! signs an app in; the app opens the sealed result. Needs a built web
//! client (`BL_WEB_DIR`, see apps/beatlumen/web/finish.sh) and Playwright
//! (`NODE_PATH`); run with `cargo test --test auth_page -- --ignored`.

mod common;

use bl_json::Value;
use bl_vault::handoff::HandoffKey;
use bl_vault::identity::PublicIdentity;
use bl_vault::keys::{Account, KdfParams, KeyBundle};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

#[test]
#[ignore]
fn sign_in_page_with_passkeys() {
    let Ok(web) = std::env::var("BL_WEB_DIR") else {
        eprintln!("BL_WEB_DIR not set");
        return;
    };
    let data = std::env::temp_dir().join(format!("bl-auth-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let _server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:7997".into(),
        rescan_interval: 0,
        web_dir: Some(web.into()),
        public_url: Some("http://localhost:7997".into()),
        ..Default::default()
    })
    .unwrap();
    let base = "http://localhost:7997";
    // An account made by the app.
    let mut app = common::App::new("auth-page");
    app.connect(base);
    let invite = beatlumen_server::vault::create_invite(&data, 1).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite)
            .with("user", "alice")
            .with("password", "alice password 1"),
    );
    assert_eq!(app.ready().get("vault").str_or("error", ""), "");
    let fingerprint = app
        .state()
        .get("vault")
        .str_or("fingerprint", "")
        .to_string();

    // The app's side of a browser sign-in.
    let key = HandoffKey::generate().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr().unwrap().port()
    );
    let app_url = format!(
        "{base}/auth/?mode=app&state=s1&key={}&redirect={}",
        bl_vault::webpush::base64url(&key.public()),
        bl_net::http::percent_encode(&redirect)
    );
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/auth_page.js");
    let mut child = std::process::Command::new("node")
        .args([script, base, "alice", "alice password 1", &app_url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    listener.set_nonblocking(true).unwrap();
    let t0 = std::time::Instant::now();
    let mut line = String::new();
    let mut done_at: Option<Duration> = None;
    loop {
        if let Ok((mut conn, _)) = listener.accept() {
            conn.set_nonblocking(false).unwrap();
            conn.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            BufReader::new(conn.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = conn
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            break;
        }
        // The browser may be done before the connection is taken here.
        let finished = child.try_wait().unwrap().is_some();
        if (finished
            && t0.elapsed()
                > done_at
                    .get_or_insert(t0.elapsed())
                    .saturating_add(Duration::from_secs(2)))
            || t0.elapsed() > Duration::from_secs(120)
        {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let log =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    println!("{log}");
    assert!(out.status.success(), "{log}");
    assert!(log.contains("password refused"), "{log}");
    assert!(
        log.contains(&format!("pairing code {}", key.pairing_code())),
        "{log}"
    );
    assert!(!log.contains("errors:"), "{log}");

    assert!(!line.is_empty(), "no callback: {log}");
    // The result: sealed to the app's key, bound to the state.
    let path = line.split_whitespace().nth(1).unwrap();
    let query = path.split_once('?').unwrap().1;
    let get = |k: &str| {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
    };
    assert_eq!(get("state"), "s1");
    let sealed = bl_net::base64_decode(get("result")).unwrap();
    assert!(key.open("other", &sealed).is_err());
    let plain = key.open("s1", &sealed).unwrap();
    let v = bl_json::parse_bytes(&plain).unwrap();
    let b = v.get("bundle");
    assert!(b.bool_or("passwordOff", false));
    let unb = |k: &str| bl_net::base64_decode(b.str_or(k, "")).unwrap_or_default();
    let p = b.get("params");
    let bundle = KeyBundle {
        salt: unb("salt").try_into().unwrap(),
        params: KdfParams {
            m_kib: p.get("m").as_u64().unwrap() as u32,
            t: p.get("t").as_u64().unwrap() as u32,
            p: p.get("p").as_u64().unwrap() as u32,
        },
        master_by_password: Vec::new(),
        master_by_recovery: unb("masterByRecovery"),
        identity: unb("identity"),
        public: PublicIdentity::from_bytes(&unb("public")).unwrap(),
    };
    let master = bl_vault::Secret::from_bytes(
        bl_net::base64_decode(v.str_or("master", ""))
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let acc = Account::from_master(v.str_or("user", ""), master, &bundle).unwrap();
    assert_eq!(acc.identity.public().fingerprint(), fingerprint);
    drop(app);
    let _ = std::fs::remove_dir_all(&data);
}
