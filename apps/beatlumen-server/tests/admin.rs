//! The admin interface: setup link, login and sessions, settings (live,
//! after a restart, fixed by the command line), the server password,
//! invitations, protection against other sites, and model downloads
//! (resumed, checked against the checksum) from a local fake source.

use beatlumen_server::{config, Config};
use bl_json::Value;
use bl_net::client;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Http {
    base: String,
    cookie: Option<String>,
}

impl Http {
    fn req(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> client::Response {
        let mut headers: Vec<(String, String)> = vec![("X-Beatlumen-Admin".into(), "1".into())];
        if let Some(c) = &self.cookie {
            headers.push(("Cookie".into(), c.clone()));
        }
        let data = body.map(|b| b.to_json().into_bytes()).unwrap_or_default();
        if !data.is_empty() {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        for (k, v) in extra {
            headers.push((k.to_string(), v.to_string()));
        }
        client::request(
            method,
            &format!("{}{path}", self.base),
            &headers,
            &data,
            &client::Options::default(),
        )
        .unwrap()
    }

    fn json(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let r = self.req(method, path, body, &[]);
        let v = bl_json::parse_bytes(&r.body).unwrap_or(Value::Null);
        (r.status, v)
    }

    /// Keeps the session cookie of a login or setup answer.
    fn take_cookie(&mut self, r: &client::Response) {
        let c = r.headers.get("set-cookie").expect("session cookie");
        assert!(
            c.contains("HttpOnly")
                && c.contains("SameSite=Strict")
                && c.contains("Path=/api/admin"),
            "{c}"
        );
        self.cookie = Some(c.split(';').next().unwrap().to_string());
    }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bl-admin-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A file source for the model downloads: answers with `body` (from the
/// requested range start); the first answer stops after `cut` bytes.
/// Records the Range headers it got.
fn fake_source(body: Arc<Vec<u8>>, cut: usize) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/models/", l.local_addr().unwrap());
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let seen = ranges.clone();
    std::thread::spawn(move || {
        for (n, s) in l.incoming().flatten().enumerate() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut from = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    from = v.trim().trim_end_matches('-').parse().unwrap_or(0);
                    seen.lock().unwrap().push(v.trim().to_string());
                }
            }
            let mut s = s;
            let total = body.len();
            let head = if from > 0 {
                format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{total}\r\nConnection: close\r\n\r\n", total - from, total - 1)
            } else {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")
            };
            let _ = s.write_all(head.as_bytes());
            let end = if n == 0 { cut.min(total) } else { total };
            let _ = s.write_all(&body[from.min(end)..end]);
        }
    });
    (url, ranges)
}

