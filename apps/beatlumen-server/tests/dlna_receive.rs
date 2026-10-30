//! The app as a DLNA speaker: a control point (bl-upnp) finds it, sends it
//! a stream and controls playback; events reach subscribers.

mod common;

use bl_json::Value;
use common::App;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn tone(seconds: u32, freq: f32) -> Vec<u8> {
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
        let s = ((i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * 12000.0) as i16;
        d.extend_from_slice(&s.to_le_bytes());
        d.extend_from_slice(&s.to_le_bytes());
    }
    d
}

/// A media server: serves two WAV files, with range requests.
fn media_server() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let long = tone(30, 440.0);
    let short = tone(3, 660.0);
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let (long, short) = (long.clone(), short.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut range = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v
                            .trim()
                            .trim_end_matches('-')
                            .split('-')
                            .next()
                            .unwrap()
                            .parse()
                            .unwrap_or(0);
                    }
                }
                let data = if line.contains("/short.wav") {
                    short
                } else {
                    long
                };
                let mut c = conn;
                let body = &data[range.min(data.len())..];
                let status = if range > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    c,
                    "HTTP/1.1 {status}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    range,
                    data.len() - 1,
                    data.len()
                );
                let _ = c.write_all(body);
            });
        }
    });
    base
}

fn tick(app: &mut App, d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        app.state();
        // Pull audio like a sound card.
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            let mut buf = vec![0f32; 1920];
            src.render(&mut buf, 0);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn control_point_plays_on_the_app() {
    let media = media_server();
    let mut app = App::new("dlna-receive");
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "airplayName")
            .with("value", "Testlautsprecher"),
    );
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "dlnaReceiver")
            .with("value", true),
    );
    let s = app.wait("renderer", |s| {
        s.get("dlnaReceiver").bool_or("running", false)
    });
    let port = s.get("dlnaReceiver").get("port").as_u64().unwrap();
    let location = format!("http://127.0.0.1:{port}/description.xml");

    // Found through SSDP, described correctly.
    let found =
        bl_upnp::ssdp::discover(bl_upnp::ssdp::MEDIA_RENDERER, Duration::from_secs(2)).unwrap();
    assert!(
        found
            .iter()
            .any(|a| a.location.ends_with(&format!(":{port}/description.xml"))),
        "{found:?}"
    );
    let r = bl_upnp::Renderer::fetch(&location).unwrap();
    assert_eq!(r.name, "Testlautsprecher");
    let sinks = r.sink_protocols().unwrap();
    assert!(sinks.iter().any(|s| s.contains("audio/flac")), "{sinks:?}");
    assert_eq!(r.transport_state().unwrap(), "NO_MEDIA_PRESENT");

    // Events: subscribe, get the initial state.
    let events = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut sub = TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    write!(
        sub,
        "SUBSCRIBE /evt/AVTransport HTTP/1.1\r\nHOST: 127.0.0.1:{port}\r\nCALLBACK: <http://{}/cb>\r\nNT: upnp:event\r\nTIMEOUT: Second-300\r\n\r\n",
        events.local_addr().unwrap()
    )
    .unwrap();
    let mut answer = String::new();
    sub.read_to_string(&mut answer).unwrap();
    assert!(
        answer.starts_with("HTTP/1.1 200") && answer.contains("SID: uuid:"),
        "{answer}"
    );
    events.set_nonblocking(false).unwrap();
    let (mut ev, _) = events.accept().unwrap();
    ev.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // Headers and body may arrive in separate packets.
    let mut first = Vec::new();
    let mut buf = vec![0u8; 8192];
    while !String::from_utf8_lossy(&first).contains("propertyset>") {
        let n = ev.read(&mut buf).unwrap();
        assert!(
            n > 0,
            "event ended early: {}",
            String::from_utf8_lossy(&first)
        );
        first.extend_from_slice(&buf[..n]);
    }
    let first = String::from_utf8_lossy(&first).into_owned();
    assert!(
        first.starts_with("NOTIFY /cb") && first.contains("TransportState"),
        "{first}"
    );
    let _ = ev.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

    // Play a stream.
    let url = format!("{media}/long.wav");
    let didl = bl_upnp::didl("Testlied", "Testband", &url, "http-get:*:audio/wav:*");
    r.set_uri(&url, &didl).unwrap();
    r.play().unwrap();
    let s = app.wait("playing", |s| s.str_or("play", "") == "playing");
    assert_eq!(s.get("current").str_or("title", ""), "Testlied");
    assert_eq!(s.get("current").str_or("artist", ""), "Testband");
    tick(&mut app, Duration::from_millis(1500));
    assert_eq!(r.transport_state().unwrap(), "PLAYING");
    // The file was read ahead and analysed as a whole.
    let s = app.wait("analysis", |s| {
        let a = s.str_or("analysis", "");
        a == "ready" || a == "failed" || a == "error"
    });
    assert_eq!(s.str_or("analysis", ""), "ready");
    let pos = r.position().unwrap().unwrap_or(0.0);
    assert!(pos >= 1.0, "position {pos}");

    // Pause, seek, volume.
    r.pause().unwrap();
    tick(&mut app, Duration::from_millis(300));
    assert_eq!(app.state().str_or("play", ""), "paused");
    assert_eq!(r.transport_state().unwrap(), "PAUSED_PLAYBACK");
    r.seek(20.0).unwrap();
    r.play().unwrap();
    tick(&mut app, Duration::from_millis(500));
    let pos = app.core.position(&app.host);
    assert!((19.0..23.0).contains(&pos), "position after seek {pos}");
    r.set_volume(30).unwrap();
    tick(&mut app, Duration::from_millis(100));
    assert!((app.state().f64_or("volume", 0.0) - 0.3).abs() < 0.01);

    // The next stream follows when the current one ends.
    let next = format!("{media}/short.wav");
    r.set_next_uri(
        &next,
        &bl_upnp::didl("Zweites", "Testband", &next, "http-get:*:audio/wav:*"),
    )
    .unwrap();
    r.seek(28.5).unwrap();
    let t0 = Instant::now();
    loop {
        tick(&mut app, Duration::from_millis(50));
        if app.state().get("current").str_or("title", "") == "Zweites" {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "next stream did not start"
        );
    }
    // It ends; nothing follows.
    let t0 = Instant::now();
    loop {
        tick(&mut app, Duration::from_millis(50));
        if r.transport_state().unwrap() == "STOPPED" {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "did not stop");
    }

    // Switched off: gone from the network.
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "dlnaReceiver")
            .with("value", false),
    );
    app.state();
    assert!(bl_upnp::Renderer::fetch(&location).is_err());
}

