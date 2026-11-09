//! Karaoke data in the app: UltraStar import into the server, export,
//! removal, the offline copy, and karaoke data computed on the device
//! (ignored; needs the vocal separation model and a song with a timed
//! text): `BL_SEP_MODEL=vocals-b62c91ce.pth BL_KARAOKE_SONG=song.flac
//! BL_KARAOKE_LRC=song.lrc cargo test --release --test app_karaoke --
//! --ignored`.

mod common;

use bl_json::Value;
use common::App;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The microphone streams and the song clock they are placed on are one
/// per process (as in the app, which has one core). Tests that use them
/// take turns: run at the same time, two players moved the one song clock
/// back and forth, and held notes broke off (seen on the Windows runner).
fn mic_turn() -> MutexGuard<'static, ()> {
    static MIC: Mutex<()> = Mutex::new(());
    MIC.lock().unwrap_or_else(|e| e.into_inner())
}

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
    let t =
        bl_pitch::KaraokeTrack::from_json(&bl_json::parse_bytes(&on_server.body).unwrap()).unwrap();
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
    eprintln!(
        "computed on the device in {:.1} s",
        t0.elapsed().as_secs_f64()
    );
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

/// Two singers on microphones: one sings the notes (an octave higher, and
/// holds the long note), one sings a semitone and a half off. Points per
/// singer at the end.
#[test]
fn singers_are_scored_against_the_notes() {
    use bl_core::mic;
    let _mic = mic_turn();
    let mut app = App::new("karaoke-round");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    let song = dir.join("Song.wav");
    std::fs::write(&song, common::wav(4)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "pause"));
    karaoke_loaded(&mut app);
    // 25 ms steps: A3 from 0.5 to 1.5 s, C4 held from 1.5 to 3 s.
    let r = app.cmd(Value::object().with("cmd", "karaoke.import").with(
        "text",
        "#TITLE:Song\n#BPM:600\n#GAP:0\n: 20 40 57 La\n: 60 60 60 lu\nE\n",
    ));
    assert_eq!(r.get("notes").as_u64(), Some(2), "{}", r.to_json());

    // Two microphones.
    app.host.mic = Some(Default::default());
    let devices = Value::Array(vec![
        Value::object()
            .with("id", "a")
            .with("name", "A")
            .with("rate", 48000),
        Value::object()
            .with("id", "b")
            .with("name", "B")
            .with("rate", 48000),
    ]);
    app.cmd(
        Value::object()
            .with("cmd", "mic.devices")
            .with("devices", devices),
    );
    let open = |app: &mut App, d: &str| {
        app.cmd(Value::object().with("cmd", "mic.open").with("device", d))
            .get("stream")
            .as_u64()
            .unwrap()
    };
    let (a, b) = (open(&mut app, "a"), open(&mut app, "b"));
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.start")
            .with("difficulty", "medium")
            .with(
                "singers",
                vec![
                    Value::object().with("stream", a).with("name", "Ana"),
                    Value::object().with("stream", b).with("name", "Ben"),
                ],
            ),
    );
    assert!(r.bool_or("ok", false), "{}", r.to_json());
    app.cmd(Value::object().with("cmd", "seek").with("position", 0.0));
    app.cmd(Value::object().with("cmd", "play"));

    let rate = 48000.0;
    let (mut phase_a, mut phase_b) = (0f64, 0f64);
    let t0 = Instant::now();
    let mut rendered = 0f64;
    let mut since = i64::MIN;
    let mut seen = 0usize;
    // The microphones' samples so far (each), from their own start.
    let (mic_t0, mic_clock0) = (Instant::now(), mic::clock_us());
    let mut pushed = 0usize;
    loop {
        app.state();
        // The audio device plays in real time.
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            while rendered < t0.elapsed().as_secs_f64() * rate {
                let mut buf = vec![0f32; 960];
                src.render(&mut buf, 0);
                rendered += 480.0;
            }
        }
        let live = app.cmd(
            Value::object()
                .with("cmd", "karaoke.live")
                .with("since", since),
        );
        if live.bool_or("finished", false) {
            break;
        }
        for s in live.get("singers").as_array().unwrap() {
            let t = s.get("trail").as_array().unwrap();
            seen += t.len() / 3;
            if t.len() >= 3 {
                since = since.max(t[t.len() - 3].as_i64().unwrap());
            }
        }
        let song = live.f64_or("songMs", 0.0) / 1000.0;
        let note = if (0.5..1.5).contains(&song) {
            Some(57.0)
        } else if (1.5..3.0).contains(&song) {
            Some(60.0)
        } else {
            None
        };
        // Each singer since the last pass: the microphones deliver in real
        // time without gaps, however long the sleep below takes (on
        // Windows at least about 15.6 ms; gaps would break held notes).
        let due = (mic_t0.elapsed().as_secs_f64() * rate) as usize;
        let n = due.saturating_sub(pushed);
        if n > 0 {
            let mut pcm_a = vec![0i16; n];
            let mut pcm_b = vec![0i16; n];
            if let Some(m) = note {
                let fa = 440.0 * 2f64.powf((m + 12.0 - 69.0) / 12.0);
                let fb = 440.0 * 2f64.powf((m + 1.5 - 69.0) / 12.0);
                for i in 0..n {
                    pcm_a[i] = (8000.0 * phase_a.sin()) as i16;
                    pcm_b[i] = (8000.0 * phase_b.sin()) as i16;
                    phase_a += std::f64::consts::TAU * fa / rate;
                    phase_b += std::f64::consts::TAU * fb / rate;
                }
            }
            let at = mic_clock0 + (pushed as f64 / rate * 1e6) as i64;
            mic::push_pcm(a as u32, &pcm_a, 1, 48000, at);
            mic::push_pcm(b as u32, &pcm_b, 1, 48000, at);
            pushed = due;
        }
        assert!(t0.elapsed() < Duration::from_secs(15), "{}", live.to_json());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(seen > 100, "only {seen} frames scored");
    let r = app.cmd(Value::object().with("cmd", "karaoke.results"));
    let singers = r.get("singers").as_array().unwrap();
    assert_eq!(singers[0].str_or("name", ""), "Ana", "{}", r.to_json());
    let ana = &singers[0];
    let ben = &singers[1];
    // Short of perfect: the tracker needs a moment at each note's start.
    assert!(ana.f64_or("points", 0.0) > 7000.0, "{}", r.to_json());
    // Both notes are long enough for the held-note bonus.
    assert_eq!(ana.get("heldNotes").as_u64(), Some(2), "{}", r.to_json());
    assert!(ben.f64_or("points", 1.0) < 1000.0, "{}", r.to_json());
    assert_eq!(ana.get("lines").as_array().unwrap().len(), 1);
}

