//! Services the core needs from the platform.
//!
//! The user interface (Qt/QML) owns the window, the GL context and the
//! event loop. It calls into [`crate::Core`] and provides the services
//! below through the [`Host`] trait: persistence, file system, HTTP,
//! background threads and the audio device.

use std::sync::Arc;

/// A HTTP request issued through the host.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Audio produced by the core and consumed by the platform audio device
/// (pull model). Implementations must be real-time friendly.
pub trait AudioSource: Send + Sync {
    /// Fills `out` with interleaved stereo samples at the device rate.
    /// `latency_frames` is the platform's current estimate of how many
    /// frames are queued in the device after previously written data.
    fn render(&self, out: &mut [f32], latency_frames: u32);
}

/// Description of an opened audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A directory entry for the folder browser.
#[derive(Debug, Clone, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time in seconds since the Unix epoch.
    pub modified: u64,
}

/// Services provided by the platform.
/// Writes a file readable only by the user (settings hold the server
/// login token); replaces an existing file atomically.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> bool {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp~");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let ok = opts
        .open(&tmp)
        .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()))
        .is_ok();
    ok && std::fs::rename(&tmp, path).is_ok()
}

pub trait Host {
    /// Monotonic time in seconds.
    fn now(&self) -> f64;
    /// Wall clock time in seconds since the Unix epoch.
    fn unix_time(&self) -> f64;
    /// A random seed (time or OS entropy based).
    fn random_seed(&self) -> u64;
    /// Short platform name ("linux", "windows", "android", "web").
    fn platform(&self) -> &'static str;

    // ---- persistence (settings, presets, library index) -------------------
    fn storage_read(&self, key: &str) -> Option<Vec<u8>>;
    fn storage_write(&self, key: &str, data: &[u8]) -> bool;
    fn storage_delete(&self, key: &str);
    fn storage_list(&self, prefix: &str) -> Vec<String>;
    /// Absolute path of a storage key on disk, for files the UI loads
    /// directly (extracted covers). None without a file system.
    fn storage_path(&self, _key: &str) -> Option<String> {
        None
    }

    // ---- local file system (None on the web) -----------------------------
    fn has_filesystem(&self) -> bool;
    /// Platform specific roots/bookmarks for the folder browser (home,
    /// music folder, drives, external storage).
    fn fs_roots(&self) -> Vec<(String, String)>;
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String>;
    fn fs_open(&self, path: &str) -> Result<Box<dyn bl_media::Source>, String>;
    fn fs_read(&self, path: &str) -> Result<Vec<u8>, String>;
    fn fs_metadata(&self, path: &str) -> Option<DirEntry>;

    // ---- network ----------------------------------------------------------
    /// Starts an asynchronous HTTP request; the result is returned by
    /// [`Host::http_poll`] with the same id.
    fn http_request(&mut self, req: HttpRequest) -> u64;
    fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)>;

    // ---- background work --------------------------------------------------
    /// Whether [`Host::spawn`] runs work on other threads.
    fn has_threads(&self) -> bool;
    /// Runs `job` on a background thread. Platforms without threads run it
    /// immediately.
    fn spawn(&self, job: Box<dyn FnOnce() + Send>);

    // ---- audio -------------------------------------------------------------
    /// Opens the audio device and starts pulling audio from `source`.
    /// Returns the device format.
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String>;
    /// Push based audio (web): queue interleaved stereo samples.
    fn audio_push(&mut self, _samples: &[f32]) {}
    /// Push based audio (web): frames queued but not yet played.
    fn audio_queued(&self) -> u32 {
        0
    }
    /// Push based audio (web): drop all queued audio.
    fn audio_clear(&mut self) {}
    /// Push based audio (web): pause/resume the output without dropping
    /// queued audio.
    fn audio_set_paused(&mut self, _paused: bool) {}
    /// Push based audio (web): output gain (volume and normalization),
    /// applied smoothly by the host.
    fn audio_set_gain(&mut self, _gain: f32) {}
    /// Whether audio is pull based ([`Host::audio_start`]) or push based.
    fn audio_is_push(&self) -> bool {
        false
    }

    /// Logs a message (stderr, logcat or the browser console).
    fn log(&self, msg: &str);

    /// Announces a network service through the platform (Android's
    /// NsdManager). Returns false where the platform has no registration
    /// of its own; the core then answers mDNS queries itself.
    fn service_register(
        &mut self,
        _instance: &str,
        _service_type: &str,
        _port: u16,
        _txt: &[String],
    ) -> bool {
        false
    }

    /// Withdraws the service announced with [`Host::service_register`].
    fn service_unregister(&mut self) {}

    /// Registers `instance` with the user's UnifiedPush distributor
    /// (`vapid`: the server's VAPID key, base64url). The endpoint arrives
    /// later as the `push.endpoint` command. Returns 1 when registration
    /// started, -1 when no distributor is installed, 0 when the platform
    /// has no UnifiedPush.
    fn push_register(&mut self, _instance: &str, _vapid: &str) -> i32 {
        0
    }

    fn push_unregister(&mut self, _instance: &str) {}
}
