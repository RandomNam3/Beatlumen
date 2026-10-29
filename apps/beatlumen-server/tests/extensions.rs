//! Source extensions: the server lists a (fake) extension, proxies its
//! search and pictures, imports a song while it streams it (WebM/Opus with
//! a part left out becomes Ogg/Opus with tags and cover) and adds it to the
//! library; temporary imports stay out of the library.

mod common;

use bl_json::Value;
use bl_net::client;
use common::fake_extension::{fake_extension, media, TOKEN};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn get(base: &str, path: &str) -> client::Response {
    client::request(
        "GET",
        &format!("{base}{path}"),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap()
}

fn post(base: &str, path: &str, body: &Value) -> client::Response {
    client::request(
        "POST",
        &format!("{base}{path}"),
        &[("Content-Type".into(), "application/json".into())],
        body.to_json().as_bytes(),
        &client::Options::default(),
    )
    .unwrap()
}

fn json(r: &client::Response) -> Value {
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    bl_json::parse_bytes(&r.body).unwrap()
}

#[test]
fn extension_search_import_and_stream() {
    let dir: PathBuf = std::env::temp_dir().join(format!("bl-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some((webm, jpeg)) = media(&dir) else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let audio_requests = Arc::new(AtomicUsize::new(0));
    let ext = fake_extension(webm, jpeg, audio_requests.clone());
    let token = dir.join("token");
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
    let music = dir.join("music");
    std::fs::create_dir_all(&music).unwrap();

    let data = dir.join("data");
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        music_dirs: vec![music],
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        extensions: vec![
            ("fake".into(), ext.clone()),
            ("down".into(), "http://127.0.0.1:9".into()),
        ],
        extension_token_file: Some(token.clone()),
        ..Default::default()
    })
    .unwrap();
    let base = format!("http://{}", server.addr);
    assert!(server.wait_for_scan(Duration::from_secs(10)));

    // Only extensions that answer are listed.
    let list = json(&get(&base, "/api/extensions"));
    let exts = list.get("extensions").as_array().unwrap();
    assert_eq!(exts.len(), 1, "{}", list.to_json());
    assert_eq!(exts[0].str_or("name", ""), "Fake Tube");
    assert!(exts[0].bool_or("search", false));

    // Search: proxied, unsafe ids dropped, pictures through the server.
    let found = json(&get(
        &base,
        "/api/extensions/fake/search?q=fremdes%20lied%20%26%20co",
    ));
    let items = found.get("items").as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", found.to_json());
    assert_eq!(items[0].str_or("title", ""), "Fremdes Lied");
    assert_eq!(items[0].str_or("detail", ""), "Lyric video");
    assert!(items[0].get("track").is_null());
    let image = items[0].str_or("image", "").to_string();
    assert_eq!(image, "/api/extensions/fake/image/song:1");
    let pic = get(&base, &format!("{image}?size=64"));
    assert_eq!(pic.status, 200);
    assert_eq!(pic.headers.get("content-type"), Some("image/jpeg"));
    assert!(pic.body.starts_with(&[0xff, 0xd8]));

    // Import into the library; play it while it is written.
    let t0 = Instant::now();
    let job = json(&post(
        &base,
        "/api/extensions/fake/import",
        &Value::object().with("id", "song:1"),
    ));
    assert_eq!(job.str_or("title", ""), "Fremdes Lied");
    assert_eq!(job.f64_or("duration", 0.0), 8.0);
    let audio = format!("{base}{}", job.str_or("audio", ""));
    let mut stream = client::HttpStream::open(&audio, &[]).unwrap();
    assert_eq!(stream.len(), u64::MAX, "length unknown while importing");
    let mut first = vec![0u8; 4096];
    stream.read_exact(&mut first).unwrap();
    assert!(first.starts_with(b"OggS"));
    assert!(
        t0.elapsed() < Duration::from_secs(3),
        "playable early: {:?}",
        t0.elapsed()
    );
    let still = json(&get(
        &base,
        &format!("/api/extensions/jobs/{}", job.str_or("job", "")),
    ));
    assert_eq!(still.str_or("state", ""), "running");
    // The same song again joins the running import.
    let again = json(&post(
        &base,
        "/api/extensions/fake/import",
        &Value::object().with("id", "song:1"),
    ));
    assert_eq!(again.str_or("job", ""), job.str_or("job", ""));

    // Decoding the stream as it comes gives the song without the intro.
    let mut dec = bl_media::open(Box::new(stream), Some("opus")).unwrap();
    let mut pcm = Vec::new();
    while dec.next_chunk(&mut pcm).unwrap() {}
    let secs = pcm.len() as f64 / 2.0 / 48_000.0;
    assert!((secs - 8.0).abs() < 0.1, "{secs} s");
    let loud = pcm[..9600].iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(
        loud > 0.05,
        "starts with the tone, not the silence ({loud})"
    );
    assert_eq!(audio_requests.load(Ordering::Relaxed), 1);

    // Done: in the library with tags and cover.
    let job_url = format!("/api/extensions/jobs/{}", job.str_or("job", ""));
    let t0 = Instant::now();
    let done = loop {
        let s = json(&get(&base, &job_url));
        if !s.get("track").is_null() {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "{}", s.to_json());
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(done.str_or("state", ""), "done");
    let track = done.str_or("track", "").to_string();
    let lib = json(&get(&base, "/api/library"));
    let t = lib
        .get("tracks")
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t.str_or("id", "") == track)
        .unwrap()
        .clone();
    assert_eq!(t.str_or("title", ""), "Fremdes Lied");
    assert!(t.bool_or("hasCover", false), "{}", t.to_json());
    let file = data.join("extensions/library/fake/Fremde Band - Fremdes Lied.opus");
    assert!(file.is_file());
    // The finished stream has a length (seekable).
    let s = client::HttpStream::open(&audio, &[]).unwrap();
    assert_eq!(s.len(), std::fs::metadata(&file).unwrap().len());

    // Known now: search and import point to the library track.
    let found = json(&get(
        &base,
        "/api/extensions/fake/search?q=fremdes%20lied%20%26%20co",
    ));
    assert_eq!(
        found.get("items").as_array().unwrap()[0].str_or("track", ""),
        track
    );
    let known = json(&post(
        &base,
        "/api/extensions/fake/import",
        &Value::object().with("id", "song:1"),
    ));
    assert_eq!(known.str_or("track", ""), track);
    assert_eq!(audio_requests.load(Ordering::Relaxed), 1);

    // Temporary (for the user's encrypted files): not in the library.
    let tmp = json(&post(
        &base,
        "/api/extensions/fake/import",
        &Value::object().with("id", "song:1").with("keep", false),
    ));
    let tmp_audio = format!("{base}{}", tmp.str_or("audio", ""));
    let mut body = Vec::new();
    client::HttpStream::open(&tmp_audio, &[])
        .unwrap()
        .read_to_end(&mut body)
        .unwrap();
    assert!(body.starts_with(b"OggS") && body.len() > 50_000);
    let s = json(&get(
        &base,
        &format!("/api/extensions/jobs/{}", tmp.str_or("job", "")),
    ));
    assert_eq!(s.str_or("state", ""), "done");
    assert!(s.get("track").is_null());
    assert!(server.wait_for_scan(Duration::from_secs(10)));
    let lib = json(&get(&base, "/api/library"));
    assert_eq!(lib.get("tracks").as_array().unwrap().len(), 1);

    // Bad ids and unknown extensions.
    let r = post(
        &base,
        "/api/extensions/fake/import",
        &Value::object().with("id", "../x"),
    );
    assert_eq!(r.status, 400);
    assert_eq!(get(&base, "/api/extensions/nope/search?q=a").status, 404);
    assert_eq!(get(&base, "/api/extensions/down/search?q=a").status, 502);
    let _ = std::fs::remove_dir_all(&dir);
}
