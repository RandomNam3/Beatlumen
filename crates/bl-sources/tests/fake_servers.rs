//! The sources against small fake servers: Subsonic (token login, search3
//! listing, raw stream with range, scrobble), Jellyfin (login, items,
//! static stream with the token header), WebDAV (PROPFIND walk, tags read
//! from the files, Basic login).

use bl_sources::{connect, MusicSource, RemoteTrack, SourceConfig};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A server answering with `route`: (status, content type, body). Range
/// requests are honoured for 200 answers. Records the requests.
fn serve(
    route: impl Fn(&Req) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let route = Arc::new(route);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let route = route.clone();
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let target = parts.next().unwrap_or("").to_string();
                    let mut headers = Vec::new();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            headers.push((k.trim().to_string(), v.trim().to_string()));
                        }
                    }
                    let len = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; len];
                    r.read_exact(&mut body).unwrap();
                    let req = Req {
                        method,
                        target,
                        headers,
                        body,
                    };
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {}", req.method, req.target));
                    let (status, kind, data) = route(&req);
                    let mut s = s.try_clone().unwrap();
                    let range = req
                        .header("range")
                        .and_then(|v| v.strip_prefix("bytes="))
                        .and_then(|v| v.trim_end_matches('-').parse::<usize>().ok());
                    let head = match (status, range) {
                        (200, Some(from)) if from < data.len() => format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\n\r\n",
                            data.len() - from,
                            data.len() - 1,
                            data.len()
                        ),
                        _ => format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n", data.len()),
                    };
                    let from = if status == 200 {
                        range.unwrap_or(0).min(data.len())
                    } else {
                        0
                    };
                    if s.write_all(head.as_bytes()).is_err() || s.write_all(&data[from..]).is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (base, log)
}

/// A short WAV file with a title in a LIST/INFO chunk.
fn wav(title: &str) -> Vec<u8> {
    let rate = 8000u32;
    let samples: Vec<u8> = (0..8000u32)
        .flat_map(|i| (((i % 50) as i16 - 25) * 300).to_le_bytes())
        .collect();
    let mut info = b"INFO".to_vec();
    let mut name = title.as_bytes().to_vec();
    name.push(0);
    if name.len() % 2 == 1 {
        name.push(0);
    }
    info.extend_from_slice(b"INAM");
    info.extend_from_slice(&(name.len() as u32).to_le_bytes());
    info.extend_from_slice(&name);
    let mut d = b"RIFF".to_vec();
    let total = 4 + 8 + 16 + 8 + info.len() + 8 + samples.len();
    d.extend_from_slice(&(total as u32).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&rate.to_le_bytes());
    d.extend_from_slice(&(rate * 2).to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&16u16.to_le_bytes());
    d.extend_from_slice(b"LIST");
    d.extend_from_slice(&(info.len() as u32).to_le_bytes());
    d.extend_from_slice(&info);
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    d.extend_from_slice(&samples);
    d
}

fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn no_known(_: &str, _: u64, _: u64) -> Option<RemoteTrack> {
    None
}

/// Reads the whole stream after seeking to `at` and checks it against `data`.
fn check_stream(src: &dyn MusicSource, key: &str, data: &[u8]) {
    let mut s = src.open(key).unwrap();
    s.seek(SeekFrom::Start(100)).unwrap();
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, data[100..]);
}