/// A non-loopback address of this machine (None without a network).
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

#[test]
fn new_control_points_are_asked_first() {
    let Some(ip) = lan_ip() else {
        eprintln!("no network address: skipped");
        return;
    };
    let media = media_server();
    let mut app = App::new("dlna-ask");
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "dlnaReceiver")
            .with("value", true),
    );
    let s = app.wait("renderer", |s| {
        s.get("dlnaReceiver").bool_or("running", false)
    });
    let port = s.get("dlnaReceiver").get("port").as_u64().unwrap();
    // Reached over the network address: not loopback, so a new one.
    let r = bl_upnp::Renderer::fetch(&format!("http://{ip}:{port}/description.xml")).unwrap();
    r.set_uri(&format!("{media}/long.wav"), "").unwrap();
    r.play().unwrap();
    let s = app.wait("asked", |s| !s.get("dlnaReceiver").get("asking").is_null());
    assert_eq!(
        s.get("dlnaReceiver").get("asking").str_or("ip", ""),
        ip.to_string()
    );
    tick(&mut app, Duration::from_millis(300));
    assert_ne!(app.state().str_or("play", ""), "playing");
    // Allowed (for good): what it sent meanwhile plays.
    app.cmd(
        Value::object()
            .with("cmd", "dlna.answer")
            .with("allow", true)
            .with("always", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    let s = app.state();
    assert!(s.get("dlnaReceiver").get("asking").is_null());
    assert_eq!(
        s.get("settings").get("dlnaAllowed").at(0).as_str(),
        Some(ip.to_string().as_str())
    );
    // Known now: commands act at once.
    r.pause().unwrap();
    app.wait("paused", |s| s.str_or("play", "") == "paused");
}
