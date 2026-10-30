//! Remote music sources set up in the admin interface: a fake Subsonic
//! server's songs become part of the library, play through the server
//! (with range requests), get analysed and covers, are reported as played,
//! survive a restart (cached listing) and leave with their source.

mod common;

use beatlumen_server::Config;
use bl_json::Value;
use bl_net::client;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn temp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "bl-sources-{name}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
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

struct Admin {
    base: String,
    cookie: String,
}

impl Admin {
    fn login(addr: std::net::SocketAddr, data: &std::path::Path) -> Admin {
        let token = beatlumen_server::admin::create_setup_token(data, 3600).unwrap();
        let base = format!("http://{addr}/api/admin/");
        let r = client::request(
            "POST",
            &format!("{base}setup"),
            &[
                ("X-Beatlumen-Admin".into(), "1".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            Value::object()
                .with("token", token.as_str())
                .with("password", "das admin passwort")
                .to_json()
                .as_bytes(),
            &client::Options::default(),
        )
        .unwrap();
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let cookie = r.headers.get("set-cookie").unwrap();
        Admin {
            base,
            cookie: cookie.split(';').next().unwrap().to_string(),
        }
    }

    fn json(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let r = client::request(
            method,
            &format!("{}{path}", self.base),
            &[
                ("X-Beatlumen-Admin".into(), "1".into()),
                ("Cookie".into(), self.cookie.clone()),
                ("Content-Type".into(), "application/json".into()),
            ],
            body.map(|b| b.to_json()).unwrap_or_default().as_bytes(),
            &client::Options::default(),
        )
        .unwrap();
        (
            r.status,
            bl_json::parse_bytes(&r.body).unwrap_or(Value::Null),
        )
    }
}

fn get(url: &str, headers: &[(String, String)]) -> client::Response {
    let opts = client::Options {
        max_body: 64 << 20,
        ..Default::default()
    };
    client::request("GET", url, headers, &[], &opts).unwrap()
}

/// The library's tracks.
fn tracks(addr: std::net::SocketAddr) -> Vec<Value> {
    let r = get(&format!("http://{addr}/api/library"), &[]);
    bl_json::parse_bytes(&r.body)
        .unwrap()
        .get("tracks")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn remote_track(addr: std::net::SocketAddr) -> Option<Value> {
    tracks(addr)
        .into_iter()
        .find(|t| t.str_or("title", "") == "Remote One")
}

#[test]
fn subsonic_songs_join_the_library() {
    let song = Arc::new(common::wav(2));
    let mut img = bl_image::Image::new(64, 64);
    for p in img.pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[200, 40, 90, 255]);
    }
    let cover = Arc::new(bl_image::encode_jpeg(&img, 90));
    let (url, calls) = common::fake_subsonic::fake_subsonic(song.clone(), cover);

    let root = temp("main");
    let data = root.join("data");
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("local.wav"), common::wav(1)).unwrap();
    let start = || {
        beatlumen_server::start(Config {
            data_dir: data.clone(),
            music_dirs: vec![music.clone()],
            bind: "127.0.0.1:0".into(),
            rescan_interval: 0,
            ..Default::default()
        })
        .unwrap()
    };
    let server = start();
    let admin = Admin::login(server.addr, &data);

