//! A headless app core for tests (host services from `bl_core::native`).
#![allow(dead_code)]

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
        "linux"
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
        let mut host = TestHost {
            svc: NativeServices::new(dir.clone()),
            audio: Default::default(),
            push: Default::default(),
            opened: Default::default(),
        };
        let core = Core::new(&mut host, LaunchArgs::default(), "en");
        App { core, host, dir }
    }

    pub fn cmd(&mut self, v: Value) -> Value {
        self.core.command(&mut self.host, &v)
    }

    pub fn state(&mut self) -> Value {
        self.core.tick(&mut self.host);
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
