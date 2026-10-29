//! A headless app core for tests (host services from `bl_core::native`).
#![allow(dead_code)]

pub mod authenticator;
pub mod fake_extension;

use bl_core::host::{AudioDeviceInfo, AudioSource, DirEntry, Host, HttpRequest, HttpResponse};
use bl_core::native::NativeServices;
use bl_core::{Core, LaunchArgs};
use bl_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct TestHost {
    pub svc: NativeServices,
    pub audio: Arc<Mutex<Option<Arc<dyn AudioSource>>>>,
    /// The last UnifiedPush registration: (instance, VAPID key).
    pub push: Arc<Mutex<Option<(String, String)>>>,
    /// The last URL the core asked to open in the browser.
    pub opened: Arc<Mutex<Option<String>>>,
    /// Plays Android: a platform credential manager.
    pub android: bool,
    /// Requests to the credential manager: (kind, JSON).
    pub credentials: Arc<Mutex<Vec<(String, String)>>>,
    /// A protected store (None: the platform has none).
    pub secrets: Option<Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>>,
    /// Reads waiting to be answered (`App::state` answers them).
    pub secret_reads: Arc<Mutex<Vec<String>>>,
    /// Requests for other apps' playback (Android): (kind, JSON).
    pub capture: Arc<Mutex<Vec<(String, String)>>>,
}

impl Host for TestHost {
    fn now(&self) -> f64 {
        self.svc.now()
    }
    fn unix_time(&self) -> f64 {
        self.svc.unix_time()
    }
    fn random_seed(&self) -> u64 {
        self.svc.random_seed()
    }
    fn platform(&self) -> &'static str {
        if self.android {
            "android"
        } else {
            "linux"
        }
    }
    fn storage_read(&self, key: &str) -> Option<Vec<u8>> {
        self.svc.storage_read(key)
    }
    fn storage_write(&self, key: &str, data: &[u8]) -> bool {
        self.svc.storage_write(key, data)
    }
    fn storage_delete(&self, key: &str) {
        self.svc.storage_delete(key)
    }
    fn storage_path(&self, key: &str) -> Option<String> {
        self.svc.storage_path(key)
    }
    fn storage_list(&self, prefix: &str) -> Vec<String> {
        self.svc.storage_list(prefix)
    }
    fn has_filesystem(&self) -> bool {
        true
    }
    fn fs_roots(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        self.svc.fs_list(path)
    }
    fn fs_open(&self, path: &str) -> Result<Box<dyn bl_media::Source>, String> {
        self.svc.fs_open(path)
    }
    fn fs_read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.svc.fs_read(path)
    }
    fn fs_metadata(&self, path: &str) -> Option<DirEntry> {
        self.svc.fs_metadata(path)
    }
    fn http_request(&mut self, req: HttpRequest) -> u64 {
        self.svc.http_request(req)
    }
    fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)> {
        self.svc.http_poll()
    }
    fn has_threads(&self) -> bool {
        true
    }
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        self.svc.spawn(job)
    }
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String> {
        *self.audio.lock().unwrap() = Some(source);
        Ok(AudioDeviceInfo {
            sample_rate: 48000,
            channels: 2,
        })
    }
    fn open_url(&mut self, url: &str) -> bool {
        *self.opened.lock().unwrap() = Some(url.to_string());
        true
    }
    fn push_register(&mut self, instance: &str, vapid: &str) -> i32 {
        *self.push.lock().unwrap() = Some((instance.to_string(), vapid.to_string()));
        1
    }
    fn credential_request(&mut self, kind: &str, json: &str) -> bool {
        if self.android {
            let r = (kind.to_string(), json.to_string());
            self.credentials.lock().unwrap().push(r);
        }
        self.android
    }
    fn has_capture(&self) -> bool {
        self.android
    }
    fn capture_request(&mut self, kind: &str, json: &str) -> bool {
        if self.android {
            let r = (kind.to_string(), json.to_string());
            self.capture.lock().unwrap().push(r);
        }
        self.android
    }
    fn has_secret_store(&self) -> bool {
        self.secrets.is_some()
    }
    fn secret_store(&mut self, key: &str, value: &[u8]) {
        if let Some(s) = &self.secrets {
            let mut s = s.lock().unwrap();
            if value.is_empty() {
                s.remove(key);
            } else {
                s.insert(key.to_string(), value.to_vec());
            }
        }
    }
    fn secret_load(&mut self, key: &str) -> bool {
        self.secret_reads.lock().unwrap().push(key.to_string());
        self.secrets.is_some()
    }
    fn log(&self, msg: &str) {
        eprintln!("[core] {msg}");
    }
}

