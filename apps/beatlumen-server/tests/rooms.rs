//! Listening together: real apps in rooms of a real server, with the audio
//! pulled like a sound card does, some of them at the wrong speed.

mod common;

use bl_json::Value;
use bl_net::client;
use common::App;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct World {
    dir: PathBuf,
    server: beatlumen_server::Server,
    url: String,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn world(name: &str, cfg: beatlumen_server::Config) -> World {
    let dir = std::env::temp_dir().join(format!("bl-rooms-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).unwrap();
    for n in ["Eins", "Zwei", "Drei"] {
        std::fs::write(music.join(format!("{n}.wav")), common::wav(60)).unwrap();
    }
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: dir.join("data"),
        music_dirs: vec![music],
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..cfg
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(20)));
    let url = format!("http://{}", server.addr);
    World { dir, server, url }
}

/// Pulls an app's audio in real time, times `speed`, like a sound card whose
/// clock is a little off.
struct Speaker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn speaker(app: &App, speed: f64) -> Speaker {
    let stop = Arc::new(AtomicBool::new(false));
    let audio = app.host.audio.clone();
    let s = stop.clone();
    let thread = std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut done = 0f64;
        let mut buf = vec![0f32; 96 * 2 * 20];
        while !s.load(Ordering::Relaxed) {
            let due = t0.elapsed().as_secs_f64() * 48000.0 * speed;
            let src = audio.lock().unwrap().clone();
            while done + 960.0 <= due {
                done += 960.0;
                buf.resize(960 * 2, 0.0);
                if let Some(src) = &src {
                    src.render(&mut buf, 0);
                }
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    });
    Speaker {
        stop,
        thread: Some(thread),
    }
}

fn cmd(a: &mut App, name: &str) -> Value {
    a.cmd(Value::object().with("cmd", name))
}

fn room(a: &mut App) -> Value {
    a.state().get("room").clone()
}

/// All apps go on until `f` holds.
fn until(apps: &mut [&mut App], what: &str, secs: u64, mut f: impl FnMut(&mut [&mut App]) -> bool) {
    let t0 = Instant::now();
    loop {
        for a in apps.iter_mut() {
            a.state();
        }
        if f(apps) {
            return;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(secs),
            "timeout: {what}: {}",
            apps[0].state().get("room").to_json()
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Apps go on for a while.
fn run(apps: &mut [&mut App], d: Duration) {
    let t0 = Instant::now();
    while t0.elapsed() < d {
        for a in apps.iter_mut() {
            a.state();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn pos(a: &mut App) -> f64 {
    a.core.position(&a.host)
}

fn song_ids(a: &mut App) -> Vec<String> {
    common::wait_tracks(a, 3)
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect()
}

fn create(a: &mut App) -> String {
    cmd(a, "room.create");
    until(&mut [a], "room", 10, |x| {
        x[0].state().get("room").bool_or("active", false)
    });
    room(a).str_or("code", "").to_string()
}

fn join(a: &mut App, code: &str) {
    a.cmd(Value::object().with("cmd", "room.join").with("code", code));
    until(&mut [a], "in the room", 10, |x| {
        x[0].state().get("room").bool_or("active", false)
    });
}

fn play(a: &mut App, ids: &[String]) {
    a.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", ids.to_vec())
            .with("index", 0u64),
    );
}

fn current(s: &Value) -> String {
    s.get("current").str_or("id", "").to_string()
}

#[test]
fn a_room_plays_together_and_stays_in_step() {
    let w = world("step", Default::default());
    let mut a = App::new("room-a");
    let mut b = App::new("room-b");
    a.connect(&w.url);
    b.connect(&w.url);
    let ids = song_ids(&mut a);
    let code = create(&mut a);
    assert_eq!(code.len(), 8);
    // As people type it.
    let typed = format!("{} {}", &code[..4].to_lowercase(), &code[4..]);
    join(&mut b, &typed);
    until(&mut [&mut a, &mut b], "two members", 10, |x| {
        x[0].state()
            .get("room")
            .get("members")
            .as_array()
            .map(Vec::len)
            == Some(2)
    });
    let _sa = speaker(&a, 1.0);
    // The other sound card is 0.5 % too fast: 5 ms each second.
    let _sb = speaker(&b, 1.005);

    // A new song: neither is playing until both have it, then both start.
    play(&mut a, &ids);
    until(&mut [&mut a, &mut b], "both play the song", 20, |x| {
        let (sa, sb) = (x[0].state(), x[1].state());
        current(&sa) == ids[0]
            && current(&sb) == ids[0]
            && sa.str_or("play", "") == "playing"
            && sb.str_or("play", "") == "playing"
    });
    assert_eq!(room(&mut b).str_or("phase", ""), "playing");

    // With the drift of that sound card they stay close together for long.
    let mut worst = 0f64;
    run(&mut [&mut a, &mut b], Duration::from_secs(3));
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        run(&mut [&mut a, &mut b], Duration::from_millis(100));
        let off = (pos(&mut a) - pos(&mut b)).abs();
        worst = worst.max(off);
    }
    assert!(worst < 0.2, "out of step by {worst} s");
    assert!(pos(&mut b) > 10.0, "it went on: {}", pos(&mut b));
    // They report how they do.
    run(&mut [&mut a, &mut b], Duration::from_secs(6));
    let members = room(&mut a).get("members").clone();
    let others: Vec<&Value> = members
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| !m.bool_or("host", false))
        .collect();
    assert_eq!(others.len(), 1);
    assert!(others[0].get("driftMs").as_f64().unwrap().abs() < 200.0);

    // Only the host decides.
    cmd(&mut b, "next");
    cmd(&mut b, "pause");
    run(&mut [&mut a, &mut b], Duration::from_millis(500));
    let s = b.state();
    assert_eq!(current(&s), ids[0]);
    assert_eq!(s.str_or("play", ""), "playing");

    // The host's pause goes to everybody, at the same place.
    cmd(&mut a, "pause");
    until(&mut [&mut a, &mut b], "both paused", 10, |x| {
        x[0].state().str_or("play", "") == "paused" && x[1].state().str_or("play", "") == "paused"
    });
    run(&mut [&mut a, &mut b], Duration::from_millis(500));
    let (pa, pb) = (pos(&mut a), pos(&mut b));
    assert!((pa - pb).abs() < 0.2, "paused at {pa} and {pb}");

    // On again from elsewhere.
    a.cmd(Value::object().with("cmd", "seek").with("position", 40.0));
    cmd(&mut a, "play");
    until(&mut [&mut a, &mut b], "both play again", 10, |x| {
        x[0].state().str_or("play", "") == "playing" && x[1].state().str_or("play", "") == "playing"
    });
    run(&mut [&mut a, &mut b], Duration::from_secs(3));
    let (pa, pb) = (pos(&mut a), pos(&mut b));
    assert!(pa >= 40.0 && (pa - pb).abs() < 0.2, "at {pa} and {pb}");

    // Another song: together again.
    cmd(&mut a, "next");
    until(&mut [&mut a, &mut b], "the next song", 20, |x| {
        let (sa, sb) = (x[0].state(), x[1].state());
        current(&sa) == ids[1]
            && current(&sb) == ids[1]
            && sa.str_or("play", "") == "playing"
            && sb.str_or("play", "") == "playing"
    });
    run(&mut [&mut a, &mut b], Duration::from_secs(3));
    let (pa, pb) = (pos(&mut a), pos(&mut b));
    assert!(pa < 15.0 && (pa - pb).abs() < 0.2, "at {pa} and {pb}");

    // The host's own song is not for the room: the others stop.
    let own = w.dir.join("eigen.wav");
    std::fs::write(&own, common::wav(10)).unwrap();
    a.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![own.display().to_string()]),
    );
    until(&mut [&mut a, &mut b], "b stops", 15, |x| {
        x[0].state().get("current").str_or("id", "").contains(':')
            && x[1].state().str_or("play", "") != "playing"
    });
    assert_eq!(room(&mut b).str_or("phase", ""), "idle");
    assert_eq!(a.state().str_or("play", ""), "playing", "that one goes on");

    // Leaving is for one; the host ending is for all.
    let mut c = App::new("room-c");
    c.connect(&w.url);
    join(&mut c, &code);
    cmd(&mut c, "room.leave");
    assert!(!room(&mut c).bool_or("active", true));
    cmd(&mut a, "room.leave");
    until(&mut [&mut a, &mut b], "the room ended", 25, |x| {
        !x[1].state().get("room").bool_or("active", true)
    });
    // Whoever was in it can use the app as before.
    play(&mut b, &ids[2..]);
    until(&mut [&mut b], "plays alone", 10, |x| {
        current(&x[0].state()) == ids[2]
    });
    drop(c);
    let _ = &w.server;
}

#[test]
fn a_song_on_repeat_starts_again_for_everybody() {
    let w = world("repeat", Default::default());
    let short = w.dir.join("music").join("Kurz.wav");
    std::fs::write(&short, common::wav(4)).unwrap();
    w.server.state.request_scan();
    std::thread::sleep(Duration::from_millis(100));
    assert!(w.server.wait_for_scan(Duration::from_secs(20)));
    let mut a = App::new("room-rep-a");
    let mut b = App::new("room-rep-b");
    a.connect(&w.url);
    b.connect(&w.url);
    let tracks = common::wait_tracks(&mut a, 4);
    let id = tracks
        .iter()
        .find(|t| t.str_or("title", "") == "Kurz")
        .unwrap()
        .str_or("id", "")
        .to_string();
    let code = create(&mut a);
    join(&mut b, &code);
    let _sa = speaker(&a, 1.0);
    let _sb = speaker(&b, 1.0);
    a.cmd(Value::object().with("cmd", "repeat").with("value", "Item"));
    play(&mut a, std::slice::from_ref(&id));
    until(&mut [&mut a, &mut b], "both play", 20, |x| {
        x[0].state().str_or("play", "") == "playing" && x[1].state().str_or("play", "") == "playing"
    });
    // Twice round and more, and still together.
    let first = room(&mut a).f64_or("at", 0.0);
    until(&mut [&mut a, &mut b], "once more", 20, |x| {
        let (sa, sb) = (x[0].state(), x[1].state());
        let (pa, pb) = (pos(x[0]), pos(x[1]));
        sa.get("room").f64_or("at", 0.0) > first + 3000.0
            && sa.str_or("play", "") == "playing"
            && sb.str_or("play", "") == "playing"
            && pa > 1.0
            && pa < 2.5
            && pb < 2.5
    });
    run(&mut [&mut a, &mut b], Duration::from_millis(200));
    let (pa, pb) = (pos(&mut a), pos(&mut b));
    assert!((pa - pb).abs() < 0.3, "at {pa} and {pb}");
}

/// Status and answer of a request with a token.
fn http(url: &str, token: &str, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if !token.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    let r = client::request(
        method,
        &format!("{url}{path}"),
        &headers,
        body,
        &client::Options::default(),
    )
    .unwrap();
    let v = bl_json::parse_bytes(&r.body).unwrap_or(Value::Null);
    (r.status, v)
}

fn join_http(url: &str, code: &str) -> (u16, Value) {
    let body = Value::object()
        .with("code", code)
        .with("name", "Gast")
        .with("device", "guest-device-1");
    http(
        url,
        "",
        "POST",
        "/api/rooms/join",
        body.to_json().as_bytes(),
    )
}

#[test]
fn a_guest_gets_this_rooms_songs_and_nothing_else() {
    let w = world(
        "guest",
        beatlumen_server::Config {
            password: Some("the password".into()),
            ..Default::default()
        },
    );
    let mut a = App::new("room-host");
    a.cmd(
        Value::object()
            .with("cmd", "server.connect")
            .with("url", w.url.as_str()),
    );
    a.wait("needs login", |s| s.str_or("server", "") == "login");
    a.cmd(
        Value::object()
            .with("cmd", "server.login")
            .with("password", "the password"),
    );
    a.wait("logged in", |s| s.str_or("server", "") == "connected");
    let ids = song_ids(&mut a);
    let _sa = speaker(&a, 1.0);
    let code = create(&mut a);

    // Without a login nothing works but the room.
    assert_eq!(http(&w.url, "", "GET", "/api/library", b"").0, 401);
    assert_eq!(http(&w.url, "", "POST", "/api/rooms", b"{}").0, 401);
    let (status, _) = join_http(&w.url, "WRONGCOD");
    assert_eq!(status, 404);

    // The app of a guest: at the door of the server, then in the room.
    let mut g = App::new("room-guest");
    g.cmd(
        Value::object()
            .with("cmd", "server.connect")
            .with("url", w.url.as_str()),
    );
    g.wait("needs login", |s| s.str_or("server", "") == "login");
    join(&mut g, &code);
    g.wait("in", |s| s.str_or("server", "") == "connected");
    assert!(room(&mut g).bool_or("guest", false));
    let _sg = speaker(&g, 1.0);
    play(&mut a, &ids);
    until(&mut [&mut a, &mut g], "both play", 20, |x| {
        x[1].state().str_or("play", "") == "playing" && x[0].state().str_or("play", "") == "playing"
    });
    // It knows the song without the library.
    let s = g.state();
    assert_eq!(current(&s), ids[0]);
    assert_eq!(s.get("current").str_or("title", ""), "Drei");
    assert!(g
        .cmd(Value::object().with("cmd", "library.tracks"))
        .as_array()
        .unwrap()
        .is_empty());

    // What the guest's token opens.
    let (status, v) = join_http(&w.url, &code);
    assert_eq!(status, 200);
    let token = v.str_or("token", "").to_string();
    assert!(v.bool_or("guest", false));
    let stream = |id: &str| format!("/api/tracks/{id}/stream");
    assert_eq!(http(&w.url, &token, "GET", "/api/time", b"").0, 200);
    // The song, and what comes next.
    assert_eq!(http(&w.url, &token, "GET", &stream(&ids[0]), b"").0, 200);
    assert_eq!(http(&w.url, &token, "GET", &stream(&ids[1]), b"").0, 200);
    assert_eq!(
        http(&w.url, &token, "GET", "/api/library", b"").0,
        403,
        "not the library"
    );
    for path in ["/api/devices", "/api/playlists", "/api/images"] {
        assert_eq!(http(&w.url, &token, "GET", path, b"").0, 403, "{path}");
    }
    assert_eq!(http(&w.url, &token, "POST", "/api/rescan", b"").0, 403);
    let text = format!("/api/tracks/{}/lyrics", ids[0]);
    assert_eq!(http(&w.url, &token, "PUT", &text, b"mine now").0, 403);
    assert_eq!(http(&w.url, &token, "POST", "/api/rooms", b"{}").0, 403);
    // A guest decides nothing.
    let id = v.get("room").str_or("id", "").to_string();
    let body = Value::object().with("track", ids[1].as_str()).to_json();
    let path = format!("/api/rooms/{id}");
    assert_eq!(
        http(
            &w.url,
            &token,
            "POST",
            &format!("{path}/state"),
            body.as_bytes()
        )
        .0,
        403
    );
    assert_eq!(
        http(&w.url, &token, "POST", &format!("{path}/settings"), b"{}").0,
        403
    );
    // Nobody else's room either.
    assert_eq!(
        http(&w.url, &token, "GET", "/api/rooms/0123456789abcdef", b"").0,
        401
    );

    // The host turns guests off: they are out, their right with them.
    a.cmd(Value::object().with("cmd", "room.guests").with("on", false));
    until(&mut [&mut a, &mut g], "guests out", 15, |x| {
        !x[1].state().get("room").bool_or("active", true)
    });
    assert_eq!(http(&w.url, &token, "GET", &stream(&ids[0]), b"").0, 401);
    assert_eq!(http(&w.url, &token, "GET", "/api/time", b"").0, 401);
    g.wait("back at the door", |s| s.str_or("server", "") == "login");
    assert!(g.state().str_or("play", "") != "playing");
    let (status, _) = join_http(&w.url, &code);
    assert_eq!(status, 403, "no more guests");
    let (status, v) = {
        // Somebody with the password still gets in.
        let t = bl_json::parse_bytes(
            &client::request(
                "POST",
                &format!("{}/api/login", w.url),
                &[],
                Value::object()
                    .with("password", "the password")
                    .to_json()
                    .as_bytes(),
                &client::Options::default(),
            )
            .unwrap()
            .body,
        )
        .unwrap();
        let login = t.str_or("token", "").to_string();
        let body = Value::object()
            .with("code", code.as_str())
            .with("name", "M");
        http(
            &w.url,
            &login,
            "POST",
            "/api/rooms/join",
            body.to_json().as_bytes(),
        )
    };
    assert_eq!(status, 200);
    assert!(!v.bool_or("guest", true));

    // Guests on again, and the room ending takes the token along.
    a.cmd(Value::object().with("cmd", "room.guests").with("on", true));
    until(&mut [&mut a], "guests again", 10, |x| {
        x[0].state().get("room").bool_or("guests", false)
    });
    let (status, v) = join_http(&w.url, &code);
    assert_eq!(status, 200);
    let token = v.str_or("token", "").to_string();
    assert_eq!(http(&w.url, &token, "GET", &stream(&ids[0]), b"").0, 200);
    cmd(&mut a, "room.leave");
    assert!(!room(&mut a).bool_or("active", true));
    let t0 = Instant::now();
    while http(&w.url, &token, "GET", &stream(&ids[0]), b"").0 != 401 {
        a.state();
        assert!(t0.elapsed() < Duration::from_secs(10), "the right ended");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (status, _) = join_http(&w.url, &code);
    assert_eq!(status, 404);
}

#[test]
fn the_admin_can_keep_guests_out() {
    let w = world(
        "admin",
        beatlumen_server::Config {
            password: Some("the password".into()),
            room_guests: false,
            ..Default::default()
        },
    );
    let mut a = App::new("room-adm");
    a.cmd(
        Value::object()
            .with("cmd", "server.connect")
            .with("url", w.url.as_str()),
    );
    a.wait("needs login", |s| s.str_or("server", "") == "login");
    a.cmd(
        Value::object()
            .with("cmd", "server.login")
            .with("password", "the password"),
    );
    a.wait("logged in", |s| s.str_or("server", "") == "connected");
    let code = create(&mut a);
    assert!(!room(&mut a).bool_or("guests", true));
    assert!(!a.state().bool_or("serverRoomGuests", true));
    // The host cannot turn them on against the server.
    a.cmd(Value::object().with("cmd", "room.guests").with("on", true));
    run(&mut [&mut a], Duration::from_millis(500));
    assert!(!room(&mut a).bool_or("guests", true));
    assert_eq!(join_http(&w.url, &code).0, 403);
}

#[test]
fn only_songs_of_the_server_and_no_guessing_codes() {
    let w = world("songs", Default::default());
    let mut a = App::new("room-songs");
    a.connect(&w.url);
    let ids = song_ids(&mut a);
    let code = create(&mut a);
    let (_, v) = join_http(&w.url, &code);
    let id = v.get("room").str_or("id", "").to_string();

    // The host's token is only known to its app, so through a second room.
    let host = Value::object()
        .with("name", "H")
        .with("device", "host-device-1");
    let (status, r) = http(&w.url, "", "POST", "/api/rooms", host.to_json().as_bytes());
    assert_eq!(status, 201);
    let (rid, token) = (
        r.get("room").str_or("id", "").to_string(),
        r.str_or("token", "").to_string(),
    );
    assert_ne!(rid, id);
    let state = |track: &str| {
        let body = Value::object().with("track", track).with("playing", true);
        http(
            &w.url,
            &token,
            "POST",
            &format!("/api/rooms/{rid}/state"),
            body.to_json().as_bytes(),
        )
    };
    // Nothing of this device, nothing unknown.
    assert_eq!(state("file:/home/someone/song.flac").0, 404);
    assert_eq!(state("vault:0123456789abcdef").0, 404);
    assert_eq!(state("0123456789abcdef").0, 404);
    let (status, v) = state(&ids[0]);
    assert_eq!(status, 200);
    assert_eq!(v.str_or("phase", ""), "preparing");
    // Alone in the room and not ready: it starts by itself after the wait.
    let path = format!("/api/rooms/{rid}");
    let t0 = Instant::now();
    loop {
        let (_, v) = http(&w.url, &token, "GET", &path, b"");
        if v.str_or("phase", "") == "playing" {
            assert!(t0.elapsed() > Duration::from_secs(5), "it waited first");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(15), "started");
        std::thread::sleep(Duration::from_millis(200));
    }
    // A song in this device's own files stays out of the room.
    let local = w.dir.join("eigen.wav");
    std::fs::write(&local, common::wav(10)).unwrap();
    a.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![local.display().to_string()]),
    );
    a.wait("plays its own", |s| {
        s.get("current").str_or("id", "").contains(':')
    });
    run(&mut [&mut a], Duration::from_millis(300));
    assert_eq!(room(&mut a).str_or("phase", ""), "idle");

    // Guessing codes does not last.
    let mut last = 0;
    for i in 0..30 {
        last = join_http(&w.url, &format!("AAAAAA{:02}", i % 90)).0;
    }
    assert_eq!(last, 429);
    assert_eq!(
        join_http(&w.url, &code).0,
        429,
        "for now not even the right one"
    );
}
