//! Things shared with the app: a link or files wait for the user's choice
//! (play now, queue, keep), and nothing plays before.

mod common;

use bl_json::Value;
use common::App;

#[test]
fn shared_files_and_links() {
    let mut app = App::new("share");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("geteilt.wav");
    std::fs::write(&song, common::wav(5)).unwrap();
    app.state();

    app.cmd(
        Value::object()
            .with("cmd", "share.receive")
            .with("text", "Hör mal: https://radio.example.org/live.mp3 !"),
    );
    let s = app.state();
    assert_eq!(
        s.get("share").str_or("link", ""),
        "https://radio.example.org/live.mp3"
    );
    app.cmd(Value::object().with("cmd", "share.dismiss"));
    assert!(app.state().get("share").is_null());

    app.cmd(
        Value::object()
            .with("cmd", "share.receive")
            .with("paths", vec![song.to_string_lossy().to_string()]),
    );
    let s = app.state();
    assert_eq!(
        s.get("share").get("files").at(0).as_str(),
        Some("geteilt.wav")
    );
    assert!(
        !s.get("share").bool_or("canKeep", true),
        "no account: cannot keep"
    );
    assert_ne!(s.str_or("play", ""), "playing");
    app.cmd(Value::object().with("cmd", "share.play"));
    let s = app.wait("playing", |s| s.str_or("play", "") == "playing");
    assert!(s.get("share").is_null());
    assert_eq!(s.get("current").str_or("title", ""), "geteilt");
}
