//! A fake Subsonic server with one song ("Remote One", key "s1", login
//! anna / geheim); records the methods called.

use bl_json::Value;
use bl_net::server::{Request, Response};
use std::io::Cursor;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A Subsonic answer with the field `key` (none when empty).
fn ok(key: &str, v: Value) -> Response {
    let mut inner = Value::object()
        .with("status", "ok")
        .with("version", "1.16.1");
    if !key.is_empty() {
        inner = inner.with(key, v);
    }
    Response::json(200, &Value::object().with("subsonic-response", inner))
}

/// A Subsonic server with one song; records the methods called.
pub fn fake_subsonic(song: Arc<Vec<u8>>, cover: Arc<Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let handler: Arc<bl_net::server::Handler> = Arc::new(move |req: Request| {
        let method = req
            .path
            .strip_prefix("/rest/")
            .and_then(|m| m.strip_suffix(".view"))
            .unwrap_or("")
            .to_string();
        seen.lock().unwrap().push(method.clone());
        // The login: a token made from the password and a salt.
        let salt = req.query("s").unwrap_or("");
        let token = bl_sources::md5::md5_hex(format!("geheim{salt}").as_bytes());
        if req.query("u") != Some("anna") || req.query("t") != Some(token.as_str()) {
            let error = Value::object().with("code", 40u64).with("message", "Wrong");
            return Response::json(
                200,
                &Value::object().with(
                    "subsonic-response",
                    Value::object()
                        .with("status", "failed")
                        .with("error", error),
                ),
            );
        }
        match method.as_str() {
            "ping" | "scrobble" => ok("", Value::Null),
            "search3" => {
                let songs = if req.query("songOffset") == Some("0") {
                    vec![Value::object()
                        .with("id", "s1")
                        .with("title", "Remote One")
                        .with("artist", "Far Artist")
                        .with("album", "Far Album")
                        .with("track", 3u64)
                        .with("year", 2020u64)
                        .with("duration", 2u64)
                        .with("size", song.len() as u64)
                        .with("suffix", "wav")
                        .with("coverArt", "al-1")]
                } else {
                    Vec::new()
                };
                ok(
                    "searchResult3",
                    Value::object().with("song", Value::Array(songs)),
                )
            }
            "stream" if req.query("id") == Some("s1") => Response::seekable(
                Cursor::new(song.as_ref().clone()),
                "audio/wav",
                req.header("range"),
            ),
            "getCoverArt" if req.query("id") == Some("al-1") => {
                Response::bytes(200, "image/jpeg", cover.as_ref().clone())
            }
            _ => Response::error(404, "not found"),
        }
    });
    std::thread::spawn(move || bl_net::server::serve(listener, handler, Default::default()));
    (base, calls)
}
