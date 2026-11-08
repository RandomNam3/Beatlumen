//! Hostile clients: bodies without a login are refused before they are
//! read, forwarded addresses count only from trusted proxies, the health
//! check gets through a full server, the device relay keeps its limits and
//! background threads start again after a panic.

use beatlumen_server::{Config, Server};
use bl_net::client;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn start(name: &str, password: Option<&str>, edit: impl FnOnce(&mut Config)) -> (Server, PathBuf) {
    let root = std::env::temp_dir().join(format!("bl-hardening-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("music")).unwrap();
    let mut cfg = Config {
        music_dirs: vec![root.join("music")],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        password: password.map(str::to_string),
        rescan_interval: 0,
        auto_labels: false,
        ..Default::default()
    };
    edit(&mut cfg);
    let server = beatlumen_server::start(cfg).unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    (server, root)
}

/// Sends `data` and reads the answer until the server closes.
fn raw(addr: SocketAddr, data: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(data);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn status(resp: &str) -> u16 {
    resp.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn post(path: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[test]
fn bodies_without_a_login_are_refused_before_they_are_read() {
    let (s, root) = start("bodies", Some("a password for tests"), |_| {});
    let a = s.addr;
    // Only the head of a 200 MB upload: refused at once, nothing read.
    let t0 = Instant::now();
    let r = raw(
        a,
        b"POST /api/images HTTP/1.1\r\nHost: localhost\r\nX-Filename: big.png\r\nContent-Length: 200000000\r\n\r\n",
    );
    assert_eq!(status(&r), 401, "{r}");
    assert!(r.contains("WWW-Authenticate"), "{r}");
    assert!(t0.elapsed() < Duration::from_secs(3), "waited for the body");
    // Also on routes that need no login, beyond the small limit.
    let r = raw(
        a,
        b"POST /api/login HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n\r\n",
    );
    assert_eq!(status(&r), 401, "{r}");
    // A chunked body without a login ends at the small limit.
    let mut chunked = b"POST /api/vault/login HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    for _ in 0..3 {
        chunked.extend_from_slice(format!("{:x}\r\n", 30_000).as_bytes());
        chunked.extend(std::iter::repeat_n(b'x', 30_000));
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    assert_eq!(status(&raw(a, &chunked)), 413);
    // Small bodies without a login still reach their handler.
    let r = raw(a, &post("/api/login", "", r#"{"password":"wrong"}"#));
    assert_eq!(status(&r), 401, "{r}");
    assert!(r.contains("wrong password"), "{r}");

    // With a login, large uploads are taken (100 Continue) ...
    let opts = client::Options::default();
    let login = client::request(
        "POST",
        &format!("http://{a}/api/login"),
        &[],
        br#"{"password":"a password for tests"}"#,
        &opts,
    )
    .unwrap();
    let token = bl_json::parse_bytes(&login.body)
        .unwrap()
        .str_or("token", "")
        .to_string();
    assert!(!token.is_empty());
    let mut st = TcpStream::connect(a).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    st.write_all(
        format!(
            "POST /api/images HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nX-Filename: big.png\r\nContent-Length: 10000000\r\nExpect: 100-continue\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut cont = [0u8; 25];
    st.read_exact(&mut cont).unwrap();
    assert_eq!(&cont, b"HTTP/1.1 100 Continue\r\n\r\n");
    drop(st);
    // ... and the general limit stays for everything else.
    let auth = format!("Authorization: Bearer {token}\r\n");
    let r = raw(
        a,
        format!("POST /api/playlists HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: 5000000\r\n\r\n")
            .as_bytes(),
    );
    assert_eq!(status(&r), 413, "{r}");
    // A token that is not one counts as no login.
    let r = raw(
        a,
        b"POST /api/images HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer 0123456789abcdef\r\nContent-Length: 200000000\r\n\r\n",
    );
    assert_eq!(status(&r), 401, "{r}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Joins with a wrong code as `client` (sent by the test as a proxy would).
fn wrong_join(a: SocketAddr, client: &str) -> u16 {
    let body = r#"{"code":"WRONGCOD","name":"x","device":"device-test"}"#;
    status(&raw(
        a,
        &post(
            "/api/rooms/join",
            &format!("X-Forwarded-For: {client}\r\n"),
            body,
        ),
    ))
}

#[test]
fn forwarded_addresses_count_only_from_trusted_proxies() {
    // Loopback is a trusted proxy by default: limits count per forwarded
    // client.
    let (s, root) = start("proxy-trusted", None, |_| {});
    for _ in 0..20 {
        assert_eq!(wrong_join(s.addr, "203.0.113.1"), 404);
    }
    assert_eq!(wrong_join(s.addr, "203.0.113.1"), 429);
    assert_eq!(wrong_join(s.addr, "203.0.113.2"), 404, "another client");
    // IPv6 clients count per /64.
    for _ in 0..20 {
        assert_eq!(wrong_join(s.addr, "2001:db8:1:2::1"), 404);
    }
    assert_eq!(wrong_join(s.addr, "2001:db8:1:2:ffff::9"), 429);
    let _ = std::fs::remove_dir_all(&root);

    // Without trusted proxies the header names nobody: one client.
    let (s, root) = start("proxy-none", None, |c| c.trusted_proxies.clear());
    for i in 0..20 {
        assert_eq!(wrong_join(s.addr, &format!("203.0.113.{i}")), 404);
    }
    assert_eq!(wrong_join(s.addr, "198.51.100.77"), 429);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_health_check_gets_through_a_full_server() {
    // Loopback counts like any client: 32 connections at most.
    let (s, root) = start("probe", None, |c| c.trusted_proxies.clear());
    let held: Vec<TcpStream> = (0..32)
        .map(|_| TcpStream::connect(s.addr).unwrap())
        .collect();
    let t0 = Instant::now();
    loop {
        let r = raw(
            s.addr,
            b"GET /api/time HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        if status(&r) == 503 {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "not full: {r}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let cfg = Config {
        bind: s.addr.to_string(),
        ..Default::default()
    };
    assert!(beatlumen_server::health_check(&cfg));
    for _ in 0..10 {
        assert!(
            beatlumen_server::health_check(&cfg),
            "probes are given back"
        );
    }
    // Other requests from there wait for a free place.
    let r = raw(
        s.addr,
        b"GET /api/library HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert_eq!(status(&r), 503, "{r}");
    drop(held);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn device_commands_and_their_queue_are_limited() {
    let (s, root) = start("devices", None, |_| {});
    let base = format!("http://{}", s.addr);
    let opts = client::Options::default();
    let send = |m: &str, path: &str, body: &str| -> u16 {
        client::request(m, &format!("{base}{path}"), &[], body.as_bytes(), &opts)
            .unwrap()
            .status
    };
    assert_eq!(
        send("PUT", "/api/devices/device-aaaa", r#"{"name":"A"}"#),
        200
    );
    assert_eq!(
        send("PUT", "/api/devices/device-bbbb", r#"{"name":"B"}"#),
        200
    );
    let cmd = r#"{"from":"device-bbbb","command":{"cmd":"pause"}}"#;
    // Too large: refused (it used to be relayed up to the 4 MiB body limit).
    let big = format!(
        r#"{{"from":"device-bbbb","command":{{"cmd":"queue.set","items":"{}"}}}}"#,
        "x".repeat(20_000)
    );
    assert_eq!(send("POST", "/api/devices/device-aaaa/command", &big), 413);
    // A device that does not fetch its commands gets at most 64 waiting.
    for i in 0..64 {
        assert_eq!(
            send("POST", "/api/devices/device-aaaa/command", cmd),
            202,
            "{i}"
        );
    }
    assert_eq!(send("POST", "/api/devices/device-aaaa/command", cmd), 429);
    // The other device is not affected.
    assert_eq!(send("POST", "/api/devices/device-bbbb/command", cmd), 202);
    // Once it fetched them, there is room again.
    let r = client::request(
        "GET",
        &format!("{base}/api/events?device=device-aaaa&since=0"),
        &[],
        &[],
        &opts,
    )
    .unwrap();
    let v = bl_json::parse_bytes(&r.body).unwrap();
    let seq = v.get("seq").as_u64().unwrap();
    let events = v.get("events").as_array().map_or(0, |a| a.len());
    assert!(events >= 64, "{events} events");
    let next = format!("{base}/api/events?device=device-aaaa&since={seq}");
    let poll = std::thread::spawn(move || {
        client::request("GET", &next, &[], &[], &client::Options::default()).map(|r| r.status)
    });
    let t0 = Instant::now();
    loop {
        let st = send("POST", "/api/devices/device-aaaa/command", cmd);
        if st == 202 {
            break;
        }
        assert_eq!(st, 429);
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "the queue stayed full"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(poll.join().unwrap().unwrap(), 200);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn supervised_threads_start_again_after_a_panic() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let t = beatlumen_server::util::supervise_with(
        "test-worker",
        Duration::from_millis(5),
        Duration::from_millis(20),
        move || {
            // Panics twice, then ends normally (and is not started again).
            if r.fetch_add(1, Ordering::SeqCst) < 2 {
                panic!("the worker broke (on purpose)");
            }
        },
    )
    .unwrap();
    t.join().unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}
