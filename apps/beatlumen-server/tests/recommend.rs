//! Labels and recommendations of the server: the sound analysis labels songs
//! in the background, people add their own, and radios and suggestions follow
//! what is known. Needs ffmpeg for the tagged test songs (skipped without it).

use beatlumen_server::Config;
use bl_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

/// A tagged MP3: a tone that pulses at `bpm`, loud or quiet.
fn song(dir: &Path, name: &str, artist: &str, genre: &str, bpm: u32, volume: f32) -> bool {
    let beat = bpm as f32 / 60.0;
    let expr = format!("{volume}*sin(2*PI*330*t)*(0.2+0.8*pow(0.5+0.5*cos(2*PI*{beat}*t),8))");
    std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc=exprs='{expr}':s=22050:d=12"))
        .args(["-ac", "1", "-b:a", "48k"])
        .arg("-metadata")
        .arg(format!("title={name}"))
        .arg("-metadata")
        .arg(format!("artist={artist}"))
        .arg("-metadata")
        .arg(format!("album={artist} album"))
        .arg("-metadata")
        .arg(format!("genre={genre}"))
        .arg("-metadata")
        .arg("date=1994")
        .arg(dir.join(format!("{name}.mp3")))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn request(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, bl_json::parse(rest).unwrap_or(Value::Null))
}

