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

/// What a sound card in direct use opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectOpened {
    pub rate: u32,
    pub format: bl_media::PcmFormat,
    /// Channels per frame; the music is on the first two.
    pub channels: u32,
}

/// A sound card that can be used directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDevice {
    pub id: String,
    pub name: String,
    /// Other programs stay audible on it (through the sound server).
    pub shared: bool,
}

/// A sound card in direct use: nothing lies between the samples and the card.
/// The platform pulls the samples itself (`bl_audio_render_pcm`).
pub trait DirectSink: Send + Sync {
    /// Opens the card at `rate` with the first format of `formats` it takes,
    /// or at the nearest rate if it refuses that one; nothing happens when it
    /// already runs like that. What was queued is played out first. May take
    /// seconds: the sound server has to let go of the card.
    fn open(&self, rate: u32, formats: &[bl_media::PcmFormat]) -> Result<DirectOpened, String>;
    /// Gives the card back.
    fn close(&self);
    /// Gives the card back for now (a long pause) or takes it again.
    fn set_idle(&self, idle: bool);
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

/// Ending of the files [`write_private`] writes before renaming them;
/// listings of stored files leave them out.
pub const TEMP_SUFFIX: &str = ".tmp~";

/// Writes a file readable only by the user (settings hold the server
/// login token); replaces an existing file atomically. Each write goes
/// through its own temporary file (process id and a counter in the name),
/// so writers in other threads or another running instance of the app
/// never write into or remove each other's file: the last rename wins and
/// readers see either the old or the new file, never a mix.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> bool {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(name) = path.file_name() else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}-{}{TEMP_SUFFIX}",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
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
    if ok && rename_over(&tmp, path) {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// Renames `from` over `to`. Windows can refuse for a moment while another
/// process has `to` open or replaces it too; that is tried again.
fn rename_over(from: &std::path::Path, to: &std::path::Path) -> bool {
    for attempt in 0..if cfg!(windows) { 8 } else { 1 } {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1 << attempt));
        }
        if std::fs::rename(from, to).is_ok() {
            return true;
        }
    }
    false
}

/// Services provided by the platform.
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

    /// Stops the normal audio output while a sound card is used directly, or
    /// brings it back.
    fn audio_suspend(&mut self, _suspended: bool) {}
    /// Whether sound cards can be used directly here.
    fn has_direct(&self) -> bool {
        false
    }
    /// The sound cards that can be used directly. Looks without opening any.
    fn direct_devices(&self) -> Vec<DirectDevice> {
        Vec::new()
    }
    /// Prepares the direct use of `device`, which pulls from `source`.
    fn direct_sink(
        &mut self,
        _device: &str,
        _source: Arc<dyn AudioSource>,
    ) -> Option<Arc<dyn DirectSink>> {
        None
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

    /// Opens a web page in the system's browser.
    fn open_url(&mut self, _url: &str) -> bool {
        false
    }

    /// Asks the platform's credential manager (Android) for a passkey or
    /// password (`kind` "get": {"publicKey": WebAuthn options JSON or "",
    /// "password": bool}), to create a passkey ("create": {"publicKey",
    /// "user"}) or to save a password ("save": {"user", "password"}).
    /// "get" and "create" answer later with the `vault.credentialResult`
    /// command. Returns false where there is none.
    fn credential_request(&mut self, _kind: &str, _json: &str) -> bool {
        false
    }

    /// Whether the platform can record other apps' playback and read their
    /// media sessions (Android 10+).
    fn has_capture(&self) -> bool {
        false
    }

    /// Other apps' playback: "start" asks the user to allow recording and
    /// records (the audio arrives through [`crate::capture::push_pcm`], the
    /// state as the `capture.state` command, the other app's media session
    /// as `capture.session`, whether sessions can be read as
    /// `capture.access`), "stop" ends it, "access" asks for access to
    /// media sessions, "control" ({"action": "play", "pause", "toggle",
    /// "next", "prev" or "seek", "position": seconds}) controls the other app.
    fn capture_request(&mut self, _kind: &str, _json: &str) -> bool {
        false
    }

    /// Whether the platform can record its audio inputs (microphones).
    fn has_mic(&self) -> bool {
        false
    }

    /// Microphones: "list" asks for the inputs (they arrive as the
    /// `mic.devices` command: {"devices": [{"id", "name", "channels",
    /// "rate", "default"}]}), "open" ({"stream", "device"}) records input
    /// `device` as stream `stream` (asking the user for permission where the
    /// platform does; the audio arrives through [`crate::mic::push_pcm`],
    /// the state as `mic.state` {"stream", "running", "error"}), "close"
    /// ({"stream"}) stops it. Returns false where there are none.
    fn mic_request(&mut self, _kind: &str, _json: &str) -> bool {
        false
    }

    /// Whether the platform keeps secrets protected for this app (Android
    /// Keystore, Windows DPAPI, the Secret Service on Linux).
    fn has_secret_store(&self) -> bool {
        false
    }

    /// Keeps `value` under `key` in that store; empty deletes it.
    fn secret_store(&mut self, _key: &str, _value: &[u8]) {}

    /// Reads `key` from that store; the value arrives later as the
    /// `vault.secret` command ({"key", "value": base64, empty if none}).
    fn secret_load(&mut self, _key: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::write_private;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Threads (as two app instances would) write the same file at once:
    /// every write succeeds, the file always holds one complete version and
    /// no temporary file stays behind.
    #[test]
    fn concurrent_writes_never_mix_or_fail() {
        let dir = std::env::temp_dir().join(format!("bl-write-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let versions: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 64 * 1024]).collect();
        assert!(write_private(&path, &versions[0]));
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            // A reader sees whole versions only.
            let reader = s.spawn(|| {
                let mut reads = 0;
                while !stop.load(Ordering::Relaxed) {
                    match std::fs::read(&path) {
                        Ok(d) => assert!(versions.contains(&d), "a mixed or partial file"),
                        // Windows may refuse to open a file being replaced.
                        Err(_) if cfg!(windows) => continue,
                        Err(e) => panic!("the file is always there: {e}"),
                    }
                    reads += 1;
                }
                reads
            });
            let writers: Vec<_> = versions
                .iter()
                .map(|v| {
                    let path = &path;
                    s.spawn(move || {
                        for _ in 0..40 {
                            assert!(write_private(path, v));
                        }
                    })
                })
                .collect();
            for w in writers {
                w.join().unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            let reads: u32 = reader.join().unwrap();
            assert!(reads > 0 || cfg!(windows));
        });
        assert!(versions.contains(&std::fs::read(&path).unwrap()));
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["settings.json".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