/// Points for the text with the CTC head (ignored; needs a speech model
/// with a head, the vocals of a song and its timed text):
/// `BL_HEAD_MODEL=ggml-lyrics-base1e2-q5_1.bin BL_KARAOKE_VOCALS=vocals.wav
/// BL_KARAOKE_LRC=song.lrc cargo test --release --test app_karaoke text --
/// --ignored --nocapture`. The first 45 s of the vocals are the song; one
/// singer sings them in time, one three seconds late.
#[test]
#[ignore]
fn text_is_scored_with_the_head() {
    use bl_core::mic;
    let _mic = mic_turn();
    let (Ok(model), Ok(vocals), Ok(lrc)) = (
        std::env::var("BL_HEAD_MODEL"),
        std::env::var("BL_KARAOKE_VOCALS"),
        std::env::var("BL_KARAOKE_LRC"),
    ) else {
        eprintln!("BL_HEAD_MODEL / BL_KARAOKE_VOCALS / BL_KARAOKE_LRC not set");
        return;
    };
    let secs = 45.0;
    let ext = std::path::Path::new(&vocals)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string);
    let audio = bl_media::decode_mono(
        Box::new(std::fs::File::open(&vocals).unwrap()),
        ext.as_deref(),
        48000,
        secs as u32,
        &mut || true,
    )
    .unwrap();
    let mut app = App::new("karaoke-text");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    // The song: the vocals themselves (16-bit WAV).
    let song = dir.join("Song.wav");
    let mut wav = Vec::new();
    let data = audio.len() as u32 * 2;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&48000u32.to_le_bytes());
    wav.extend_from_slice(&96000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for s in &audio {
        wav.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(&song, wav).unwrap();
    // Notes on every syllable (the pitch does not matter here).
    let parsed = bl_pitch::lrc::parse(&std::fs::read_to_string(&lrc).unwrap());
    let mut us = String::from("#TITLE:Song\n#BPM:1500\n#GAP:0\n#LANGUAGE:English\n");
    for l in parsed
        .lines
        .iter()
        .filter(|l| l.syllables[0].start < secs - 2.0)
    {
        for s in &l.syllables {
            let at = (s.start * 100.0).round() as i64;
            let len = (((s.end - s.start) * 100.0).round() as i64).max(1);
            us += &format!(": {at} {len} 60 {}\n", s.text);
        }
        us += &format!(
            "- {}\n",
            (l.syllables.last().unwrap().end * 100.0).round() as i64 + 1
        );
    }
    us += "E\n";
    // The model with its head, as if downloaded.
    let models = app.dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    let mp = std::path::Path::new(&model);
    std::fs::copy(mp, models.join(mp.file_name().unwrap())).unwrap();
    let stem = mp.file_stem().unwrap().to_str().unwrap();
    let plain = stem.rsplit_once("-q").map_or(stem, |(s, _)| s);
    for s in [stem, plain] {
        let h = mp.with_file_name(format!("{s}.head"));
        if h.is_file() {
            std::fs::copy(&h, models.join(h.file_name().unwrap())).unwrap();
        }
    }

    app.cmd(
        Value::object()
            .with("cmd", "open")
            .with("paths", vec![song.to_string_lossy().to_string()])
            .with("play", true),
    );
    app.wait("playing", |s| s.str_or("play", "") == "playing");
    app.cmd(Value::object().with("cmd", "pause"));
    karaoke_loaded(&mut app);
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.import")
            .with("text", us),
    );
    assert!(r.get("error").is_null(), "{}", r.to_json());
    app.host.mic = Some(Default::default());
    let devices = Value::Array(vec![
        Value::object()
            .with("id", "a")
            .with("name", "A")
            .with("rate", 48000),
        Value::object()
            .with("id", "b")
            .with("name", "B")
            .with("rate", 48000),
    ]);
    app.cmd(
        Value::object()
            .with("cmd", "mic.devices")
            .with("devices", devices),
    );
    let open = |app: &mut App, d: &str| {
        app.cmd(Value::object().with("cmd", "mic.open").with("device", d))
            .get("stream")
            .as_u64()
            .unwrap()
    };
    let (a, b) = (open(&mut app, "a"), open(&mut app, "b"));
    let r = app.cmd(Value::object().with("cmd", "karaoke.start").with(
        "singers",
        vec![
            Value::object().with("stream", a).with("name", "In time"),
            Value::object().with("stream", b).with("name", "Late"),
        ],
    ));
    assert!(r.bool_or("text", false), "no text scoring: {}", r.to_json());
    app.cmd(Value::object().with("cmd", "seek").with("position", 0.0));
    app.cmd(Value::object().with("cmd", "play"));
    let t0 = Instant::now();
    let mut rendered = 0f64;
    let mut fed = 0f64;
    loop {
        app.state();
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            while rendered < t0.elapsed().as_secs_f64() * 48000.0 {
                let mut buf = vec![0f32; 960];
                src.render(&mut buf, 0);
                rendered += 480.0;
            }
        }
        let live = app.cmd(
            Value::object()
                .with("cmd", "karaoke.live")
                .with("since", 1e12),
        );
        if live.bool_or("finished", false) || t0.elapsed().as_secs_f64() > secs + 10.0 {
            break;
        }
        // The microphones hear the song at its position (the late one 3 s back).
        let song_s = live.f64_or("songMs", 0.0) / 1000.0;
        let upto = (song_s * 48000.0) as usize;
        let from = (fed as usize).min(upto);
        if upto > from {
            let cut = |o: isize| -> Vec<i16> {
                (from..upto)
                    .map(|i| {
                        let j = i as isize - o;
                        let v = if j >= 0 {
                            audio.get(j as usize).copied().unwrap_or(0.0)
                        } else {
                            0.0
                        };
                        (v.clamp(-1.0, 1.0) * 32767.0) as i16
                    })
                    .collect()
            };
            let at = mic::clock_us() - ((upto - from) as i64 * 1_000_000 / 48000);
            mic::push_pcm(a as u32, &cut(0), 1, 48000, at);
            mic::push_pcm(b as u32, &cut(3 * 48000), 1, 48000, at);
            fed = upto as f64;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // The last lines' scores come from the worker.
    let t1 = Instant::now();
    let r = loop {
        app.state();
        let r = app.cmd(Value::object().with("cmd", "karaoke.results"));
        if t1.elapsed() > Duration::from_secs(5) {
            break r;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    eprintln!("{}", r.to_json());
    let by = |n: &str| {
        r.get("singers")
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s.str_or("name", "") == n)
            .unwrap()
            .f64_or("text", -1.0)
    };
    let (in_time, late) = (by("In time"), by("Late"));
    eprintln!("text points: in time {in_time}, late {late}");
    assert!(
        in_time > 500.0 && in_time > late + 200.0,
        "{in_time} vs {late}"
    );
}

/// Latency calibration: the microphone hears the click track 120 ms after
/// it is played (as through speakers with a slow input); the calibration
/// finds that and stores it as the input's offset.
#[test]
fn calibration_finds_the_microphone_latency() {
    use bl_core::mic;
    let _mic = mic_turn();
    let mut app = App::new("karaoke-calibrate");
    app.host.mic = Some(Default::default());
    app.cmd(Value::object().with("cmd", "mic.devices").with(
        "devices",
        Value::Array(vec![Value::object()
            .with("id", "usb")
            .with("name", "USB")
            .with("rate", 48000)]),
    ));
    let stream = app
        .cmd(
            Value::object()
                .with("cmd", "mic.open")
                .with("device", "usb"),
        )
        .get("stream")
        .as_u64()
        .unwrap();
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.calibrate")
            .with("stream", stream),
    );
    assert!(r.bool_or("ok", false), "{}", r.to_json());
    let delay_us = 120_000i64;
    let t0 = Instant::now();
    let mut rendered = 0f64;
    // Rendered audio waiting to reach the microphone: (capture time, mono).
    let mut air: std::collections::VecDeque<(i64, Vec<i16>)> = Default::default();
    let result = loop {
        app.state();
        if let Some(src) = app.host.audio.lock().unwrap().clone() {
            while rendered < t0.elapsed().as_secs_f64() * 48000.0 {
                let mut buf = vec![0f32; 960];
                src.render(&mut buf, 0);
                rendered += 480.0;
                let mono: Vec<i16> = buf
                    .chunks_exact(2)
                    .map(|f| ((f[0] + f[1]) * 0.5 * 20000.0) as i16)
                    .collect();
                air.push_back((mic::clock_us() + delay_us, mono));
            }
        }
        while air.front().is_some_and(|(t, _)| *t <= mic::clock_us()) {
            let (t, pcm) = air.pop_front().unwrap();
            mic::push_pcm(stream as u32, &pcm, 1, 48000, t);
        }
        let c = app.cmd(Value::object().with("cmd", "karaoke.calibration"));
        if c.str_or("state", "") != "running" {
            break c;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{}", c.to_json());
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(result.str_or("state", ""), "done", "{}", result.to_json());
    let ms = result.get("result").f64_or("offsetMs", 0.0);
    assert!((ms - 120.0).abs() <= 25.0, "{}", result.to_json());
    // Stored for the input.
    let l = app.cmd(Value::object().with("cmd", "mic.list"));
    let stored = l.get("devices").at(0).f64_or("offsetMs", 0.0);
    assert_eq!(stored, ms, "{}", l.to_json());
}

/// "Vocals quieter" on a real song (ignored; needs the separation model):
/// `BL_SEP_MODEL=... BL_KARAOKE_SONG=song.flac cargo test --release --test
/// app_karaoke vocals -- --ignored --nocapture`. The same two seconds of
/// singing are quieter with the vocals taken out, and not silent.
#[test]
#[ignore]
fn vocals_are_turned_down() {
    let (Ok(model), Ok(song)) = (
        std::env::var("BL_SEP_MODEL"),
        std::env::var("BL_KARAOKE_SONG"),
    ) else {
        eprintln!("BL_SEP_MODEL / BL_KARAOKE_SONG not set");
        return;
    };
    let mut app = App::new("karaoke-vocals");
    let dir = app.dir.join("music");
    std::fs::create_dir_all(&dir).unwrap();
    let ext = std::path::Path::new(&song)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("flac")
        .to_string();
    let path = dir.join(format!("Song.{ext}"));
    std::fs::copy(&song, &path).unwrap();
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
    app.cmd(Value::object().with("cmd", "volume").with("value", 1.0));
    // Two seconds from 40 s on: their loudness.
    let listen = |app: &mut App| -> f64 {
        app.cmd(Value::object().with("cmd", "seek").with("position", 40.0));
        let src = app.host.audio.lock().unwrap().clone().unwrap();
        let (mut sum, mut n) = (0f64, 0usize);
        let t0 = Instant::now();
        let mut rendered = 0usize;
        // The device in real time for 2.5 s; the last two count.
        while rendered < 48000 * 5 / 2 {
            app.state();
            while (rendered as f64) < t0.elapsed().as_secs_f64() * 48000.0 {
                let mut buf = vec![0f32; 960];
                src.render(&mut buf, 0);
                rendered += 480;
                if rendered > 24000 {
                    sum += buf.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
                    n += buf.len();
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (sum / n.max(1) as f64).sqrt()
    };
    let before = listen(&mut app);
    let r = app.cmd(
        Value::object()
            .with("cmd", "karaoke.vocals")
            .with("level", 0.0),
    );
    assert!(r.get("error").is_null(), "{}", r.to_json());
    let t0 = Instant::now();
    loop {
        app.state();
        let k = app.cmd(Value::object().with("cmd", "karaoke.get"));
        if k.get("vocals").str_or("state", "") == "on" {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(300),
            "{}",
            k.get("vocals").to_json()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("separated in {:.1} s", t0.elapsed().as_secs_f64());
    let after = listen(&mut app);
    eprintln!("loudness with the singing {before:.4}, without {after:.4}");
    assert!(
        after < before * 0.9 && after > before * 0.1,
        "{before} -> {after}"
    );
    // Back up: as before.
    app.cmd(
        Value::object()
            .with("cmd", "karaoke.vocals")
            .with("level", 1.0),
    );
    let again = listen(&mut app);
    assert!(
        (again - before).abs() < before * 0.05,
        "{before} vs {again}"
    );
}
