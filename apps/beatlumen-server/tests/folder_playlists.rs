//! Playlist files in the music folders appear as read-only playlists;
//! their entries resolve only to songs of the library.

mod common;

use beatlumen_server::Config;
use bl_net::client;

#[test]
fn playlist_files_in_music_folders() {
    let root = std::env::temp_dir().join(format!("bl-folder-playlists-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(music.join("sub")).unwrap();
    std::fs::write(music.join("a.wav"), common::wav(1)).unwrap();
    std::fs::write(music.join("sub").join("b.wav"), common::wav(1)).unwrap();
    std::fs::write(root.join("outside.wav"), common::wav(1)).unwrap();
    std::fs::write(
        music.join("Lieblinge.m3u8"),
        "#EXTM3U\nsub/b.wav\na.wav\n../outside.wav\n/etc/passwd\nhttp://example.org/x.mp3\n",
    )
    .unwrap();
    let server = beatlumen_server::start(Config {
        data_dir: root.join("data"),
        music_dirs: vec![music.clone()],
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(std::time::Duration::from_secs(30)));
    let get = |path: &str| {
        let r = client::request(
            "GET",
            &format!("http://{}{path}", server.addr),
            &[],
            &[],
            &client::Options::default(),
        )
        .unwrap();
        bl_json::parse_bytes(&r.body).unwrap()
    };
    let lists = get("/api/playlists");
    let p = lists
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p.str_or("name", "") == "Lieblinge")
        .unwrap_or_else(|| panic!("{lists:?}"))
        .clone();
    assert!(p.bool_or("folder", false));
    let items: Vec<String> = p
        .get("items")
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_string())
        .collect();
    let lib = get("/api/library");
    let path_of = |id: &str| {
        lib.get("tracks")
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.str_or("id", "") == id)
            .map(|t| t.str_or("path", "").to_string())
            .unwrap()
    };
    let paths: Vec<String> = items.iter().map(|i| path_of(i)).collect();
    assert_eq!(paths, ["sub/b.wav", "a.wav"]);
    // Read-only.
    let r = client::request(
        "DELETE",
        &format!(
            "http://{}/api/playlists/{}",
            server.addr,
            p.str_or("id", "")
        ),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(r.status, 403);
    let _ = std::fs::remove_dir_all(&root);
}
