//! Playing on a Chromecast, against a simulated device: it speaks the Cast
//! protocol (plain TCP here; the apps use TLS), "plays" by fetching the
//! stream URL like the Default Media Receiver, and records the commands.

mod common;

use bl_cast::proto::{take_messages, Message};
use bl_json::Value;
use common::App;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Seen {
    commands: Vec<String>,
    volume: Option<f64>,
    audio: Vec<u8>,
    pongs: usize,
    url: String,
}

fn send(s: &mut TcpStream, src: &str, dst: &str, ns: &str, payload: Value) {
    let m = Message {
        source: src.into(),
        destination: dst.into(),
        namespace: ns.into(),
        payload: payload.to_json(),
    };
    let _ = s.write_all(&m.encode());
}

/// Fetches the stream like the device would and keeps the audio.
fn fetch(url: String, seen: Arc<Mutex<Seen>>) {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = TcpStream::connect(host).unwrap();
    write!(s, "GET /{path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).unwrap_or(0) == 0 {
            return;
        }
        head.push(b[0]);
    }
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => seen.lock().unwrap().audio.extend_from_slice(&buf[..n]),
        }
    }
}

fn device(listener: TcpListener, seen: Arc<Mutex<Seen>>) {
    let (mut s, _) = listener.accept().unwrap();
    s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut buf = Vec::new();
    let mut launched = false;
    let mut state = "PLAYING";
    let mut loaded_at: Option<Instant> = None;
    let mut pinged = false;
    let mut chunk = [0u8; 4096];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => {}
        }
        if !pinged {
            pinged = true;
            send(
                &mut s,
                "receiver-0",
                "sender-0",
                "urn:x-cast:com.google.cast.tp.heartbeat",
                Value::object().with("type", "PING"),
            );
        }
        for m in take_messages(&mut buf).unwrap() {
            let v = bl_json::parse(&m.payload).unwrap();
            let kind = v.str_or("type", "").to_string();
            seen.lock().unwrap().commands.push(kind.clone());
            let status = |launched: bool| {
                let apps = if launched {
                    vec![Value::object()
                        .with("appId", "CC1AD845")
                        .with("sessionId", "session-1")
                        .with("transportId", "web-5")]
                } else {
                    Vec::new()
                };
                Value::object()
                    .with("type", "RECEIVER_STATUS")
                    .with("requestId", v.get("requestId").clone())
                    .with(
                        "status",
                        Value::object().with("applications", Value::Array(apps)),
                    )
            };
            let media = |state: &str, loaded_at: Option<Instant>| {
                // The device plays what it fetched, half a second behind.
                let time = loaded_at.map_or(0.0, |t| (t.elapsed().as_secs_f64() - 0.5).max(0.0));
                Value::object().with("type", "MEDIA_STATUS").with(
                    "status",
                    Value::Array(vec![Value::object()
                        .with("mediaSessionId", 1u64)
                        .with("playerState", state)
                        .with("currentTime", time)]),
                )
            };
            match (m.namespace.as_str(), kind.as_str()) {
                ("urn:x-cast:com.google.cast.tp.heartbeat", "PONG") => {
                    seen.lock().unwrap().pongs += 1
                }
                ("urn:x-cast:com.google.cast.tp.heartbeat", "PING") => send(
                    &mut s,
                    &m.destination,
                    &m.source,
                    &m.namespace,
                    Value::object().with("type", "PONG"),
                ),
                ("urn:x-cast:com.google.cast.receiver", "GET_STATUS") => send(
                    &mut s,
                    "receiver-0",
                    "sender-0",
                    &m.namespace,
                    status(launched),
                ),
                ("urn:x-cast:com.google.cast.receiver", "LAUNCH") => {
                    launched = true;
                    send(&mut s, "receiver-0", "sender-0", &m.namespace, status(true));
                }
                ("urn:x-cast:com.google.cast.receiver", "SET_VOLUME") => {
                    seen.lock().unwrap().volume = v.get("volume").get("level").as_f64();
                }
                ("urn:x-cast:com.google.cast.receiver", "STOP") => return,
                ("urn:x-cast:com.google.cast.media", "LOAD") => {
                    assert_eq!(m.destination, "web-5");
                    assert_eq!(v.str_or("sessionId", ""), "session-1");
                    let url = v.get("media").str_or("contentId", "").to_string();
                    assert_eq!(v.get("media").str_or("contentType", ""), "audio/wav");
                    seen.lock().unwrap().url = url.clone();
                    let s2 = seen.clone();
                    std::thread::spawn(move || fetch(url, s2));
                    loaded_at = Some(Instant::now());
                    send(
                        &mut s,
                        "web-5",
                        "sender-0",
                        &m.namespace,
                        media(state, loaded_at),
                    );
                }
                ("urn:x-cast:com.google.cast.media", "PAUSE") => {
                    state = "PAUSED";
                    send(
                        &mut s,
                        "web-5",
                        "sender-0",
                        &m.namespace,
                        media(state, loaded_at),
                    );
                }
                ("urn:x-cast:com.google.cast.media", "PLAY") => {
                    state = "PLAYING";
                    send(
                        &mut s,
                        "web-5",
                        "sender-0",
                        &m.namespace,
                        media(state, loaded_at),
                    );
                }
                ("urn:x-cast:com.google.cast.media", "GET_STATUS") => send(
                    &mut s,
                    "web-5",
                    "sender-0",
                    &m.namespace,
                    media(state, loaded_at),
                ),
                _ => {}
            }
        }
    }
}

