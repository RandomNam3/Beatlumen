//! The health check: `/api/health` without a login, and `--health-check`
//! (containers) fails when the data folder takes no writes (and the
//! database no write transaction, which runs with every check).

use beatlumen_server::Config;

#[test]
fn health_reports_a_data_folder_without_writes() {
    let root = std::env::temp_dir().join(format!("bl-health-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cfg = Config {
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        password: Some("pw".into()),
        rescan_interval: 0,
        auto_labels: false,
        ..Default::default()
    };
    let server = beatlumen_server::start(cfg.clone()).unwrap();
    let base = format!("http://{}", server.addr);
    let get = || {
        bl_net::client::request(
            "GET",
            &format!("{base}/api/health"),
            &[],
            &[],
            &Default::default(),
        )
        .unwrap()
    };
    let check = Config {
        bind: server.addr.to_string(),
        ..cfg
    };
    let r = get();
    assert_eq!(r.status, 200);
    let v = bl_json::parse_bytes(&r.body).unwrap();
    assert_eq!(v.str_or("status", ""), "ok");
    assert_eq!(v.get("problems").as_array().map(Vec::len), Some(0));
    assert!(beatlumen_server::health_check(&check));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let data = root.join("data");
        let mode = std::fs::metadata(&data).unwrap().permissions().mode();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root writes anyway: nothing to check then.
        let blocked = std::fs::write(data.join("probe"), b"x").is_err();
        if blocked {
            let r = get();
            assert_eq!(r.status, 503);
            let v = bl_json::parse_bytes(&r.body).unwrap();
            assert_eq!(v.str_or("status", ""), "failing");
            let problems = v.get("problems").as_array().cloned().unwrap_or_default();
            assert!(problems
                .iter()
                .any(|p| p.as_str().unwrap_or("").contains("data folder")));
            assert!(!beatlumen_server::health_check(&check));
        } else {
            let _ = std::fs::remove_file(data.join("probe"));
        }
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(get().status, 200);
    }
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}
