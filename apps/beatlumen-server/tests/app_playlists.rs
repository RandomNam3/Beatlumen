//! Playlist files in the app: an M3U opens as a playlist (entries outside
//! the music folders and the playlist's folder are left out), a station
//! list becomes radio stations, and a playlist exports as M3U8.

mod common;

use bl_json::Value;
use common::App;

#[test]
fn playlist_files() {
    let mut app = App::new("playlists");
    let base = app.dir.join("files");
    let lists = base.join("lists");
    std::fs::create_dir_all(lists.join("sub")).unwrap();
    std::fs::write(lists.join("a.wav"), common::wav(1)).unwrap();
    std::fs::write(lists.join("sub").join("b.wav"), common::wav(1)).unwrap();
    // Outside the playlist's folder and no music folder: not taken.
    std::fs::write(base.join("secret.wav"), common::wav(1)).unwrap();
    std::fs::write(
        lists.join("Mix.m3u"),
        "#EXTM3U\n#EXTINF:1,Eins\na.wav\nsub\\b.wav\n../secret.wav\n/etc/passwd\n",
    )
    .unwrap();
    app.state();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with(
                "paths",
                vec![lists.join("Mix.m3u").to_string_lossy().to_string()],
            )
            .with("play", false),
    );
    let lists_json = app.cmd(Value::object().with("cmd", "playlists.list"));
    let pl = lists_json
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.str_or("name", "") == "Mix")
        .unwrap_or_else(|| panic!("no playlist: {lists_json:?}"));
    let items = app.cmd(
        Value::object()
            .with("cmd", "playlist.items")
            .with("id", pl.str_or("id", "")),
    );
    let ids: Vec<String> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(
        ids[0].ends_with("lists/a.wav") && ids[1].ends_with("sub/b.wav"),
        "{ids:?}"
    );

    // Export.
    let out = app.dir.join("export");
    std::fs::create_dir_all(&out).unwrap();
    let r = app.cmd(
        Value::object()
            .with("cmd", "playlist.export")
            .with("id", pl.str_or("id", ""))
            .with("folder", out.to_string_lossy().to_string()),
    );
    let text = std::fs::read_to_string(r.str_or("path", "")).unwrap();
    assert!(text.starts_with("#EXTM3U"), "{text}");
    assert!(
        text.contains("a.wav") && text.contains("b.wav") && !text.contains("secret"),
        "{text}"
    );

    // A station list.
    std::fs::write(
        lists.join("Sender.pls"),
        "[playlist]\nFile1=http://radio.example.org/live\nTitle1=Beispielradio\nNumberOfEntries=1\n",
    )
    .unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with(
                "paths",
                vec![lists.join("Sender.pls").to_string_lossy().to_string()],
            )
            .with("play", false),
    );
    let s = app.state();
    let st = s.get("sources").get("stations").at(0).clone();
    assert_eq!(st.str_or("name", ""), "Beispielradio");
    assert_eq!(st.str_or("url", ""), "http://radio.example.org/live");
}