pub struct App {
    pub core: Core,
    pub host: TestHost,
    pub dir: PathBuf,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl App {
    pub fn new(name: &str) -> App {
        let dir = std::env::temp_dir().join(format!("bl-app-vault-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        App::with_dir(dir)
    }

    /// Quits the app but keeps its data folder (for [`App::with_dir`]).
    #[allow(dead_code)]
    pub fn close(mut self) -> std::path::PathBuf {
        self.core.shutdown(&mut self.host);
        std::mem::take(&mut self.dir)
    }

    /// An app on an existing data folder (as after a restart).
    pub fn with_dir(dir: std::path::PathBuf) -> App {
        let mut host = TestHost {
            svc: NativeServices::new(dir.clone()),
            audio: Default::default(),
            push: Default::default(),
            opened: Default::default(),
            android: false,
            credentials: Default::default(),
            secrets: None,
            secret_reads: Default::default(),
            capture: Default::default(),
        };
        let core = Core::new(&mut host, LaunchArgs::default(), "en");
        App { core, host, dir }
    }

    pub fn cmd(&mut self, v: Value) -> Value {
        self.core.command(&mut self.host, &v)
    }

    pub fn state(&mut self) -> Value {
        self.core.tick(&mut self.host);
        let reads: Vec<String> = std::mem::take(&mut *self.host.secret_reads.lock().unwrap());
        for key in reads {
            let value = self
                .host
                .secrets
                .as_ref()
                .and_then(|s| s.lock().unwrap().get(&key).cloned())
                .unwrap_or_default();
            self.cmd(
                Value::object()
                    .with("cmd", "vault.secret")
                    .with("key", key)
                    .with("value", bl_net::base64_encode(&value)),
            );
        }
        self.core.state(&self.host)
    }

    /// Ticks until `pred` holds for the state (fails after 60 s).
    pub fn wait(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let t0 = Instant::now();
        loop {
            let s = self.state();
            if pred(&s) {
                return s;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "timeout waiting for {what}: {}",
                s.get("vault").to_json()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn files(&mut self) -> Vec<Value> {
        self.cmd(Value::object().with("cmd", "vault.files"))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub fn connect(&mut self, url: &str) {
        self.cmd(
            Value::object()
                .with("cmd", "server.connect")
                .with("url", url),
        );
        self.wait("server", |s| s.str_or("server", "") == "connected");
    }

    pub fn ready(&mut self) -> Value {
        self.wait("vault ready", |s| {
            let v = s.get("vault");
            v.str_or("state", "") == "ready" || !v.str_or("error", "").is_empty()
        })
    }
}

/// A fake push service: accepts one POST per connection and records it.
pub fn fake_push_service() -> (String, std::sync::mpsc::Receiver<(String, Vec<u8>)>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/push/abc", l.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let mut s = s;
            let _ = s.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = tx.send((head, body));
        }
    });
    (url, rx)
}

/// A stereo 44.1 kHz WAV file with a 440 Hz tone.
pub fn wav(seconds: u32) -> Vec<u8> {
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

/// The app's library once it has `n` songs (fails after 20 s).
pub fn wait_tracks(app: &mut App, n: usize) -> Vec<Value> {
    let t0 = Instant::now();
    loop {
        app.state();
        let l = app
            .cmd(Value::object().with("cmd", "library.tracks"))
            .as_array()
            .cloned()
            .unwrap_or_default();
        if l.len() >= n {
            return l;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "library: {l:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
