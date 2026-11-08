//! Karaoke data in the app: UltraStar import into the server, export,
//! removal, the offline copy, and karaoke data computed on the device
//! (ignored; needs the vocal separation model and a song with a timed
//! text): `BL_SEP_MODEL=vocals-b62c91ce.pth BL_KARAOKE_SONG=song.flac
//! BL_KARAOKE_LRC=song.lrc cargo test --release --test app_karaoke --
//! --ignored`.

mod common;

use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

const ULTRASTAR: &str = "#TITLE:Sine Song\n#ARTIST:Someone\n#BPM:300\n#GAP:500\n\
    : 0 4 60 Hel\n: 4 4 62 lo\n- 10\n: 12 8 64 world\nE\n";

fn karaoke(app: &mut App) -> Value {
    app.state();
    app.cmd(Value::object().with("cmd", "karaoke.get"))
}

/// Ticks until `karaoke.get` is no longer loading.
fn karaoke_loaded(app: &mut App) -> Value {
    let t0 = Instant::now();
    loop {
        let k = karaoke(app);
        if k.str_or("status", "") != "loading" {
            return k;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{}", k.to_json());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn lyrics_ready(app: &mut App) {
    let t0 = Instant::now();
    loop {
        app.state();
        let l = app.cmd(Value::object().with("cmd", "lyrics.get"));
        if l.str_or("status", "") == "ready" {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{}", l.to_json());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn import_export_offline_and_removal() {
    let root = std::env::temp_dir().join(format!("bl-app-karaoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("Sine Song.wav"), common::wav(4)).unwrap();
    std::fs::write(
        music.join("Sine Song.lrc"),
        "[00:00.50]<00:00.50>Hel<00:00.70>lo <00:01.10>world<00:01.80>\n",
    )
    .unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![music.clone()],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let url = format!("http://{}", server.addr);
    let mut app = App::new("karaoke");
    app.connect(&url);
    let id = common::wait_tracks(&mut app, 1)[0]
        .str_or("id", "")
        .to_string();
    app.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![id.clone()]),
    );
    lyrics_ready(&mut app);
    let k = karaoke_loaded(&mut app);
    assert_eq!(k.str_or("status", ""), "none", "{}", k.to_json());
    assert!(k.bool_or("lyricsReady", false));
    // This server does not compute; the device could (the separation
    // model is downloaded when asked).
    assert!(k.bool_or("canCompute", false));

    // Junk is refused, UltraStar goes to the server.
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.import")
            .with("text", "no karaoke here"),
    );
    assert!(r.get("error").as_str().is_some());
    let rev = app.state().get("karaokeRev").as_u64().unwrap();
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.import")
            .with("text", ULTRASTAR),
    );
    assert_eq!(r.get("notes").as_u64(), Some(3), "{}", r.to_json());
    assert!(app.state().get("karaokeRev").as_u64().unwrap() > rev);
    let k = karaoke(&mut app);
    assert_eq!(k.str_or("status", ""), "ready");
    assert_eq!(k.str_or("place", ""), "server");
    assert_eq!(k.str_or("source", ""), "imported");
    let t0 = Instant::now();
    let on_server = loop {
        app.state();
        let r = bl_net::client::request(
            "GET",
            &format!("{url}/api/tracks/{id}/karaoke"),
            &[],
            &[],
            &bl_net::client::Options::default(),
        )
        .unwrap();
        if r.status == 200 || t0.elapsed() > Duration::from_secs(10) {
            break r;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(on_server.status, 200);
    let t = bl_pitch::KaraokeTrack::from_json(&bl_json::parse_bytes(&on_server.body).unwrap())
        .unwrap();
    assert_eq!(t.notes().count(), 3);
    assert_eq!(t.title, "Sine Song");

    // Export: UltraStar text, also as a file.
    let u = app.cmd(Value::object().with("cmd", "karaoke.ultrastar"));
    assert_eq!(u.str_or("name", ""), "Someone - Sine Song.txt");
    let back = bl_pitch::ultrastar::from_ultrastar(u.str_or("text", "")).unwrap();
    assert_eq!(back.notes().count(), 3);
    let out = root.join("export");
    std::fs::create_dir_all(&out).unwrap();
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.export")
            .with("folder", out.to_string_lossy().to_string()),
    );
    let written = std::fs::read_to_string(r.str_or("path", "")).unwrap();
    assert!(written.contains("#TITLE:Sine Song"), "{written}");

    // Kept offline: the container carries the karaoke data.
    app.cmd(
        Value::object()
            .with("cmd", "offline.save")
            .with("ids", vec![id.clone()]),
    );
    app.wait("saved", |s| {
        s.get("offline").get("count").as_u64() == Some(1)
            && s.get("offline")
                .get("saving")
                .as_array()
                .is_some_and(|a| a.is_empty())
    });
    let file = std::fs::read_dir(app.dir.join("offline"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "blo"))
        .expect("container written");
    let h = bl_offline::read_header(&file).unwrap();
    let k = bl_pitch::KaraokeTrack::from_json(h.karaoke.as_ref().expect("karaoke data")).unwrap();
    assert_eq!(k.notes().count(), 3);

    // Removed: from the server and the offline copy.
    let r = app.cmd(Value::object().with("cmd", "karaoke.remove"));
    assert!(r.bool_or("ok", false), "{}", r.to_json());
    assert_eq!(karaoke(&mut app).str_or("status", ""), "none");
    let t0 = Instant::now();
    loop {
        app.state();
        let gone = bl_net::client::request(
            "GET",
            &format!("{url}/api/tracks/{id}/karaoke"),
            &[],
            &[],
            &bl_net::client::Options::default(),
        )
        .unwrap()
        .status
            == 404
            && bl_offline::read_header(&file)
                .map(|h| h.karaoke.is_none())
                .unwrap_or(false);
        if gone {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "not removed");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(app);
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn local_songs_keep_imports_on_the_device() {
    let mut app = App::new("karaoke-local");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    let song = dir.join("Song.wav");
    std::fs::write(&song, common::wav(3)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    assert_eq!(karaoke_loaded(&mut app).str_or("status", ""), "none");
    // Picked as a file after asking for karaoke data.
    let file = app.dir.join("song.txt");
    std::fs::write(&file, ULTRASTAR).unwrap();
    app.cmd(Value::object().with("cmd", "karaoke.expectImport"));
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![file.to_string_lossy().to_string()]),
    );
    let k = karaoke(&mut app);
    assert_eq!(k.str_or("status", ""), "ready", "{}", k.to_json());
    assert_eq!(k.str_or("place", ""), "device");
    // Still there after a restart.
    let mut app = App::with_dir(app.close());
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let k = karaoke_loaded(&mut app);
    assert_eq!(k.str_or("status", ""), "ready", "{}", k.to_json());
    let t = bl_pitch::KaraokeTrack::from_json(k.get("track")).unwrap();
    assert_eq!(t.lines.len(), 2);
}

#[test]
#[ignore]
fn computed_on_the_device() {
    let (Ok(model), Ok(song), Ok(lrc)) = (
        std::env::var("BL_SEP_MODEL"),
        std::env::var("BL_KARAOKE_SONG"),
        std::env::var("BL_KARAOKE_LRC"),
    ) else {
        eprintln!("BL_SEP_MODEL / BL_KARAOKE_SONG / BL_KARAOKE_LRC not set");
        return;
    };
    let mut app = App::new("karaoke-device");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    let ext = std::path::Path::new(&song)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac")
        .to_string();
    let path = dir.join(format!("Song.{ext}"));
    std::fs::copy(&song, &path).unwrap();
    std::fs::copy(&lrc, dir.join("Song.lrc")).unwrap();
    // The model as if downloaded.
    let models = app.dir.join("models").join(bl_sep::MODEL_ID);
    std::fs::create_dir_all(&models).unwrap();
    std::fs::copy(&model, models.join(bl_sep::MODEL_FILE)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![path.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "pause"));
    lyrics_ready(&mut app);
    let k = karaoke_loaded(&mut app);
    assert!(k.bool_or("canCompute", false), "{}", k.to_json());
    let r = app.cmd(Value::object().with("cmd", "karaoke.compute"));
    assert_eq!(r.str_or("place", ""), "device", "{}", r.to_json());
    let t0 = Instant::now();
    let k = loop {
        let k = karaoke(&mut app);
        if k.str_or("status", "") == "ready" && k.get("computing").is_null() {
            break k;
        }
        assert!(k.get("error").is_null(), "{}", k.to_json());
        assert!(t0.elapsed() < Duration::from_secs(900), "too slow");
        std::thread::sleep(Duration::from_millis(100));
    };
    eprintln!("computed on the device in {:.1} s", t0.elapsed().as_secs_f64());
    assert_eq!(k.str_or("source", ""), "computed");
    assert_eq!(k.str_or("place", ""), "device");
    let t = bl_pitch::KaraokeTrack::from_json(k.get("track")).unwrap();
    eprintln!(
        "{} lines, {} notes, key {:?}",
        t.lines.len(),
        t.notes().count(),
        t.key.map(|k| k.name())
    );
    assert!(t.notes().filter(|n| n.kind.has_pitch()).count() > 10);
}
