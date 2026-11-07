//! Fake source extensions for tests (protocol: docs/EXTENSIONS.md). Protocol
//! 1: one song, WebM/Opus made with ffmpeg, delivered slowly. Protocol 2: a few
//! songs and a collection, rows of suggestions and a radio.

use bl_json::Value;
use bl_net::server::{Body, Request, Response, ServerConfig};
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const TOKEN: &str = "extension-secret-0123456789";

pub fn ffmpeg(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// Hands out the audio slowly: about 40 KB/s (roughly real time for 2 s
/// per second), so the import is still running while it is played.
pub struct Slow(std::io::Cursor<Vec<u8>>);

impl Read for Slow {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(50));
        let n = buf.len().min(2000);
        self.0.read(&mut buf[..n])
    }
}

pub fn fake_extension(webm: Vec<u8>, jpeg: Vec<u8>, audio_requests: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handler: Arc<bl_net::server::Handler> = Arc::new(move |req: Request| {
        if req.header("authorization") != Some(&format!("Bearer {TOKEN}")) {
            return Response::error(401, "wrong token");
        }
        let item = Value::object()
            .with("id", "song:1")
            .with("title", "Fremdes Lied")
            .with("artist", "Fremde Band")
            .with("album", "Album")
            .with("year", "2019")
            .with("duration", 8.0)
            .with("image", true)
            .with("detail", "Lyric video")
            .with("format", "webm-opus")
            .with("source", "https://example.org/song1")
            .with(
                "cuts",
                Value::Array(vec![Value::Array(vec![Value::from(0.0), Value::from(2.0)])]),
            );
        match req.path.as_str() {
            "/manifest" => Response::json(
                200,
                &Value::object()
                    .with("protocol", 1u64)
                    .with("id", "fake")
                    .with("name", "Fake Tube")
                    .with("version", "1")
                    .with("search", true),
            ),
            "/search" => {
                assert_eq!(req.query("q"), Some("fremdes lied & co"));
                Response::json(
                    200,
                    &Value::object().with(
                        "items",
                        Value::Array(vec![
                            item.clone(),
                            // Ids the server must not pass on.
                            Value::object().with("id", "../etc").with("title", "x"),
                        ]),
                    ),
                )
            }
            "/item/song:1" => Response::json(200, &item),
            "/image/song:1" => Response::bytes(200, "image/jpeg", jpeg.clone()),
            "/audio/song:1" => {
                audio_requests.fetch_add(1, Ordering::Relaxed);
                let mut r = Response::new(200);
                r.headers.set("Content-Type", "audio/webm");
                r.body = Body::Stream(Box::new(Slow(std::io::Cursor::new(webm.clone()))));
                r
            }
            _ => Response::error(404, "not found"),
        }
    });
    std::thread::spawn(move || {
        let _ = bl_net::server::serve(listener, handler, ServerConfig::default());
    });
    url
}

/// 2 s of silence (an intro to leave out), then 8 s of a tone; and a cover.
pub fn media(dir: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    if !ffmpeg(&["-version"]) {
        return None;
    }
    let webm = dir.join("src.webm");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=48000:cl=stereo:d=2",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=8:sample_rate=48000",
        "-filter_complex",
        "[1]aformat=channel_layouts=stereo[b];[0][b]concat=n=2:v=0:a=1",
        "-c:a",
        "libopus",
        "-b:a",
        "128k",
        webm.to_str().unwrap(),
    ]));
    let jpg = dir.join("cover.jpg");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64x64:d=1",
        "-frames:v",
        "1",
        jpg.to_str().unwrap()
    ]));
    Some((std::fs::read(&webm).unwrap(), std::fs::read(&jpg).unwrap()))
}

// ---- protocol 2 ---------------------------------------------------------------

pub fn song(id: &str, title: &str) -> Value {
    Value::object()
        .with("id", id)
        .with("title", title)
        .with("artist", "Band")
        .with("duration", 8.0)
        .with("image", true)
}

