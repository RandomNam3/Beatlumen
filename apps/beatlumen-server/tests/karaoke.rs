//! Karaoke data on the server: import, removal, stale computed data, and
//! computing it from a real song (ignored; needs the vocal separation
//! model and a song with a timed text):
//! `BL_SEP_MODEL=vocals-b62c91ce.pth BL_KARAOKE_SONG=song.flac
//! BL_KARAOKE_LRC=song.lrc cargo test --release --test karaoke -- --ignored`.

mod common;

use beatlumen_server::Config;
use bl_json::Value;
use bl_net::client;
use std::time::{Duration, Instant};

struct Server {
    _server: beatlumen_server::Server,
    base: String,
    root: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Server {
    fn req(&self, m: &str, p: &str, body: &[u8]) -> client::Response {
        client::request(
            m,
            &format!("{}{p}", self.base),
            &[],
            body,
            &client::Options::default(),
        )
        .unwrap()
    }

    fn json(&self, m: &str, p: &str) -> Value {
        bl_json::parse_bytes(&self.req(m, p, &[]).body).unwrap()
    }

    fn only_id(&self) -> String {
        self.json("GET", "/api/library")
            .get("tracks")
            .as_array()
            .unwrap()[0]
            .str_or("id", "")
            .to_string()
    }
}

fn start(
    name: &str,
    files: &[(&str, &[u8])],
    cfg: impl FnOnce(&mut Config, &std::path::Path),
) -> Server {
    let root = std::env::temp_dir().join(format!("bl-karaoke-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    for (n, b) in files {
        std::fs::write(music.join(n), b).unwrap();
    }
    let mut config = Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    };
    cfg(&mut config, &root);
    let server = beatlumen_server::start(config).unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let base = format!("http://{}", server.addr);
    Server {
        _server: server,
        base,
        root,
    }
}

const ULTRASTAR: &str = "#TITLE:Song\n#ARTIST:Someone\n#BPM:300\n#GAP:500\n\
    : 0 4 60 Hel\n: 4 4 62 lo\n- 10\n: 12 8 64 world\nE\n";

#[test]
fn import_export_and_removal() {
    let wav = common::wav(2);
    let s = start("import", &[("Song.wav", &wav)], |_, _| {});
    let id = s.only_id();
    let path = format!("/api/tracks/{id}/karaoke");
    assert!(!s.json("GET", "/api/info").bool_or("karaoke", true));
    assert_eq!(s.req("GET", &path, &[]).status, 404);
    // Not computed on this server.
    assert_eq!(s.req("POST", &format!("{path}/compute"), &[]).status, 501);
    // Junk is refused.
    assert_eq!(s.req("PUT", &path, b"#TITLE:x\nE\n").status, 400);

    assert_eq!(s.req("PUT", &path, ULTRASTAR.as_bytes()).status, 204);
    let v = s.json("GET", &path);
    assert_eq!(v.str_or("format", ""), "beatlumen-karaoke");
    assert_eq!(v.str_or("source", ""), "imported");
    assert_eq!(v.str_or("title", ""), "Song");
    let t = bl_pitch::KaraokeTrack::from_json(&v).unwrap();
    assert_eq!(t.lines.len(), 2);
    assert_eq!(t.notes().count(), 3);
    // 300 BPM in UltraStar quarter steps: 50 ms a step after the 500 ms gap.
    assert_eq!(t.lines[0].notes[1].start_ms, 700);
    // The JSON form imports too.
    let json = t.to_json().to_json();
    assert_eq!(s.req("PUT", &path, json.as_bytes()).status, 204);
    assert_eq!(
        bl_pitch::KaraokeTrack::from_json(&s.json("GET", &path)).unwrap(),
        t
    );

    assert_eq!(s.req("DELETE", &path, &[]).status, 204);
    assert_eq!(s.req("GET", &path, &[]).status, 404);
    assert_eq!(
        s.req("GET", "/api/tracks/0123456789abcdef/karaoke", &[])
            .status,
        404
    );
}

#[test]
fn computed_data_goes_stale_with_the_song_text() {
    let wav = common::wav(2);
    let lrc = "[00:00.50]<00:00.50>Hel<00:00.70>lo <00:01.10>world<00:01.80>\n";
    let s = start(
        "stale",
        &[("Song.wav", &wav), ("Song.lrc", lrc.as_bytes())],
        |_, _| {},
    );
    let id = s.only_id();
    let path = format!("/api/tracks/{id}/karaoke");
    // As the background job stores it.
    let track = bl_pitch::KaraokeTrack::from_json(
        &bl_pitch::KaraokeTrack {
            title: "Song".into(),
            lines: vec![bl_pitch::KaraokeLine {
                voice: 0,
                notes: vec![bl_pitch::Note {
                    start_ms: 500,
                    dur_ms: 200,
                    pitch: 60,
                    kind: bl_pitch::NoteKind::Normal,
                    text: "Hel".into(),
                }],
            }],
            ..Default::default()
        }
        .to_json(),
    )
    .unwrap();
    let dir = s.root.join("data").join("karaoke");
    std::fs::create_dir_all(&dir).unwrap();
    let stored = bl_karaoke::to_stored(&track, bl_karaoke::Source::Computed, Some(lrc));
    std::fs::write(dir.join(format!("{id}.json")), stored.to_json()).unwrap();
    let v = s.json("GET", &path);
    assert_eq!(v.str_or("source", ""), "computed");

    // A new song text: the computed notes no longer fit.
    let other = "[00:00.50]Something else\n";
    assert_eq!(
        s.req("PUT", &format!("/api/tracks/{id}/lyrics"), other.as_bytes())
            .status,
        204
    );
    assert_eq!(s.req("GET", &path, &[]).status, 404);
    // An import wins over it either way.
    assert_eq!(s.req("PUT", &path, ULTRASTAR.as_bytes()).status, 204);
    assert_eq!(s.json("GET", &path).str_or("source", ""), "imported");
}

#[test]
#[ignore]
fn computes_from_a_real_song() {
    let (Ok(model), Ok(song), Ok(lrc)) = (
        std::env::var("BL_SEP_MODEL"),
        std::env::var("BL_KARAOKE_SONG"),
        std::env::var("BL_KARAOKE_LRC"),
    ) else {
        eprintln!("BL_SEP_MODEL / BL_KARAOKE_SONG / BL_KARAOKE_LRC not set");
        return;
    };
    let ext = std::path::Path::new(&song)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac")
        .to_string();
    let audio = std::fs::read(&song).unwrap();
    let text = std::fs::read(&lrc).unwrap();
    let name = format!("Song.{ext}");
    let s = start(
        "real",
        &[(&name, &audio), ("Song.lrc", &text)],
        |c, root| {
            c.karaoke = true;
            // The model as if downloaded.
            let models = root.join("data").join("models").join(bl_sep::MODEL_ID);
            std::fs::create_dir_all(&models).unwrap();
            std::fs::copy(&model, models.join(bl_sep::MODEL_FILE)).unwrap();
        },
    );
    assert!(s.json("GET", "/api/info").bool_or("karaoke", false));
    let id = s.only_id();
    let path = format!("/api/tracks/{id}/karaoke");
    assert_eq!(s.req("POST", &format!("{path}/compute"), &[]).status, 202);
    let t0 = Instant::now();
    loop {
        let st = s.json("GET", &format!("{path}/compute"));
        match st.str_or("state", "") {
            "done" => break,
            "failed" => panic!("{}", st.to_json()),
            _ => {}
        }
        assert!(t0.elapsed() < Duration::from_secs(600), "too slow");
        std::thread::sleep(Duration::from_millis(250));
    }
    eprintln!("computed in {:.1} s", t0.elapsed().as_secs_f64());
    let v = s.json("GET", &path);
    assert_eq!(v.str_or("source", ""), "computed");
    let t = bl_pitch::KaraokeTrack::from_json(&v).unwrap();
    let pitched = t.notes().filter(|n| n.kind.has_pitch()).count();
    eprintln!(
        "{} lines, {} notes ({pitched} with a pitch), key {:?}, range {:?}",
        t.lines.len(),
        t.notes().count(),
        t.key.map(|k| k.name()),
        t.range()
    );
    assert!(pitched > 10);
}