fn wait<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn admin_interface() {
    let root = temp("main");
    let data = root.join("data");
    let model = beatlumen_server::models::find("base-q5_1").unwrap();
    let fake = Arc::new(vec![0u8; model.size as usize]);
    let (source, ranges) = fake_source(fake, 1000);
    let mut cfg = Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        model_source: Some(source),
        ..Default::default()
    };
    // As if given on the command line: fixed.
    cfg.origins.push((
        "analysisMaxMinutes".into(),
        "command line --analysis-max-minutes".into(),
    ));
    let server = beatlumen_server::start(cfg).unwrap();
    let base = format!("http://{}", server.addr);
    let mut http = Http {
        base: format!("{base}/api/admin/"),
        cookie: None,
    };

    // The pages, with their own strict policy.
    let page = client::request(
        "GET",
        &format!("{base}/admin/"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(page.status, 200);
    let csp = page.headers.get("content-security-policy").unwrap();
    assert!(
        csp.starts_with("default-src 'none'; script-src 'self'"),
        "{csp}"
    );
    assert!(String::from_utf8_lossy(&page.body).contains("admin.js"));

    // Every API call needs the header.
    let bare = client::request(
        "GET",
        &format!("{base}/api/admin/state"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(bare.status, 403);
    let (s, st) = http.json("GET", "state", None);
    assert_eq!(s, 200);
    assert!(st.bool_or("setup", false) && !st.bool_or("loggedIn", true));
    assert_eq!(http.json("GET", "overview", None).0, 401);

    // Setup: a wrong token, a short password, then the link.
    let token = beatlumen_server::admin::create_setup_token(&data, 3600).unwrap();
    let (s, _) = http.json(
        "POST",
        "setup",
        Some(
            Value::object()
                .with("token", "00")
                .with("password", "a long enough password"),
        ),
    );
    assert_eq!(s, 403);
    let (s, _) = http.json(
        "POST",
        "setup",
        Some(
            Value::object()
                .with("token", token.as_str())
                .with("password", "short"),
        ),
    );
    assert_eq!(s, 400);
    let r = http.req(
        "POST",
        "setup",
        Some(
            Value::object()
                .with("token", token.as_str())
                .with("password", "das admin passwort"),
        ),
        &[],
    );
    assert_eq!(r.status, 200);
    http.take_cookie(&r);
    assert!(http.json("GET", "state", None).1.bool_or("loggedIn", false));
    // The token is used up.
    let (s, _) = http.json(
        "POST",
        "setup",
        Some(
            Value::object()
                .with("token", token.as_str())
                .with("password", "another long password"),
        ),
    );
    assert_eq!(s, 403);

    // Other sites cannot write.
    let r = http.req(
        "PUT",
        "settings",
        Some(Value::object().with("values", Value::object().with("name", "Evil"))),
        &[("Origin", "http://evil.example")],
    );
    assert_eq!(r.status, 403);

    // Settings: live, after a restart, fixed.
    let musik = root.join("musik");
    let (s, v) = http.json(
        "PUT",
        "settings",
        Some(
            Value::object().with(
                "values",
                Value::object()
                    .with("name", "Wohnzimmer")
                    .with("rescanInterval", 300u64),
            ),
        ),
    );
    assert_eq!(s, 200, "{}", v.to_json());
    assert!(!v.bool_or("restartPending", true));
    let info = client::request(
        "GET",
        &format!("{base}/api/info"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&info.body).contains("Wohnzimmer"));
    let (s, v) = http.json(
        "PUT",
        "settings",
        Some(Value::object().with(
            "values",
            Value::object().with("music", vec![musik.display().to_string()]),
        )),
    );
    assert_eq!(s, 200);
    assert!(v.bool_or("restartPending", false));
    let (s, _) = http.json(
        "PUT",
        "settings",
        Some(Value::object().with("values", Value::object().with("analysisMaxMinutes", 5u64))),
    );
    assert_eq!(s, 409);
    let (s, _) = http.json(
        "PUT",
        "settings",
        Some(Value::object().with("values", Value::object().with("bind", "0.0.0.0:1"))),
    );
    assert_eq!(s, 400);
    let (s, _) = http.json(
        "PUT",
        "settings",
        Some(Value::object().with("values", Value::object().with("rescanInterval", "often"))),
    );
    assert_eq!(s, 400);
    let (_, all) = http.json("GET", "settings", None);
    let fixed = all
        .get("settings")
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s.str_or("key", "") == "analysisMaxMinutes")
        .unwrap()
        .clone();
    assert_eq!(
        fixed.str_or("lockedBy", ""),
        "command line --analysis-max-minutes"
    );
    // The next start reads them (environment and command line still win).
    match config::parse_args([
        "--data".to_string(),
        data.display().to_string(),
        "--name".into(),
        "Cli".into(),
    ])
    .unwrap()
    {
        config::Action::Run(c) => {
            assert_eq!(c.name, "Cli");
            assert_eq!(c.rescan_interval, 300);
            assert_eq!(c.music_dirs, vec![musik.clone()]);
            assert!(c
                .origins
                .iter()
                .any(|(k, o)| k == "name" && o == "command line --name"));
        }
        a => panic!("unexpected {a:?}"),
    }

    // The server password: apps then must log in; old logins end.
    let lib = || {
        client::request(
            "GET",
            &format!("{base}/api/library"),
            &[],
            &[],
            &client::Options::default(),
        )
        .unwrap()
        .status
    };
    assert_eq!(lib(), 200);
    let (s, _) = http.json(
        "PUT",
        "server-password",
        Some(Value::object().with("password", "server passwort 1")),
    );
    assert_eq!(s, 204);
    assert_eq!(lib(), 401);
    let login = client::request(
        "POST",
        &format!("{base}/api/login"),
        &[],
        br#"{"password":"server passwort 1"}"#,
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(
        login.status,
        200,
        "{}",
        String::from_utf8_lossy(&login.body)
    );
    let app_token = bl_json::parse_bytes(&login.body)
        .unwrap()
        .str_or("token", "")
        .to_string();
    let with_token = |t: &str| {
        client::request(
            "GET",
            &format!("{base}/api/library"),
            &[("Authorization".into(), format!("Bearer {t}"))],
            &[],
            &client::Options::default(),
        )
        .unwrap()
        .status
    };
    assert_eq!(with_token(&app_token), 200);
    assert_eq!(http.json("POST", "logins/revoke", None).0, 204);
    assert_eq!(with_token(&app_token), 401);
    let (s, _) = http.json(
        "PUT",
        "server-password",
        Some(Value::object().with("password", Value::Null)),
    );
    assert_eq!(s, 204);
    assert_eq!(lib(), 200);

    // Invitations.
    let (s, v) = http.json("POST", "invites", Some(Value::object().with("days", 2u64)));
    assert_eq!(s, 201);
    assert_eq!(v.str_or("code", "").len(), 29, "{}", v.to_json());
    let (_, list) = http.json("GET", "invites", None);
    let list = list.as_array().unwrap().clone();
    assert_eq!(list.len(), 1);
    let id = list[0].str_or("id", "").to_string();
    assert_eq!(http.json("DELETE", &format!("invites/{id}"), None).0, 204);
    assert!(http
        .json("GET", "invites", None)
        .1
        .as_array()
        .unwrap()
        .is_empty());
    assert!(http
        .json("GET", "users", None)
        .1
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(http.json("DELETE", "users/nobody", None).0, 404);

    // Overview and log.
    let (_, o) = http.json("GET", "overview", None);
    assert_eq!(o.str_or("name", ""), "Wohnzimmer");
    assert!(o.bool_or("restartPending", false) && !o.bool_or("canRestart", true));
    assert_eq!(http.json("POST", "restart", None).0, 503);
    let (_, log) = http.json("GET", "log", None);
    let log: Vec<String> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert!(log.iter().any(|l| l.contains("admin changed settings")));
    assert!(
        !log.iter().any(|l| l.contains(&token)),
        "the setup token stays out of the log view"
    );

    // A model download: cut off, resumed with a range, rejected by the
    // checksum (the fake file is zeros).
    let state_of = |http: &Http| {
        let (_, v) = http.json("GET", "speech", None);
        v.get("models")
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m.str_or("id", "") == model.id)
            .unwrap()
            .clone()
    };
    let (s, _) = http.json(
        "POST",
        "speech/model",
        Some(Value::object().with("id", model.id)),
    );
    assert_eq!(s, 202);
    let m = wait("first download fails", 30, || {
        let m = state_of(&http);
        (m.str_or("state", "") == "failed").then_some(m)
    });
    assert!(
        m.str_or("error", "").contains("ended after 1000"),
        "{}",
        m.to_json()
    );
    http.json(
        "POST",
        "speech/model",
        Some(Value::object().with("id", model.id)),
    );
    let m = wait("second download checked", 60, || {
        let m = state_of(&http);
        (m.str_or("state", "") == "failed" && m.str_or("error", "").contains("checksum"))
            .then_some(m)
    });
    assert!(!m.bool_or("downloaded", true));
    assert_eq!(ranges.lock().unwrap().as_slice(), ["1000-"]);
    assert!(!data.join("models").join(model.file).exists());
    assert!(server.state.lyrics_sync.model().is_none());

    // Changing the admin password ends the other sessions.
    let mut other = Http {
        base: http.base.clone(),
        cookie: None,
    };
    let r = other.req(
        "POST",
        "login",
        Some(Value::object().with("password", "das admin passwort")),
        &[],
    );
    other.take_cookie(&r);
    assert_eq!(other.json("GET", "overview", None).0, 200);
    assert_eq!(
        http.json(
            "POST",
            "login",
            Some(Value::object().with("password", "falsch"))
        )
        .0,
        401
    );
    let r = http.req(
        "PUT",
        "password",
        Some(
            Value::object()
                .with("current", "das admin passwort")
                .with("password", "das neue admin passwort"),
        ),
        &[],
    );
    assert_eq!(r.status, 200);
    http.take_cookie(&r);
    assert_eq!(other.json("GET", "overview", None).0, 401);
    assert_eq!(http.json("GET", "overview", None).0, 200);
    assert_eq!(http.req("POST", "logout", None, &[]).status, 204);
    assert_eq!(http.json("GET", "overview", None).0, 401);
    let _ = std::fs::remove_dir_all(&root);
}

/// A real model through the download: checked and switched to. Needs the
/// tiny model file: `BL_SPEECH_MODEL_TINY=ggml-tiny.bin cargo test
/// --release --test admin -- --ignored`.
#[test]
#[ignore]
fn model_download_switches_the_model() {
    let Ok(file) = std::env::var("BL_SPEECH_MODEL_TINY") else {
        eprintln!("BL_SPEECH_MODEL_TINY not set");
        return;
    };
    let mut body = Vec::new();
    std::fs::File::open(file)
        .unwrap()
        .read_to_end(&mut body)
        .unwrap();
    let (source, _) = fake_source(Arc::new(body), usize::MAX);
    let root = temp("model");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("admin-setup.json"), "[]").unwrap();
    let server = beatlumen_server::start(Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        model_source: Some(source),
        ..Default::default()
    })
    .unwrap();
    let mut http = Http {
        base: format!("http://{}/api/admin/", server.addr),
        cookie: None,
    };
    let token = beatlumen_server::admin::create_setup_token(&data, 60).unwrap();
    let r = http.req(
        "POST",
        "setup",
        Some(
            Value::object()
                .with("token", token.as_str())
                .with("password", "das admin passwort"),
        ),
        &[],
    );
    http.take_cookie(&r);
    assert_eq!(
        http.json(
            "POST",
            "speech/model",
            Some(Value::object().with("id", "tiny"))
        )
        .0,
        202
    );
    wait("model switched", 120, || {
        server.state.lyrics_sync.available().then_some(())
    });
    assert_eq!(
        server.state.lyrics_sync.model().unwrap(),
        data.join("models").join("ggml-tiny.bin")
    );
    let info = client::request(
        "GET",
        &format!("http://{}/api/info", server.addr),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert!(bl_json::parse_bytes(&info.body)
        .unwrap()
        .bool_or("speech", false));
    // Settings remember it.
    let saved = std::fs::read_to_string(config::settings_path(&data)).unwrap();
    assert!(saved.contains("ggml-tiny.bin"));
    let _ = std::fs::remove_dir_all(&root);
}