#[test]
fn labels_radio_and_suggestions() {
    let root = std::env::temp_dir().join(format!("bl-recommend-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();
    for (name, artist, genre, bpm, volume) in [
        ("Loud 1", "Band A", "Rock", 150, 0.9),
        ("Loud 2", "Band A", "Rock", 150, 0.9),
        ("Loud 3", "Band B", "Rock", 148, 0.85),
        ("Loud 4", "Band B", "Rock", 152, 0.85),
        ("Soft 1", "Duo C", "Ambient", 70, 0.05),
        ("Soft 2", "Duo C", "Ambient", 72, 0.05),
    ] {
        if !song(&music, name, artist, genre, bpm, volume) {
            eprintln!("no ffmpeg: skipped");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
    }
    let server = beatlumen_server::start(Config {
        music_dirs: vec![music],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let addr = server.addr;
    let lib = server.state.library();
    let id_of = |title: &str| {
        lib.tracks()
            .iter()
            .find(|e| e.track.title == title)
            .unwrap()
            .track
            .id
            .clone()
    };

    // The background analysis gets through the songs.
    let t0 = Instant::now();
    loop {
        let s = server.state.sound.summary(&server.state);
        if s.get("done").as_u64() == Some(6) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "{}", s.to_json());
        std::thread::sleep(Duration::from_millis(100));
    }

    // Automatic labels: from the tags and from the sound.
    let (status, labels) = request(addr, "GET", "/api/labels", "");
    assert_eq!(status, 200);
    let names: Vec<String> = labels
        .get("names")
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    let of = |labels: &Value, id: &str| -> Vec<String> {
        labels
            .get("songs")
            .get(id)
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| names[n.as_u64().unwrap() as usize].clone())
            .collect()
    };
    let loud = of(&labels, &id_of("Loud 1"));
    for want in ["genre:rock", "decade:1990", "tempo:fast"] {
        assert!(loud.iter().any(|l| l == want), "{want}: {loud:?}");
    }
    assert!(!loud.iter().any(|l| l == "energy:low"), "{loud:?}");
    let soft = of(&labels, &id_of("Soft 1"));
    for want in ["genre:ambient", "tempo:slow", "energy:low"] {
        assert!(soft.iter().any(|l| l == want), "{want}: {soft:?}");
    }

    // A label given by a person.
    let path = format!("/api/tracks/{}/labels", id_of("Soft 2"));
    let (status, v) = request(addr, "PUT", &path, r#"{"add":["Zum Einschlafen"]}"#);
    assert_eq!(status, 200, "{}", v.to_json());
    let (_, v) = request(addr, "GET", &path, "");
    assert_eq!(v.at(0).as_str(), Some("Zum Einschlafen"));
    let (_, labels) = request(addr, "GET", "/api/labels", "");
    assert_eq!(labels.get("given").at(0).as_str(), Some("Zum Einschlafen"));
    assert_eq!(
        request(addr, "PUT", "/api/tracks/00000000000000ff/labels", "{}").0,
        404
    );

    // A radio stays with what is alike, not repeating what was played.
    let seed = id_of("Loud 1");
    let body = format!(r#"{{"seed":{{"tracks":["{seed}"]}},"recent":["{seed}"],"count":3}}"#);
    let (status, v) = request(addr, "POST", "/api/radio", &body);
    assert_eq!(status, 200, "{}", v.to_json());
    let items = v.get("items").as_array().unwrap();
    assert_eq!(items.len(), 3);
    for i in items {
        let id = i.as_str().unwrap();
        assert_ne!(id, seed);
        assert_ne!(id, id_of("Soft 1"), "rock radio played ambient");
        assert_ne!(id, id_of("Soft 2"), "rock radio played ambient");
    }
    // By label.
    let (status, v) = request(
        addr,
        "POST",
        "/api/radio",
        r#"{"seed":{"label":"Zum Einschlafen"},"count":1}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(
        v.get("items").at(0).as_str(),
        Some(id_of("Soft 2").as_str())
    );
    // Nonsense.
    assert_eq!(request(addr, "POST", "/api/radio", r#"{"seed":{}}"#).0, 400);
    assert_eq!(request(addr, "GET", "/api/radio", "").0, 405);

    // Nobody known yet: something to try out.
    let (status, v) = request(addr, "GET", "/api/recommendations", "");
    assert_eq!(status, 200);
    assert_eq!(v.get("shelves").at(0).str_or("key", ""), "discover");
    assert!(v.get("mixes").as_array().unwrap().is_empty());

    // A favorite and plays change what is suggested.
    let (status, _) = request(
        addr,
        "PUT",
        &format!("/api/favorites/{}", id_of("Soft 1")),
        "",
    );
    assert_eq!(status, 204);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let body = format!(
        r#"{{"events":[{{"id":"{}","t":{},"ms":60000,"skip":false}}]}}"#,
        id_of("Loud 3"),
        now - 3600
    );
    assert_eq!(request(addr, "POST", "/api/history", &body).0, 200);
    let (status, st) = request(addr, "GET", "/api/stats?range=week&utcOffset=3600", "");
    assert_eq!(status, 200);
    assert_eq!(st.get("plays").as_u64(), Some(1));
    assert_eq!(st.get("seconds").as_f64(), Some(60.0));
    assert_eq!(st.get("topSongs").at(0).str_or("title", ""), "Loud 3");
    assert_eq!(st.get("timeline").as_array().unwrap().len(), 7);
    let (_, v) = request(addr, "GET", "/api/recommendations", "");
    let shelves = v.get("shelves").as_array().unwrap();
    let because = shelves
        .iter()
        .find(|s| s.str_or("key", "") == "because")
        .expect("a row for what was played");
    assert_eq!(because.str_or("arg", ""), id_of("Loud 3"));
    let fy = shelves
        .iter()
        .find(|s| s.str_or("key", "") == "for-you")
        .expect("for you");
    assert!(fy
        .get("items")
        .at(0)
        .get("reason")
        .get("key")
        .as_str()
        .is_some());
    let mixes = v.get("mixes").as_array().unwrap();
    assert!(
        mixes.iter().any(|m| m.str_or("arg", "") == "ambient"),
        "{}",
        v.to_json()
    );

    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}

/// Prints what the analysis finds in the songs of a folder, to compare by ear
/// (`BL_FEATURES_DIR=... cargo test -p beatlumen-server --test recommend
/// features_of_a_folder -- --ignored --nocapture`).
#[test]
#[ignore]
fn features_of_a_folder() {
    let Ok(dir) = std::env::var("BL_FEATURES_DIR") else {
        eprintln!("BL_FEATURES_DIR not set");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for path in files {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let src = Box::new(std::io::BufReader::new(file));
        let Ok(mut dec) = bl_media::open(src, Some(&ext)) else {
            continue;
        };
        let spec = dec.spec();
        let mut mono = Vec::new();
        let mut buf = Vec::new();
        while let Ok(more) = dec.next_chunk(&mut buf) {
            bl_analysis::to_mono(&buf, spec.channels as usize, &mut mono);
            buf.clear();
            if !more {
                break;
            }
        }
        let t0 = Instant::now();
        let f = bl_analysis::features::song_features(&mono, spec.sample_rate);
        println!(
            "{:<40} {:?} ({} ms)",
            path.file_name().unwrap().to_string_lossy(),
            f.map(|f| format!(
                "{:.0} BPM beat {:.2} {} conf {:.2} energy {:.2} bright {:.2} dyn {:.2}",
                f.bpm,
                f.beat,
                bl_recommend::key_name(f.key, f.minor),
                f.key_confidence,
                f.energy,
                f.brightness,
                f.dynamics
            )),
            t0.elapsed().as_millis()
        );
    }
}
