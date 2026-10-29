//! The server checks its web client against the integrity manifest and
//! refuses modified or added files.

mod common;

use beatlumen_server::Config;
use bl_json::Value;
use bl_net::client;

#[test]
fn modified_web_files_are_not_served() {
    let root = std::env::temp_dir().join(format!("bl-webint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let web = root.join("web");
    std::fs::create_dir_all(web.join("sub")).unwrap();
    std::fs::write(web.join("index.html"), "<html>ok</html>").unwrap();
    std::fs::write(web.join("app.js"), "console.log(1)").unwrap();
    std::fs::write(web.join("sub/x.css"), "body{}").unwrap();
    let msg = beatlumen_server::release::write_manifest(&web, "test").unwrap();
    assert!(msg.contains("3 files"), "{msg}");

    let server = beatlumen_server::start(Config {
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        web_dir: Some(web.clone()),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    let get = |p: &str| {
        client::request(
            "GET",
            &format!("http://{}{p}", server.addr),
            &[],
            &[],
            &client::Options::default(),
        )
        .unwrap()
    };
    assert_eq!(get("/").status, 200);
    assert_eq!(get("/app.js").status, 200);
    assert_eq!(get("/sub/x.css").status, 200);
    assert_eq!(get("/manifest.json").status, 200);
    let info = bl_json::parse_bytes(&get("/api/info").body).unwrap();
    assert_eq!(info.str_or("webIntegrity", ""), "unsigned");

    // The app checks the web client from outside.
    let mut app = common::App::new("webcheck");
    app.connect(&format!("http://{}", server.addr));
    assert_eq!(app.state().str_or("serverWebIntegrity", ""), "unsigned");
    app.cmd(Value::object().with("cmd", "server.verifyWeb"));
    let st = app.wait("web check", |s| {
        !matches!(s.get("webCheck").str_or("state", ""), "running" | "idle")
    });
    // Test builds have no release key: the files match, unsigned.
    assert_eq!(
        st.get("webCheck").str_or("state", ""),
        "unsigned",
        "{}",
        st.get("webCheck").to_json()
    );

    // Changed after the start: refused from then on.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(web.join("app.js"), "steal(keys)").unwrap();
    assert_eq!(get("/app.js").status, 503);
    app.cmd(Value::object().with("cmd", "server.verifyWeb"));
    let st = app.wait("web check", |s| {
        !matches!(s.get("webCheck").str_or("state", ""), "running" | "idle")
    });
    assert_eq!(st.get("webCheck").str_or("state", ""), "failed");
    // Added file: never served.
    std::fs::write(web.join("evil.js"), "steal(keys)").unwrap();
    assert_eq!(get("/evil.js").status, 503);
    // A compressed copy must match too.
    std::fs::write(web.join("app.js"), "console.log(1)").unwrap();
    assert_eq!(get("/app.js").status, 200, "restored file is served again");
    let _ = std::fs::remove_dir_all(&root);
}