pub fn album(id: &str, title: &str) -> Value {
    Value::object()
        .with("kind", "collection")
        .with("id", id)
        .with("title", title)
        .with("artist", "Band")
        .with("detail", "Album")
        .with("count", 2u64)
        .with("image", true)
}

/// Counts what the extension is asked, per kind of request.
#[derive(Default)]
pub struct Asked {
    pub search: AtomicUsize,
    pub shelves: AtomicUsize,
    pub collection: AtomicUsize,
    pub radio: AtomicUsize,
    pub audio: AtomicUsize,
    /// Songs that cannot be fetched.
    pub failing: std::sync::Mutex<Vec<String>>,
}

/// A fake extension. `protocol` 1 ignores all but the search.
pub fn extension(protocol: u64, media: Option<Vec<u8>>, asked: Arc<Asked>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handler: Arc<bl_net::server::Handler> = Arc::new(move |req: Request| {
        let path = req.path.clone();
        if path == "/manifest" {
            let m = Value::object()
                .with("protocol", protocol)
                .with("name", "Tube")
                .with("search", true);
            return Response::json(
                200,
                &if protocol >= 2 {
                    m.with(
                        "capabilities",
                        vec!["search", "shelves", "collections", "radio"],
                    )
                } else {
                    m
                },
            );
        }
        if path == "/search" {
            asked.search.fetch_add(1, Ordering::Relaxed);
            return Response::json(
                200,
                &Value::object().with(
                    "items",
                    Value::Array(vec![
                        song("s:1", "First"),
                        album("a:1", "Record"),
                        Value::object().with("kind", "video-game").with("id", "x"),
                        song("../bad", "Bad id"),
                    ]),
                ),
            );
        }
        if path == "/shelves" {
            asked.shelves.fetch_add(1, Ordering::Relaxed);
            return Response::json(
                200,
                &Value::object().with(
                    "shelves",
                    Value::Array(vec![
                        Value::object()
                            .with("id", "top")
                            .with("title", "Top\u{7}Songs")
                            .with(
                                "items",
                                Value::Array(vec![song("s:1", "First"), album("a:1", "Record")]),
                            ),
                        // Nothing to show: left out.
                        Value::object()
                            .with("title", "Empty")
                            .with("items", Value::Array(vec![])),
                    ]),
                ),
            );
        }
        if path == "/collection/a:1" {
            asked.collection.fetch_add(1, Ordering::Relaxed);
            let second = req.query("after") == Some("page/2+=");
            let items = if second {
                vec![song("s:3", "Third")]
            } else {
                vec![
                    song("s:1", "First"),
                    song("s:2", "Second"),
                    album("a:2", "No"),
                ]
            };
            let mut v = Value::object()
                .with("title", "Record")
                .with("artist", "Band")
                .with("image", true)
                .with("items", Value::Array(items));
            if !second {
                v = v.with("next", "page/2+=");
            }
            return Response::json(200, &v);
        }
        if path == "/radio/s:1" {
            asked.radio.fetch_add(1, Ordering::Relaxed);
            return Response::json(
                200,
                &Value::object().with(
                    "items",
                    Value::Array(vec![song("s:2", "Second"), song("s:3", "Third")]),
                ),
            );
        }
        if let (Some(item), true) = (path.strip_prefix("/item/"), media.is_some()) {
            if asked.failing.lock().unwrap().iter().any(|f| f == item) {
                return Response::error(500, "this one cannot be fetched");
            }
            let title = match item {
                "s:1" => "First",
                "s:2" => "Second",
                "s:3" => "Third",
                other => other,
            };
            return Response::json(
                200,
                &song(item, title)
                    .with("image", false)
                    .with("format", "webm-opus"),
            );
        }
        if let (true, Some(webm)) = (path.starts_with("/audio/"), media.as_ref()) {
            asked.audio.fetch_add(1, Ordering::Relaxed);
            let mut r = Response::new(200);
            r.headers.set("Content-Type", "audio/webm");
            r.body = Body::Bytes(webm.clone());
            return r;
        }
        Response::error(404, "not found")
    });
    std::thread::spawn(move || {
        let _ = bl_net::server::serve(listener, handler, ServerConfig::default());
    });
    url
}
