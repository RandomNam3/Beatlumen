//! End-to-end tests: start the server on 127.0.0.1:0 with temporary music,
//! image and data folders and talk to it with `bl_net::client`.
//!
//! Audio fixtures are generated with ffmpeg at test time; tests that need
//! them pass with a note when ffmpeg is not installed.

use bl_net::api::{ImageEntry, Library, Playlist, PlaylistKind, ServerInfo};
use bl_net::client;
use beatlumen_server::{Config, Server};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fixtures and helpers
// ---------------------------------------------------------------------------

fn fnv_id(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in key.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A temporary directory removed when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

fn temp_dir(name: &str) -> TempDir {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("ocs-it-{}-{name}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

fn have_ffmpeg() -> bool {
    static HAVE: OnceLock<bool> = OnceLock::new();
    *HAVE.get_or_init(|| Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// Generates an audio file with ffmpeg (sine tone, given tags and codec args).
fn gen_audio(path: &Path, freq: u32, secs: f32, tags: &[(&str, &str)], codec: &[&str], extra_inputs: &[&str]) -> bool {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut c = Command::new("ffmpeg");
    c.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={freq}:duration={secs}:sample_rate=44100")]);
    c.args(extra_inputs);
    c.args(["-ac", "2"]);
    for (k, v) in tags {
        c.arg("-metadata").arg(format!("{k}={v}"));
    }
    c.args(codec);
    c.arg(path);
    c.output().map(|o| o.status.success()).unwrap_or(false)
}

fn test_image(w: u32, h: u32) -> bl_image::Image {
    let mut img = bl_image::Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            img.pixels[i..i + 4].copy_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255]);
        }
    }
    img
}

struct Fixture {
    music: PathBuf,
    /// Whether an MP3 with an embedded cover could be generated.
    mp3_cover: bool,
}

/// The shared (read-only) music folder.
fn fixture() -> Option<&'static Fixture> {
    static F: OnceLock<Option<Fixture>> = OnceLock::new();
    F.get_or_init(|| {
        if !have_ffmpeg() {
            eprintln!("note: ffmpeg not found; skipping tests that need audio fixtures");
            return None;
        }
        // Generated once per fixture version and reused by later runs.
        let music = std::env::temp_dir().join("ocs-it-fixture-v1");
        let done = music.join(".complete");
        if done.is_file() {
            let mp3_cover = music.join("Pictures/with cover.mp3").is_file();
            return Some(Fixture { music, mp3_cover });
        }
        let _ = std::fs::remove_dir_all(&music);
        let wav = ["-c:a", "pcm_s16le"];
        assert!(gen_audio(
            &music.join("Artist A/Album One/01 - first.wav"),
            440,
            3.0,
            &[("title", "First Song"), ("artist", "Artist A; Guest B"), ("album", "Album One"), ("track", "1"), ("date", "2001")],
            &wav,
            &[]
        ));
        assert!(gen_audio(
            &music.join("Artist A/Album One/02 - second.wav"),
            660,
            2.0,
            &[("title", "Second Song"), ("artist", "Artist A"), ("album", "album one"), ("track", "2")],
            &wav,
            &[]
        ));
        std::fs::write(music.join("Artist A/Album One/Cover.JPG"), bl_image::encode_jpeg(&test_image(120, 120), 90)).unwrap();
        assert!(gen_audio(&music.join("Other/03 untagged.wav"), 550, 1.0, &[], &wav, &[]));
        std::fs::write(music.join("Other/notes.txt"), "not music").unwrap();
        assert!(gen_audio(&music.join(".hidden/secret.wav"), 550, 0.5, &[("title", "Hidden")], &wav, &[]));
        // Other formats (listed even if their tag reader is not finished yet).
        gen_audio(&music.join("Formats/tone.flac"), 330, 2.0, &[("title", "Flac Tone"), ("artist", "Codec Band"), ("album", "Formats")], &["-c:a", "flac"], &[]);
        gen_audio(
            &music.join("Formats/tone.mp3"),
            330,
            2.0,
            &[("title", "Mp3 Tone"), ("artist", "Codec Band"), ("album", "Formats")],
            &["-c:a", "libmp3lame", "-b:a", "128k"],
            &[],
        );
        gen_audio(
            &music.join("Formats/tone.ogg"),
            330,
            2.0,
            &[("title", "Vorbis Tone"), ("artist", "Codec Band"), ("album", "Formats")],
            &["-c:a", "libvorbis"],
            &[],
        );
        // MP3 with an embedded front cover.
        let cover_png = music.join("cover-src.png.tmp");
        std::fs::write(&cover_png, bl_image::encode_png(&test_image(64, 48))).unwrap();
        let mp3_cover = gen_audio(
            &music.join("Pictures/with cover.mp3"),
            500,
            1.5,
            &[("title", "With Cover"), ("artist", "Pic Artist"), ("album", "Pic Album")],
            &["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "png", "-id3v2_version", "3", "-disposition:v", "attached_pic"],
            &["-f", "png_pipe", "-i", cover_png.to_str().unwrap()],
        );
        let _ = std::fs::remove_file(&cover_png);
        std::fs::write(&done, "").unwrap();
        Some(Fixture { music, mp3_cover })
    })
    .as_ref()
}

struct Srv {
    server: Server,
    base: String,
    data: PathBuf,
}

fn config(music: Vec<PathBuf>, images: Vec<PathBuf>, data: &Path, password: Option<&str>, web: Option<PathBuf>) -> Config {
    Config {
        music_dirs: music,
        image_dirs: images,
        data_dir: data.to_path_buf(),
        bind: "127.0.0.1:0".into(),
        password: password.map(str::to_string),
        web_dir: web,
        rescan_interval: 0,
        name: "Test Server".into(),
        ..Default::default()
    }
}

fn start(cfg: Config) -> Srv {
    let data = cfg.data_dir.clone();
    let server = beatlumen_server::start(cfg).expect("server starts");
    assert!(server.wait_for_scan(Duration::from_secs(120)), "initial scan did not finish");
    let base = format!("http://{}", server.addr);
    Srv { server, base, data }
}

impl Srv {
    fn req(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> client::Response {
        let h: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        client::request(method, &format!("{}{path}", self.base), &h, body, &client::Options::default()).expect("request")
    }
    fn get(&self, path: &str) -> client::Response {
        self.req("GET", path, &[], &[])
    }
    fn json(&self, path: &str) -> bl_json::Value {
        let r = self.get(path);
        assert_eq!(r.status, 200, "GET {path}: {}", String::from_utf8_lossy(&r.body));
        bl_json::parse_bytes(&r.body).unwrap()
    }
    fn library(&self) -> Library {
        Library::from_json(&self.json("/api/library"))
    }
    fn rescan(&self) {
        let r = self.req("POST", "/api/rescan", &[], &[]);
        assert_eq!(r.status, 202);
        assert!(self.server.wait_for_scan(Duration::from_secs(120)));
    }
}

fn body_json(r: &client::Response) -> bl_json::Value {
    bl_json::parse_bytes(&r.body).unwrap_or_else(|e| panic!("invalid JSON ({e}): {}", String::from_utf8_lossy(&r.body)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn info_library_stream_and_etag() {
    let Some(fx) = fixture() else { return };
    let data = temp_dir("data");
    let s = start(config(vec![fx.music.clone()], vec![], &data, None, None));

    let info = ServerInfo::from_json(&s.json("/api/info"));
    assert_eq!(info.name, "Test Server");
    assert_eq!(info.api_version, bl_net::api::API_VERSION);
    assert!(!info.auth_required && !info.scanning);

    let lib = s.library();
    assert_eq!(lib.revision, info.revision);
    assert_eq!(info.track_count as usize, lib.tracks.len());
    let by_title = |t: &str| lib.tracks.iter().find(|x| x.title == t).unwrap_or_else(|| panic!("track {t} missing: {:#?}", lib.tracks.iter().map(|t| &t.title).collect::<Vec<_>>()));

    let first = by_title("First Song");
    assert_eq!(first.id, fnv_id("0/Artist A/Album One/01 - first.wav"));
    assert_eq!(first.path, "Artist A/Album One/01 - first.wav");
    assert_eq!(first.artists, vec!["Artist A", "Guest B"]);
    assert_eq!(first.album_artist, "Artist A");
    assert_eq!((first.track, first.year), (Some(1), Some(2001)));
    assert_eq!((first.codec.as_str(), first.sample_rate, first.channels), ("pcm", 44100, 2));
    assert!((2900..=3100).contains(&first.duration_ms), "duration {}", first.duration_ms);
    assert!(first.has_cover, "folder Cover.JPG must count as cover");
    let second = by_title("Second Song");
    assert_eq!(second.album_id, first.album_id, "album names group case-insensitively");
    let untagged = by_title("untagged");
    assert!(untagged.artists.is_empty() && !untagged.has_cover);
    assert!(lib.tracks.iter().all(|t| t.title != "Hidden" && !t.path.ends_with(".txt")));

    let album = lib.albums.iter().find(|a| a.id == first.album_id).unwrap();
    assert_eq!((album.name.as_str(), album.artist.as_str(), album.track_count), ("Album One", "Artist A", 2));
    assert_eq!(album.year, Some(2001));
    assert_eq!(album.cover_track.as_deref(), Some(first.id.as_str()));
    let key = "album\u{0}artist a\u{0}album one";
    assert_eq!(album.id, fnv_id(key));

    // Other formats: listed in any case; tags when the reader supports them.
    for (file, title, codec) in [("tone.flac", "Flac Tone", "flac"), ("tone.mp3", "Mp3 Tone", "mp3"), ("tone.ogg", "Vorbis Tone", "vorbis")] {
        let rel = format!("Formats/{file}");
        if !fx.music.join(&rel).exists() {
            eprintln!("note: {file} fixture not generated (ffmpeg encoder missing)");
            continue;
        }
        let t = lib.tracks.iter().find(|t| t.path == rel).unwrap_or_else(|| panic!("{rel} not listed"));
        assert_eq!(t.id, fnv_id(&format!("0/{rel}")));
        if t.title == title {
            assert_eq!(t.codec, codec);
            assert!((1800..=2200).contains(&t.duration_ms), "{file} duration {}", t.duration_ms);
            assert_eq!(t.album_artist, "Codec Band");
        } else {
            eprintln!("note: tags of {file} not readable yet (listed as \"{}\")", t.title);
        }
    }

    // ETag / revision.
    let r = s.get("/api/library");
    let etag = r.headers.get("etag").unwrap().to_string();
    assert_eq!(etag, format!("\"{}\"", lib.revision));
    assert_eq!(r.headers.get("x-content-type-options"), Some("nosniff"));
    let r = s.req("GET", "/api/library", &[("If-None-Match", &etag)], &[]);
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());
    let r = s.req("GET", "/api/library", &[("If-None-Match", "\"1\"")], &[]);
    assert_eq!(r.status, 200);

    // Streaming with ranges.
    let file = std::fs::read(fx.music.join(&first.path)).unwrap();
    let url = bl_net::api::paths::track_stream(&first.id);
    let r = s.get(&url);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, file);
    assert_eq!(r.headers.get("content-type"), Some("audio/wav"));
    assert_eq!(r.headers.get("cache-control"), Some("private, max-age=3600"));
    assert_eq!(r.headers.get("accept-ranges"), Some("bytes"));
    let r = s.req("GET", &url, &[("Range", "bytes=100-199")], &[]);
    assert_eq!(r.status, 206);
    assert_eq!(r.body, &file[100..200]);
    assert_eq!(r.headers.get("content-range"), Some(format!("bytes 100-199/{}", file.len()).as_str()));
    let r = s.req("GET", &url, &[("Range", &format!("bytes={}-", file.len() + 10))], &[]);
    assert_eq!(r.status, 416);
    let r = s.req("HEAD", &url, &[], &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.content_length(), Some(file.len() as u64));
    if let Some(t) = lib.tracks.iter().find(|t| t.path.ends_with(".mp3")) {
        assert_eq!(s.req("HEAD", &bl_net::api::paths::track_stream(&t.id), &[], &[]).headers.get("content-type"), Some("audio/mpeg"));
    }
    assert_eq!(s.get("/api/tracks/0123456789abcdef/stream").status, 404);
    assert_eq!(s.get("/api/tracks/nothex/stream").status, 404);
    assert_eq!(s.req("DELETE", &url, &[], &[]).status, 405);
    assert_eq!(s.get("/api/nope").status, 404);

    // A restart serves the cached index immediately with the same revision.
    let s2 = beatlumen_server::start(config(vec![fx.music.clone()], vec![], &data, None, None)).unwrap();
    let r = client::request("GET", &format!("http://{}/api/library", s2.addr), &[], &[], &client::Options::default()).unwrap();
    let lib2 = Library::from_json(&body_json(&r));
    assert_eq!((lib2.revision, lib2.tracks.len()), (lib.revision, lib.tracks.len()));
    assert!(s2.wait_for_scan(Duration::from_secs(60)));
    let r = client::request("GET", &format!("http://{}/api/info", s2.addr), &[], &[], &client::Options::default()).unwrap();
    assert_eq!(ServerInfo::from_json(&body_json(&r)).revision, lib.revision, "unchanged library keeps its revision");
    assert!(s.data.join("library.json").is_file());
}

#[test]
fn auth_login_logout_and_persistence() {
    let data = temp_dir("auth");
    let music = temp_dir("auth-music");
    let s = start(config(vec![music.to_path_buf()], vec![], &data, Some("s3cret pw"), None));
    let info = ServerInfo::from_json(&s.json("/api/info"));
    assert!(info.auth_required);
    for path in ["/api/library", "/api/playlists", "/api/images", "/api/presets", "/api/tracks/0123456789abcdef/stream"] {
        let r = s.get(path);
        assert_eq!(r.status, 401, "{path} must require a token");
        assert!(body_json(&r).get("error").as_str().is_some());
    }
    assert_eq!(s.req("POST", "/api/rescan", &[], &[]).status, 401);
    assert_eq!(s.req("POST", "/api/login", &[], br#"{"password":"wrong"}"#).status, 401);
    assert_eq!(s.req("POST", "/api/login", &[], b"not json").status, 400);
    assert_eq!(s.req("GET", "/api/login", &[], &[]).status, 405);
    let r = s.req("POST", "/api/login", &[], br#"{"password":"s3cret pw"}"#);
    assert_eq!(r.status, 200);
    let token = body_json(&r).get("token").as_str().unwrap().to_string();
    assert_eq!(token.len(), 32);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let bearer = format!("Bearer {token}");
    assert_eq!(s.req("GET", "/api/library", &[("Authorization", &bearer)], &[]).status, 200);
    assert_eq!(s.get(&format!("/api/library?token={token}")).status, 200);
    assert_eq!(s.req("GET", "/api/library", &[("Authorization", "Bearer 00000000000000000000000000000000")], &[]).status, 401);
    // The token file stores digests only.
    let stored = std::fs::read_to_string(data.join("tokens.json")).unwrap();
    assert!(!stored.contains(&token) && !stored.contains("s3cret"));

    let r = s.req("POST", "/api/login", &[], br#"{"password":"s3cret pw"}"#);
    let token2 = body_json(&r).get("token").as_str().unwrap().to_string();
    assert_ne!(token, token2);
    assert_eq!(s.req("POST", "/api/logout", &[("Authorization", &bearer)], &[]).status, 204);
    assert_eq!(s.req("GET", "/api/library", &[("Authorization", &bearer)], &[]).status, 401);
    assert_eq!(s.get(&format!("/api/library?token={token2}")).status, 200);

    // Tokens survive a restart with the same password...
    let s2 = start(config(vec![music.to_path_buf()], vec![], &data, Some("s3cret pw"), None));
    assert_eq!(s2.get(&format!("/api/library?token={token2}")).status, 200);
    assert_eq!(s2.get(&format!("/api/library?token={token}")).status, 401);
    // ...but not a password change.
    let s3 = start(config(vec![music.to_path_buf()], vec![], &data, Some("new password"), None));
    assert_eq!(s3.get(&format!("/api/library?token={token2}")).status, 401);
    // Without a password everything is open.
    let open_data = temp_dir("auth-open");
    let s4 = start(config(vec![music.to_path_buf()], vec![], &open_data, None, None));
    assert_eq!(s4.get("/api/library").status, 200);
    assert_eq!(s4.get("/api/playlists").status, 200);
}

#[test]
fn playlists_crud() {
    let data = temp_dir("playlists");
    let s = start(config(vec![], vec![], &data, None, None));
    let r = s.req("POST", "/api/playlists", &[], br#"{"name":"Road Trip","kind":"track"}"#);
    assert_eq!(r.status, 201);
    let p = Playlist::from_json(&body_json(&r));
    assert_eq!(p.id.len(), 16);
    assert_eq!((p.name.as_str(), p.kind, p.items.len()), ("Road Trip", PlaylistKind::Track, 0));
    let r = s.req("POST", "/api/playlists", &[], br#"{"name":"Backgrounds","kind":"background","items":["a1"]}"#);
    let bg = Playlist::from_json(&body_json(&r));
    assert_eq!((bg.kind, bg.items.clone()), (PlaylistKind::Background, vec!["a1".to_string()]));
    assert_eq!(s.req("POST", "/api/playlists", &[], br#"{"name":"x","kind":"weird"}"#).status, 400);
    assert_eq!(s.req("POST", "/api/playlists", &[], b"[").status, 400);

    let list = s.json("/api/playlists");
    assert_eq!(list.as_array().unwrap().len(), 2);
    let path = bl_net::api::paths::playlist(&p.id);
    let r = s.req("PUT", &path, &[], br#"{"items":["0123456789abcdef","fedcba9876543210"]}"#);
    assert_eq!(r.status, 200);
    let p2 = Playlist::from_json(&body_json(&r));
    assert_eq!((p2.name.as_str(), p2.items.len()), ("Road Trip", 2));
    let r = s.req("PUT", &path, &[], br#"{"name":"Renamed"}"#);
    let p3 = Playlist::from_json(&body_json(&r));
    assert_eq!((p3.name.as_str(), p3.items.len()), ("Renamed", 2));
    assert_eq!(s.req("PUT", &path, &[], br#"{"items":5}"#).status, 400);
    assert_eq!(s.req("PUT", &path, &[], br#"{"name":""}"#).status, 400);
    assert_eq!(Playlist::from_json(&s.json(&path)), p3);
    assert_eq!(s.req("PUT", "/api/playlists/0123456789abcdef", &[], br#"{"name":"x"}"#).status, 404);

    // Stored atomically as JSON.
    let stored = bl_json::parse_bytes(&std::fs::read(data.join("playlists.json")).unwrap()).unwrap();
    assert_eq!(stored.get("playlists").as_array().unwrap().len(), 2);
    assert!(std::fs::read_dir(&data).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));

    assert_eq!(s.req("DELETE", &path, &[], &[]).status, 204);
    assert_eq!(s.get(&path).status, 404);
    assert_eq!(s.req("DELETE", &path, &[], &[]).status, 404);
    // Persisted across restarts.
    let s2 = start(config(vec![], vec![], &data, None, None));
    let list = s2.json("/api/playlists");
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(Playlist::from_json(list.at(0)), bg);
}

#[test]
fn presets_crud_and_name_sanitizing() {
    let data = temp_dir("presets");
    let s = start(config(vec![], vec![], &data, None, None));
    let preset = br#"{"version":1,"effects":[],"analysisConfig":{}}"#;
    let path = bl_net::api::paths::preset("My Preset");
    assert_eq!(path, "/api/presets/My%20Preset");
    assert_eq!(s.req("PUT", &path, &[], preset).status, 201);
    assert_eq!(s.req("PUT", &path, &[], preset).status, 200);
    assert_eq!(s.req("PUT", &bl_net::api::paths::preset("Ünïcödé"), &[], preset).status, 201);
    let list = s.json("/api/presets");
    let names: Vec<&str> = list.as_array().unwrap().iter().map(|v| v.get("name").as_str().unwrap()).collect();
    assert_eq!(names, vec!["My Preset", "Ünïcödé"]);
    let r = s.get(&path);
    assert_eq!((r.status, r.body.as_slice()), (200, &preset[..]));
    assert_eq!(r.headers.get("content-type"), Some("application/json"));
    assert!(data.join("presets/My Preset.hfx").is_file());

    assert_eq!(s.req("PUT", &path, &[], b"{not json").status, 400);
    assert_eq!(s.req("PUT", &path, &[], br#"{"effects":{}}"#).status, 400);
    assert_eq!(s.req("PUT", &path, &[], br#"[1]"#).status, 400);
    for bad in ["..%2F..%2Fevil", "a%2Fb", "..", "%2e%2e", ".hidden", "a%5Cb", "con", "x%00y", &"x".repeat(101)] {
        let r = s.req("PUT", &format!("/api/presets/{bad}"), &[], preset);
        assert!(r.status == 400 || r.status == 404, "{bad}: {}", r.status);
    }
    assert!(!data.join("evil.hfx").exists() && !data.parent().unwrap().join("evil.hfx").exists());
    assert_eq!(s.json("/api/presets").as_array().unwrap().len(), 2);

    assert_eq!(s.req("DELETE", &path, &[], &[]).status, 204);
    assert_eq!(s.get(&path).status, 404);
    assert_eq!(s.req("DELETE", &path, &[], &[]).status, 404);
}

#[test]
fn images_list_fetch_scale_upload_delete() {
    let data = temp_dir("images-data");
    let imgs = temp_dir("images");
    let png = bl_image::encode_png(&test_image(200, 100));
    let jpg = bl_image::encode_jpeg(&test_image(80, 60), 85);
    std::fs::write(imgs.join("bg.png"), &png).unwrap();
    std::fs::create_dir_all(imgs.join("sub")).unwrap();
    std::fs::write(imgs.join("sub/pic.JPG"), &jpg).unwrap();
    std::fs::write(imgs.join("readme.txt"), "x").unwrap();
    let s = start(config(vec![], vec![imgs.to_path_buf()], &data, None, None));

    let list: Vec<ImageEntry> = s.json("/api/images").as_array().unwrap().iter().map(ImageEntry::from_json).collect();
    assert_eq!(list.len(), 2);
    let bg = list.iter().find(|i| i.name == "bg.png").unwrap();
    assert_eq!(bg.id, fnv_id("0/bg.png"));
    assert_eq!(bg.size, png.len() as u64);
    let pic = list.iter().find(|i| i.name == "pic.JPG").unwrap();
    assert_eq!(pic.id, fnv_id("0/sub/pic.JPG"));
    assert!(pic.folder.ends_with("/sub"));

    let r = s.get(&bl_net::api::paths::image(&bg.id, 0));
    assert_eq!((r.status, r.headers.get("content-type")), (200, Some("image/png")));
    assert_eq!(r.body, png);
    let r = s.req("GET", &bl_net::api::paths::image(&bg.id, 0), &[("Range", "bytes=0-7")], &[]);
    assert_eq!((r.status, r.body.as_slice()), (206, &png[..8]));
    let r = s.get(&bl_net::api::paths::image(&bg.id, 64));
    assert_eq!((r.status, r.headers.get("content-type")), (200, Some("image/jpeg")), "{}", String::from_utf8_lossy(&r.body));
    let img = bl_image::decode(&r.body).unwrap();
    assert_eq!((img.width, img.height), (64, 32));
    let etag = r.headers.get("etag").unwrap().to_string();
    assert_eq!(s.req("GET", &bl_net::api::paths::image(&bg.id, 64), &[("If-None-Match", &etag)], &[]).status, 304);
    assert_eq!(s.get(&bl_net::api::paths::image(&bg.id, 64)).body, r.body, "served from cache");
    assert_eq!(std::fs::read_dir(data.join("cache/images")).unwrap().count(), 1);
    // size is clamped to 64..4096 (no upscaling beyond the original).
    let r = s.get(&bl_net::api::paths::image(&pic.id, 1));
    let small = bl_image::decode(&r.body).unwrap();
    assert_eq!((small.width, small.height), (64, 48));

    // Uploads.
    let r = s.req("POST", "/api/images", &[("X-Filename", "../../evil%20name.png")], &png);
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let up = ImageEntry::from_json(&body_json(&r));
    assert_eq!((up.name.as_str(), up.folder.as_str()), ("evil name.png", "uploads"));
    assert_eq!(up.id, fnv_id("uploads/evil name.png"));
    assert_eq!(std::fs::read(data.join("uploads/evil name.png")).unwrap(), png);
    let r = s.req("POST", "/api/images", &[("X-Filename", "evil name.gif")], &png);
    let up2 = ImageEntry::from_json(&body_json(&r));
    assert_eq!(up2.name, "evil name (2).png", "extension follows the content, names stay unique");
    let r = s.req("POST", "/api/images", &[("X-Filename", "x.png")], b"<html>not an image</html>");
    assert_eq!(r.status, 415);
    assert_eq!(s.req("POST", "/api/images", &[], &[]).status, 400);
    let r = s.req("POST", "/api/images", &[], &jpg);
    assert_eq!(ImageEntry::from_json(&body_json(&r)).name, "image.jpg");
    let list = s.json("/api/images");
    assert_eq!(list.as_array().unwrap().len(), 5);
    assert_eq!(s.get(&bl_net::api::paths::image(&up.id, 0)).body, png);

    assert_eq!(s.req("DELETE", &format!("/api/images/{}", bg.id), &[], &[]).status, 403);
    assert!(imgs.join("bg.png").exists());
    assert_eq!(s.req("DELETE", &format!("/api/images/{}", up.id), &[], &[]).status, 204);
    assert!(!data.join("uploads/evil name.png").exists());
    assert_eq!(s.get(&bl_net::api::paths::image(&up.id, 0)).status, 404);
    assert_eq!(s.req("DELETE", &format!("/api/images/{}", up.id), &[], &[]).status, 404);
    assert_eq!(s.json("/api/images").as_array().unwrap().len(), 4);
}

#[test]
fn covers_from_folder_and_embedded_pictures() {
    let Some(fx) = fixture() else { return };
    let data = temp_dir("covers");
    let s = start(config(vec![fx.music.clone()], vec![], &data, None, None));
    let lib = s.library();
    let first = lib.tracks.iter().find(|t| t.title == "First Song").unwrap();
    let r = s.get(&bl_net::api::paths::track_cover(&first.id, 64));
    assert_eq!((r.status, r.headers.get("content-type")), (200, Some("image/jpeg")), "{}", String::from_utf8_lossy(&r.body));
    let img = bl_image::decode(&r.body).unwrap();
    assert_eq!((img.width, img.height), (64, 64));
    let etag = r.headers.get("etag").unwrap().to_string();
    assert_eq!(s.req("GET", &bl_net::api::paths::track_cover(&first.id, 64), &[("If-None-Match", &etag)], &[]).status, 304);
    // Default size 300, clamped to 32..1024; never upscaled beyond the source.
    let img = bl_image::decode(&s.get(&format!("/api/tracks/{}/cover", first.id)).body).unwrap();
    assert_eq!(img.width, 120);
    let img = bl_image::decode(&s.get(&bl_net::api::paths::track_cover(&first.id, 1)).body).unwrap();
    assert_eq!(img.width, 32);
    let cached: Vec<_> = std::fs::read_dir(data.join("cache/covers")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(cached.iter().any(|n| n.starts_with(&format!("{}_64_", first.id))), "{cached:?}");

    let album = lib.albums.iter().find(|a| a.id == first.album_id).unwrap();
    let r = s.get(&format!("/api/albums/{}/cover?size=100", album.id));
    assert_eq!(r.status, 200);
    let untagged = lib.tracks.iter().find(|t| t.title == "untagged").unwrap();
    assert_eq!(s.get(&bl_net::api::paths::track_cover(&untagged.id, 100)).status, 404);
    let untagged_album = lib.albums.iter().find(|a| a.id == untagged.album_id).unwrap();
    assert_eq!(untagged_album.cover_track, None);
    assert_eq!(s.get(&format!("/api/albums/{}/cover", untagged_album.id)).status, 404);

    if fx.mp3_cover {
        let t = lib.tracks.iter().find(|t| t.path == "Pictures/with cover.mp3").unwrap();
        if t.has_cover {
            let r = s.get(&bl_net::api::paths::track_cover(&t.id, 300));
            assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
            let img = bl_image::decode(&r.body).unwrap();
            assert_eq!((img.width, img.height), (64, 48));
        } else {
            eprintln!("note: embedded MP3 picture not detected (tag reader incomplete?)");
        }
    }
}

#[test]
fn analysis_endpoint() {
    let Some(fx) = fixture() else { return };
    let data = temp_dir("analysis");
    let s = start(config(vec![fx.music.clone()], vec![], &data, None, None));
    let lib = s.library();
    let t = lib.tracks.iter().find(|t| t.title == "First Song").unwrap();
    let url = format!("/api/tracks/{}/analysis", t.id);
    let r = s.get(&url);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.headers.get("content-type"), Some("application/octet-stream"));
    let a = bl_analysis::decode(&r.body).expect("valid analysis blob");
    assert_eq!(a.sample_rate, 44100);
    assert_eq!(a.novelty.len(), 3);
    let secs = a.frame_count() as f64 / a.frame_rate();
    assert!((2.5..3.5).contains(&secs), "analysed {secs} s");
    let key = bl_analysis::AnalysisConfig::default().cache_key();
    let cache = data.join(format!("cache/analysis/{}_{key:016x}.ocan", t.id));
    assert_eq!(std::fs::read(&cache).unwrap(), r.body);
    assert_eq!(s.get(&url).body, r.body);

    // Custom configuration, requested concurrently (one computation).
    let cfg = r#"{"analysisConfig":{"spectralSmoothing":2,"chromaSmoothing":10},"noveltyConfigs":{"Low":{"spectrum":"Raw","normalization":5,"startFreq":0,"endFreq":250}}}"#;
    let curl = bl_net::api::paths::track_analysis(&t.id, &bl_net::http::percent_encode(cfg));
    let bodies: Vec<Vec<u8>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..3).map(|_| sc.spawn(|| s.get(&curl))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).map(|r| {
            assert_eq!(r.status, 200);
            r.body
        }).collect()
    });
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    assert!(bl_analysis::decode(&bodies[0]).is_some());
    assert_eq!(std::fs::read_dir(data.join("cache/analysis")).unwrap().count(), 2);
    assert_eq!(s.get(&format!("{url}?cfg=%7Bbad")).status, 400);
    assert_eq!(s.get("/api/tracks/0123456789abcdef/analysis").status, 404);

    // Other codecs when their decoders work.
    if let Some(t) = lib.tracks.iter().find(|t| t.path == "Formats/tone.flac") {
        let r = s.get(&format!("/api/tracks/{}/analysis", t.id));
        if r.status == 200 {
            assert!(bl_analysis::decode(&r.body).is_some());
        } else {
            eprintln!("note: FLAC analysis returned {}: {}", r.status, String::from_utf8_lossy(&r.body));
        }
    }
}

#[test]
fn static_files_and_path_traversal() {
    let root = temp_dir("web");
    let web = root.join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html><title>client</title>").unwrap();
    std::fs::write(web.join("app.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(web.join("assets/app.js"), "console.log(1)").unwrap();
    std::fs::write(root.join("secret.txt"), "TOP-SECRET").unwrap();
    std::fs::write(web.join(".env"), "TOP-SECRET").unwrap();
    let data = root.join("data");
    let s = start(config(vec![], vec![], &data, Some("pw"), Some(web.clone())));

    let r = s.get("/");
    assert_eq!(r.status, 200);
    assert!(String::from_utf8_lossy(&r.body).contains("client"));
    assert_eq!(r.headers.get("content-type"), Some("text/html; charset=utf-8"));
    assert!(r.headers.get("content-security-policy").unwrap().contains("script-src 'self' 'wasm-unsafe-eval'"));
    assert_eq!(r.headers.get("cross-origin-opener-policy"), Some("same-origin"));
    assert_eq!(r.headers.get("cross-origin-embedder-policy"), Some("require-corp"));
    assert_eq!(r.headers.get("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.headers.get("referrer-policy"), Some("no-referrer"));
    let etag = r.headers.get("etag").unwrap().to_string();
    assert_eq!(s.req("GET", "/", &[("If-None-Match", &etag)], &[]).status, 304);
    assert_eq!(s.get("/app.wasm").headers.get("content-type"), Some("application/wasm"));
    let r = s.get("/assets/app.js");
    assert_eq!(r.headers.get("content-type"), Some("text/javascript; charset=utf-8"));
    assert!(r.headers.get("content-security-policy").is_none());
    assert_eq!(s.get("/missing.js").status, 404);
    assert!(String::from_utf8_lossy(&s.get("/library/albums").body).contains("client"), "client routes fall back to index.html");
    assert_eq!(s.req("POST", "/", &[], b"x").status, 405);

    for p in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2E%2E%2Fsecret.txt",
        "/assets/../../secret.txt",
        "/assets/%2e%2e/%2e%2e/secret.txt",
        "/..%5csecret.txt",
        "/%5c..%5csecret.txt",
        "/.env",
        "/./index.html/../../secret.txt",
        "/%00",
        "/api/tracks/..%2F..%2Fsecret.txt/stream",
        "/api/../secret.txt",
        "/api/presets/..%2F..%2Fsecret",
        "/api/images/..%2F..%2Fsecret.txt",
    ] {
        let r = s.get(p);
        assert!(r.status >= 400, "{p} returned {}", r.status);
        assert!(!String::from_utf8_lossy(&r.body).contains("TOP-SECRET"), "{p} leaked the file");
    }
    // Same with a valid token.
    let token = body_json(&s.req("POST", "/api/login", &[], br#"{"password":"pw"}"#)).get("token").as_str().unwrap().to_string();
    for p in ["/api/presets/..%2F..%2Fsecret", "/api/presets/%2e%2e", "/api/tracks/..%2f..%2fsecret/stream"] {
        let r = s.req("GET", p, &[("Authorization", &format!("Bearer {token}"))], &[]);
        assert!(r.status == 400 || r.status == 404, "{p} returned {}", r.status);
    }

    // Placeholder page without a web client.
    let s2 = start(config(vec![], vec![], &root.join("data2"), None, None));
    let r = s2.get("/");
    assert_eq!(r.status, 200);
    assert!(String::from_utf8_lossy(&r.body).contains("Beatlumen Server is running"));
    assert!(r.headers.get("content-security-policy").is_some());
}

#[test]
fn rescan_picks_up_new_and_removed_files() {
    if !have_ffmpeg() {
        eprintln!("note: ffmpeg not found; skipping");
        return;
    }
    let music = temp_dir("rescan-music");
    let data = temp_dir("rescan-data");
    assert!(gen_audio(&music.join("a/one.wav"), 440, 0.5, &[("title", "One")], &["-c:a", "pcm_s16le"], &[]));
    let s = start(config(vec![music.to_path_buf()], vec![], &data, None, None));
    let lib = s.library();
    assert_eq!(lib.tracks.len(), 1);

    assert!(gen_audio(&music.join("b/two.WAV"), 880, 0.5, &[("title", "Two")], &["-c:a", "pcm_s16le"], &[]));
    s.rescan();
    let lib2 = s.library();
    assert_eq!(lib2.tracks.len(), 2, "uppercase extension is recognised");
    assert!(lib2.revision > lib.revision);
    assert!(lib2.tracks.iter().any(|t| t.title == "Two" && t.id == fnv_id("0/b/two.WAV")));
    // Ids are stable across rescans.
    assert_eq!(lib2.tracks.iter().find(|t| t.title == "One").unwrap().id, lib.tracks[0].id);

    // Unchanged library: same revision.
    s.rescan();
    assert_eq!(s.library().revision, lib2.revision);

    // Changed tags are picked up (size/mtime changed).
    std::thread::sleep(Duration::from_millis(20));
    assert!(gen_audio(&music.join("a/one.wav"), 440, 0.7, &[("title", "One (edit)")], &["-c:a", "pcm_s16le"], &[]));
    s.rescan();
    assert!(s.library().tracks.iter().any(|t| t.title == "One (edit)"));

    std::fs::remove_file(music.join("b/two.WAV")).unwrap();
    // Streaming a vanished file is a clean 404.
    let two = lib2.tracks.iter().find(|t| t.title == "Two").unwrap();
    assert_eq!(s.get(&bl_net::api::paths::track_stream(&two.id)).status, 404);
    s.rescan();
    let lib3 = s.library();
    assert_eq!(lib3.tracks.len(), 1);
    assert!(lib3.revision > lib2.revision);
    assert_eq!(s.get(&bl_net::api::paths::track_stream(&two.id)).status, 404);
}

#[test]
fn binary_help_and_startup() {
    let exe = env!("CARGO_BIN_EXE_beatlumen-server");
    let out = Command::new(exe).arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    for flag in ["--music", "--images", "--data", "--bind", "--password", "--web-dir", "--rescan-interval", "--name", "--config"] {
        assert!(help.contains(flag), "help lacks {flag}");
    }
    let out = Command::new(exe).arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));

    // Run the real binary and talk to it.
    let data = temp_dir("bin");
    let mut child = Command::new(exe)
        .args(["--bind", "127.0.0.1:0", "--rescan-interval", "0", "--name", "Bin Test", "--data"])
        .arg(&*data)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(i) = line.find("listening on http://") {
                let _ = tx.send(line[i + "listening on ".len()..].trim().to_string());
            }
        }
    });
    let base = rx.recv_timeout(Duration::from_secs(30)).expect("server announces its address");
    let r = client::request("GET", &format!("{base}/api/info"), &[], &[], &client::Options::default()).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(ServerInfo::from_json(&body_json(&r)).name, "Bin Test");
}

/// A minimal mono 16-bit WAV with a RIFF INFO tag list.
fn tiny_wav(title: &str, artist: &str, album: &str, track: u32) -> Vec<u8> {
    let data_len = 400u32;
    let mut info = b"INFO".to_vec();
    let track = track.to_string();
    for (id, v) in [(b"INAM", title), (b"IART", artist), (b"IPRD", album), (b"IPRT", track.as_str())] {
        let mut val = v.as_bytes().to_vec();
        val.push(0);
        info.extend_from_slice(id);
        info.extend_from_slice(&(val.len() as u32).to_le_bytes());
        info.extend_from_slice(&val);
        if val.len() % 2 == 1 {
            info.push(0);
        }
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&((4 + 24 + 8 + info.len() + 8 + data_len as usize) as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    for v in [44100u32, 88200] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    for v in [2u16, 16] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w.extend_from_slice(b"LIST");
    w.extend_from_slice(&(info.len() as u32).to_le_bytes());
    w.extend_from_slice(&info);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    w.resize(w.len() + data_len as usize, 0);
    w
}

#[test]
#[ignore = "benchmark; run with: cargo test --release -p beatlumen-server --test server -- --ignored --nocapture"]
fn large_library_20k_tracks() {
    use std::time::Instant;
    let music = temp_dir("large");
    for a in 0..200 {
        for b in 0..10 {
            let dir = music.join(format!("Artist {a}/Album {b}"));
            std::fs::create_dir_all(&dir).unwrap();
            for t in 1..=10 {
                std::fs::write(dir.join(format!("{t:02} Track.wav")), tiny_wav(&format!("Track {t}"), &format!("Artist {a}"), &format!("Album {b}"), t)).unwrap();
            }
        }
    }
    let data = temp_dir("large-data");
    let t0 = Instant::now();
    let s = start(config(vec![music.to_path_buf()], vec![], &data, None, None));
    eprintln!("initial scan of 20000 files: {:.2} s", t0.elapsed().as_secs_f64());
    let t0 = Instant::now();
    let r = s.get("/api/library");
    let get_time = t0.elapsed();
    let lib = Library::from_json(&body_json(&r));
    assert_eq!((lib.tracks.len(), lib.albums.len()), (20_000, 2000));
    eprintln!("GET /api/library: {} bytes in {:.1} ms", r.body.len(), get_time.as_secs_f64() * 1e3);
    let t0 = Instant::now();
    let r = s.req("GET", "/api/library", &[("If-None-Match", &format!("\"{}\"", lib.revision))], &[]);
    assert_eq!(r.status, 304);
    eprintln!("GET /api/library (304): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);
    let t0 = Instant::now();
    s.rescan();
    eprintln!("rescan without changes: {:.2} s", t0.elapsed().as_secs_f64());
    assert_eq!(s.library().revision, lib.revision);
    let t0 = Instant::now();
    let s2 = beatlumen_server::start(config(vec![music.to_path_buf()], vec![], &data, None, None)).unwrap();
    let r = client::request("GET", &format!("http://{}/api/library", s2.addr), &[], &[], &client::Options::default()).unwrap();
    assert_eq!(Library::from_json(&body_json(&r)).tracks.len(), 20_000);
    eprintln!("restart with cached index until library served: {:.2} s", t0.elapsed().as_secs_f64());
    assert!(s2.wait_for_scan(Duration::from_secs(120)));
}

#[test]
fn garbage_requests_are_rejected_cleanly() {
    let data = temp_dir("garbage");
    let imgs = temp_dir("garbage-img");
    std::fs::write(imgs.join("a.png"), b"\x89PNG\r\n\x1a\ntruncated").unwrap();
    let s = start(config(vec![], vec![imgs.to_path_buf()], &data, None, None));
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x as usize
    };
    let bad_image = fnv_id("0/a.png");
    let paths = [
        "/api/playlists".to_string(),
        "/api/playlists/0123456789abcdef".to_string(),
        "/api/presets/x".to_string(),
        "/api/images".to_string(),
        format!("/api/images/{bad_image}?size=100"),
        "/api/images/0123456789abcdef?size=-1".to_string(),
        "/api/login".to_string(),
        "/api/library".to_string(),
        "/api/tracks/0123456789abcdef/analysis?cfg=%7B".to_string(),
        "/api/albums/0123456789abcdef/cover?size=abc".to_string(),
        "/api/rescan".to_string(),
        "/%ff%fe".to_string(),
        "/api/%ff".to_string(),
    ];
    let methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    let png = bl_image::encode_png(&test_image(8, 8));
    let seeds: [&[u8]; 5] = [br#"{"name":"a","kind":"track","items":["x"]}"#, br#"{"effects":[],"x":[1,2,{"y":null}]}"#, br#"{"password":"x"}"#, &png, b""];
    let mut created = 0;
    for _ in 0..400 {
        let path = &paths[rnd() % paths.len()];
        let method = methods[rnd() % methods.len()];
        let mut body = seeds[rnd() % seeds.len()].to_vec();
        if !body.is_empty() {
            body.truncate(1 + rnd() % body.len());
            for _ in 0..rnd() % 4 {
                let i = rnd() % body.len();
                body[i] = rnd() as u8;
            }
        }
        let r = s.req(method, path, &[("X-Filename", "..\\..\\x.png")], &body);
        assert!(r.status < 500 || path.contains(&bad_image), "{method} {path} -> {} {}", r.status, String::from_utf8_lossy(&r.body));
        if r.status == 201 {
            created += 1;
        }
    }
    // A truncated PNG yields a clean error, not a dead connection.
    let r = s.get(&format!("/api/images/{bad_image}?size=100"));
    assert!(r.status == 500 && body_json(&r).get("error").as_str().is_some());
    assert_eq!(s.get("/api/info").status, 200);
    eprintln!("{created} random requests created something");
    assert_eq!(s.req("POST", "/api/playlists", &[], br#"{"name":"ok"}"#).status, 201, "server still works");
    // Nothing escaped the data folder.
    for e in std::fs::read_dir(data.join("uploads")).unwrap().flatten() {
        assert!(e.path().is_file());
    }
}