#[test]
fn subsonic() {
    let song = Arc::new(wav("Tone"));
    let file = song.clone();
    let (base, log) = serve(move |r| {
        let t = &r.target;
        // Token login: t = md5(password + s).
        let (salt, token) = (query(t, "s").unwrap_or(""), query(t, "t").unwrap_or(""));
        if query(t, "u") != Some("anna")
            || token != bl_sources::md5::md5_hex(format!("geheim{salt}").as_bytes())
        {
            return (200, "application/json", br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}"#.to_vec());
        }
        let ok = |inner: &str| {
            format!(r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1"{inner}}}}}"#)
                .into_bytes()
        };
        if t.starts_with("/rest/ping.view") {
            (200, "application/json", ok(""))
        } else if t.starts_with("/rest/search3.view") {
            let page = if query(t, "songOffset") == Some("0") {
                r#","searchResult3":{"song":[{"id":"s1","title":"Tone","artist":"Synth","album":"Tests","track":2,"year":2024,"duration":1,"size":16044,"suffix":"wav","coverArt":"al-1"},{"id":"v1","title":"Video","isVideo":true}]}"#
            } else {
                r#","searchResult3":{}"#
            };
            (200, "application/json", ok(page))
        } else if t.starts_with("/rest/stream.view")
            && query(t, "id") == Some("s1")
            && query(t, "format") == Some("raw")
        {
            (200, "audio/wav", file.to_vec())
        } else if t.starts_with("/rest/getCoverArt.view") && query(t, "id") == Some("al-1") {
            (200, "image/jpeg", b"JPEG".to_vec())
        } else if t.starts_with("/rest/scrobble.view") {
            (200, "application/json", ok(""))
        } else {
            (404, "text/plain", Vec::new())
        }
    });
    let mut cfg = SourceConfig {
        kind: "subsonic".into(),
        url: base.clone(),
        user: "anna".into(),
        secret: "falsch".into(),
        ..Default::default()
    };
    let e = connect(&cfg).err().expect("wrong password refused");
    assert!(e.contains("password"), "{e}");
    cfg.secret = "geheim".into();
    let src = connect(&cfg).unwrap();
    let list = src.list(&no_known, &mut |_| true).unwrap();
    assert_eq!(list.len(), 1, "videos are skipped");
    let t = &list[0];
    assert_eq!(
        (
            t.key.as_str(),
            t.title.as_str(),
            t.track,
            t.year,
            t.ext.as_str()
        ),
        ("s1", "Tone", 2, 2024, "wav")
    );
    assert_eq!(t.artists, vec!["Synth"]);
    check_stream(src.as_ref(), "s1", &song);
    assert_eq!(src.cover("s1", 300).as_deref(), Some(&b"JPEG"[..]));
    src.played("s1");
    let log = log.lock().unwrap();
    assert!(log
        .iter()
        .any(|l| l.contains("scrobble.view") && l.contains("id=s1")));
    assert!(
        !log.iter().any(|l| l.contains("geheim")),
        "the password never goes over the wire"
    );
}

#[test]
fn jellyfin() {
    let song = Arc::new(wav("Tone"));
    let file = song.clone();
    let (base, _log) = serve(move |r| {
        let authorized = r
            .header("authorization")
            .is_some_and(|h| h.contains("Token=\"tok123\""));
        match (r.method.as_str(), r.target.split('?').next().unwrap_or("")) {
            ("POST", "/Users/AuthenticateByName") => {
                let body = String::from_utf8_lossy(&r.body);
                if body.contains("\"Pw\":\"geheim\"") {
                    (
                        200,
                        "application/json",
                        br#"{"AccessToken":"tok123","User":{"Id":"u1"}}"#.to_vec(),
                    )
                } else {
                    (401, "text/plain", Vec::new())
                }
            }
            ("GET", "/Users/u1/Items") if authorized => {
                let page = if query(&r.target, "StartIndex") == Some("0") {
                    br#"{"Items":[{"Id":"j1","Name":"Tone","Artists":["Synth","Band"],"Album":"Tests","AlbumId":"a1","IndexNumber":4,"ProductionYear":2023,"RunTimeTicks":10000000,"Genres":["Ambient"],"MediaSources":[{"Container":"wav","Size":16044}],"AlbumPrimaryImageTag":"x"}]}"#.to_vec()
                } else {
                    br#"{"Items":[]}"#.to_vec()
                };
                (200, "application/json", page)
            }
            ("GET", "/Audio/j1/stream")
                if authorized && query(&r.target, "static") == Some("true") =>
            {
                (200, "audio/wav", file.to_vec())
            }
            ("GET", "/Items/a1/Images/Primary") if authorized => {
                (200, "image/jpeg", b"JPEG".to_vec())
            }
            _ => (404, "text/plain", Vec::new()),
        }
    });
    let mut cfg = SourceConfig {
        kind: "jellyfin".into(),
        url: base,
        user: "anna".into(),
        secret: "falsch".into(),
        ..Default::default()
    };
    assert!(connect(&cfg).is_err());
    cfg.secret = "geheim".into();
    let src = connect(&cfg).unwrap();
    let list = src.list(&no_known, &mut |_| true).unwrap();
    assert_eq!(list.len(), 1);
    let t = &list[0];
    assert_eq!(
        (
            t.title.as_str(),
            t.track,
            t.year,
            t.duration_ms,
            t.ext.as_str()
        ),
        ("Tone", 4, 2023, 1000, "wav")
    );
    assert_eq!(t.artists, vec!["Synth", "Band"]);
    check_stream(src.as_ref(), "j1", &song);
    assert_eq!(
        src.cover("j1", 300).as_deref(),
        Some(&b"JPEG"[..]),
        "the album's cover"
    );
}

#[test]
fn webdav() {
    let song = Arc::new(wav("Aus dem Ordner"));
    let file = song.clone();
    let (base, log) = serve(move |r| {
        if r.header("authorization")
            != Some(&format!("Basic {}", bl_net::base64_encode(b"anna:geheim")))
        {
            return (401, "text/plain", Vec::new());
        }
        let ms = |items: &[(&str, bool, usize)]| {
            let mut x = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
            for (href, dir, len) in items {
                x.push_str(&format!(
                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{}<d:getcontentlength>{len}</d:getcontentlength><d:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</d:getlastmodified></d:prop></d:propstat></d:response>",
                    if *dir { "<d:resourcetype><d:collection/></d:resourcetype>" } else { "<d:resourcetype/>" }
                ));
            }
            x.push_str("</d:multistatus>");
            x.into_bytes()
        };
        match (r.method.as_str(), r.target.as_str()) {
            ("PROPFIND", "/dav/Musik/") => (
                207,
                "application/xml",
                ms(&[
                    ("/dav/Musik/", true, 0),
                    ("/dav/Musik/Alben/", true, 0),
                    ("/dav/Musik/.hidden/", true, 0),
                    ("/dav/Musik/notes.txt", false, 5),
                ]),
            ),
            ("PROPFIND", "/dav/Musik/Alben/") => (
                207,
                "application/xml",
                ms(&[
                    ("/dav/Musik/Alben/", true, 0),
                    ("/dav/Musik/Alben/Ein%20Lied.wav", false, 16060),
                ]),
            ),
            ("GET", "/dav/Musik/Alben/Ein%20Lied.wav") => (200, "audio/wav", file.to_vec()),
            ("GET", "/dav/Musik/Alben/cover.jpg") => (200, "image/jpeg", b"JPEG".to_vec()),
            ("PROPFIND", p) if p.contains("hidden") => panic!("hidden folders are skipped"),
            _ => (404, "text/plain", Vec::new()),
        }
    });
    let cfg = SourceConfig {
        kind: "webdav".into(),
        url: format!("{base}/dav"),
        user: "anna".into(),
        secret: "geheim".into(),
        path: "Musik".into(),
        ..Default::default()
    };
    let src = connect(&cfg).unwrap();
    let list = src.list(&no_known, &mut |_| true).unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    let t = &list[0];
    assert_eq!(
        (t.key.as_str(), t.title.as_str(), t.modified),
        ("Alben/Ein Lied.wav", "Aus dem Ordner", 784_111_777)
    );
    assert!(t.duration_ms > 900 && t.duration_ms < 1100);
    check_stream(src.as_ref(), "Alben/Ein Lied.wav", &song);
    assert_eq!(
        src.cover("Alben/Ein Lied.wav", 300).as_deref(),
        Some(&b"JPEG"[..])
    );
    // Known files are not read again.
    let before = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("GET"))
        .count();
    let known = |key: &str, _: u64, m: u64| {
        (m == 784_111_777).then(|| RemoteTrack {
            key: key.to_string(),
            title: "cached".into(),
            ..Default::default()
        })
    };
    let again = src.list(&known, &mut |_| true).unwrap();
    assert_eq!(again[0].title, "cached");
    let after = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("GET"))
        .count();
    assert_eq!(before, after);
}