    // A wrong password is refused and nothing is stored.
    let (status, v) = admin.json(
        "POST",
        "sources",
        Some(
            Value::object()
                .with("kind", "subsonic")
                .with("name", "Navi")
                .with("url", url.as_str())
                .with("user", "anna")
                .with("secret", "falsch"),
        ),
    );
    assert_eq!(status, 400, "{v:?}");
    assert!(v.str_or("error", "").contains("password"), "{v:?}");
    assert_eq!(
        admin
            .json("GET", "sources", None)
            .1
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let (status, v) = admin.json(
        "POST",
        "sources",
        Some(
            Value::object()
                .with("kind", "subsonic")
                .with("name", "Navi")
                .with("url", url.as_str())
                .with("user", "anna")
                .with("secret", "geheim"),
        ),
    );
    assert_eq!(status, 201, "{v:?}");
    let source = v.str_or("id", "").to_string();

    // The song appears next to the local one.
    let t = wait("remote song listed", 20, || remote_track(server.addr));
    assert_eq!(
        t.str_or("path", ""),
        "Navi/Far Artist/Far Album/Remote One.wav"
    );
    assert_eq!(t.str_or("album", ""), "Far Album");
    assert!(t.bool_or("hasCover", false));
    let id = t.str_or("id", "").to_string();
    wait("local song too", 20, || {
        tracks(server.addr)
            .iter()
            .any(|t| t.str_or("path", "").ends_with("local.wav"))
            .then_some(())
    });

    // The list for the admin has no secrets.
    let (_, list) = admin.json("GET", "sources", None);
    let s = list.at(0);
    assert_eq!(s.str_or("kind", ""), "subsonic");
    assert_eq!(s.get("songs").as_u64(), Some(1));
    assert!(s.bool_or("hasSecret", false));
    assert!(s.get("secret").is_null());
    assert!(!list.to_json().contains("geheim"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("sources.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "sources.json must be private");
    }
    // The password is stored encrypted.
    let stored = std::fs::read_to_string(data.join("sources.json")).unwrap();
    assert!(
        stored.contains("\"sealed\"") && !stored.contains("geheim"),
        "{stored}"
    );

    // Played through the server, whole and from an offset.
    let base = format!("http://{}/api/tracks/{id}", server.addr);
    let r = get(&format!("{base}/stream"), &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, *song);
    let r = get(
        &format!("{base}/stream"),
        &[("Range".into(), "bytes=1000-1999".into())],
    );
    assert_eq!(r.status, 206);
    assert_eq!(r.body, song[1000..2000]);
    assert_eq!(
        r.headers.get("content-range"),
        Some(format!("bytes 1000-1999/{}", song.len()).as_str())
    );
    // Reported as played once (the ranged request does not count).
    wait("scrobbled", 10, || {
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "scrobble")
            .then_some(())
    });
    let _ = get(&format!("{base}/stream"), &[]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "scrobble")
            .count(),
        1
    );

    // Analysed and with a cover like a local file.
    let r = get(&format!("{base}/analysis"), &[]);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert!(bl_analysis::decode(&r.body).is_some());
    let r = get(&format!("{base}/cover?size=64"), &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("content-type"), Some("image/jpeg"));

    // After a restart the song is there from the start (cached listing).
    let again = start();
    let ids: Vec<String> = tracks(again.addr)
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect();
    assert!(ids.contains(&id), "{ids:?}");

    // Turned off: the songs leave; removed: the source is gone.
    let (status, v) = admin.json(
        "PUT",
        &format!("sources/{source}"),
        Some(
            Value::object()
                .with("kind", "subsonic")
                .with("name", "Navi")
                .with("url", url.as_str())
                .with("user", "anna")
                .with("enabled", false),
        ),
    );
    assert_eq!(status, 200, "{v:?}");
    wait("songs of a turned off source leave", 10, || {
        remote_track(server.addr).is_none().then_some(())
    });
    // The stored password was kept (no secret sent).
    let (status, v) = admin.json(
        "PUT",
        &format!("sources/{source}"),
        Some(
            Value::object()
                .with("kind", "subsonic")
                .with("name", "Navi")
                .with("url", url.as_str())
                .with("user", "anna")
                .with("enabled", true),
        ),
    );
    assert_eq!(status, 200, "{v:?}");
    wait("listed again", 20, || remote_track(server.addr));
    assert_eq!(
        admin.json("DELETE", &format!("sources/{source}"), None).0,
        204
    );
    wait("songs of a removed source leave", 10, || {
        remote_track(server.addr).is_none().then_some(())
    });
    assert_eq!(
        admin.json("DELETE", &format!("sources/{source}"), None).0,
        404
    );
    assert!(!data.join("sources").join(format!("{source}.json")).exists());
    let _ = std::fs::remove_dir_all(&root);
}
