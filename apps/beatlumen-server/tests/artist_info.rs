//! Artist information on the server: off unless the admin switches it on, and
//! then only what the switch allows. The worker's requests to the services are
//! tested in `src/artists.rs`; this asks through the API, and with the switch on
//! nothing may be taken from the real services: the name asked for has an entry
//! already, from `artists.json`.

use beatlumen_server::Config;
use bl_json::Value;

fn get(addr: std::net::SocketAddr, path: &str) -> (u16, Value) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, bl_json::parse(rest).unwrap_or(Value::Null))
}

fn server(
    name: &str,
    online: bool,
    known: Option<&str>,
) -> (beatlumen_server::Server, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("bl-artist-info-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    if let Some(json) = known {
        std::fs::write(data.join("artists.json"), json).unwrap();
    }
    let server = beatlumen_server::start(Config {
        data_dir: data,
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        online_metadata: online,
        ..Default::default()
    })
    .unwrap();
    (server, root)
}

#[test]
fn off_by_default_and_nothing_is_said() {
    assert!(
        !Config::default().online_metadata,
        "off until the admin agrees"
    );
    let (server, root) = server("off", false, None);
    let (status, v) = get(server.addr, "/api/artist-info?name=Some%20Band&lang=de");
    assert_eq!(status, 200, "{}", v.to_json());
    assert_eq!(v.get("enabled").as_bool(), Some(false));
    assert!(v.get("artist").is_null());
    // Nothing was looked up.
    assert_eq!(
        server.state.artists.summary().get("known").as_u64(),
        Some(0)
    );
    assert!(!root.join("data/artists.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn with_the_switch_on_a_known_artist_is_served() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let known = format!(
        r#"[{{"key":"some band","fetched":{now},"artist":{{"mbid":"a74b1b7f-71a5-4011-9441-d0b5e4122711","name":"Some Band","genres":["rock"],"complete":true}}}}]"#
    );
    let (server, root) = server("on", true, Some(&known));
    let (status, v) = get(server.addr, "/api/artist-info?name=some%20band&lang=de");
    assert_eq!(status, 200);
    assert_eq!(v.get("enabled").as_bool(), Some(true));
    assert_eq!(v.str_or("state", ""), "ready");
    assert_eq!(v.get("artist").str_or("name", ""), "Some Band");
    assert_eq!(server.state.artists.genres("Some Band"), ["rock"]);
    // Live: the switch can be turned off without a restart.
    server.state.artists.set_enabled(false);
    let (_, v) = get(server.addr, "/api/artist-info?name=some%20band&lang=de");
    assert_eq!(v.get("enabled").as_bool(), Some(false));
    assert!(server.state.artists.genres("Some Band").is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