fn tone(seconds: u32) -> Vec<u8> {
    let rate = 44100u32;
    let n = rate * seconds;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&(36 + n * 4).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&2u16.to_le_bytes());
    d.extend_from_slice(&rate.to_le_bytes());
    d.extend_from_slice(&(rate * 4).to_le_bytes());
    d.extend_from_slice(&4u16.to_le_bytes());
    d.extend_from_slice(&16u16.to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(n * 4).to_le_bytes());
    for i in 0..n {
        let s = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16;
        d.extend_from_slice(&s.to_le_bytes());
        d.extend_from_slice(&s.to_le_bytes());
    }
    d
}

#[test]
fn plays_on_a_chromecast() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = seen.clone();
    let dev = std::thread::spawn(move || device(listener, s2));

    let mut app = App::new("cast-send");
    std::fs::create_dir_all(&app.dir).unwrap();
    let song = app.dir.join("tone.wav");
    std::fs::write(&song, tone(60)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "volume").with("value", 0.8));
    let id = format!("cast:127.0.0.1:{port}");
    app.cmd(
        Value::object()
            .with("cmd", "outputs.select")
            .with("id", id.as_str()),
    );
    let s = app.wait("connected", |s| {
        s.get("outputs").str_or("status", "") == "playing"
            || s.get("outputs").get("error").as_str().is_some()
    });
    assert_eq!(
        s.get("outputs").str_or("status", ""),
        "playing",
        "{}",
        s.get("outputs").to_json()
    );

    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    {
        let seen = seen.lock().unwrap();
        // WAV header, then the tone, in real time.
        assert!(
            seen.audio.starts_with(b"RIFF"),
            "{} bytes",
            seen.audio.len()
        );
        let secs = (seen.audio.len() - 44) as f64 / 176400.0;
        assert!((4.0..7.0).contains(&secs), "{secs} s in 5 s");
        let peak = seen.audio[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).saturating_abs())
            .max()
            .unwrap();
        assert!(peak > 10000, "peak {peak}");
        assert!(
            (seen.volume.unwrap_or(0.0) - 0.8).abs() < 0.01,
            "volume {:?}",
            seen.volume
        );
        assert!(seen.pongs >= 1, "heartbeat answered");
        assert!(seen.url.contains(".wav"), "{}", seen.url);
    }
    // The visuals follow the device (it plays 0.5 s behind what it fetched).
    let pos = app.core.position(&app.host);
    let fetched = (seen.lock().unwrap().audio.len() - 44) as f64 / 176400.0;
    assert!(
        (fetched - 0.5 - pos).abs() < 0.6,
        "position {pos:.2}, fetched {fetched:.2}"
    );

    app.cmd(Value::object().with("cmd", "pause"));
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(1) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    app.cmd(Value::object().with("cmd", "play"));
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(1) {
        app.state();
        std::thread::sleep(Duration::from_millis(20));
    }
    app.cmd(
        Value::object()
            .with("cmd", "outputs.select")
            .with("id", "local"),
    );
    dev.join().unwrap();
    let cmds = seen.lock().unwrap().commands.clone();
    for c in ["CONNECT", "LAUNCH", "LOAD", "PAUSE", "PLAY", "STOP"] {
        assert!(cmds.iter().any(|x| x == c), "{c} missing: {cmds:?}");
    }
}
