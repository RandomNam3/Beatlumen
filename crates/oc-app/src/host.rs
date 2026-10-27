//! Interface between the platform independent application and the
//! platform layers (Linux, Windows, Android, Web).
//!
//! The platform owns the main loop: it creates the window and GL context,
//! forwards [`Event`]s to [`crate::App::event`] and calls
//! [`crate::App::frame`] once per display frame. Services the application
//! needs from the platform are provided through the [`Host`] trait.

use std::sync::Arc;

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Keyboard modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Super/Windows/Command key.
    pub meta: bool,
}

/// Platform independent key codes (only keys the application uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    F(u8),
    /// Letter or digit keys (upper case letters, '0'..'9').
    Char(char),
    Minus,
    Equals,
    MediaPlayPause,
    MediaNext,
    MediaPrev,
    MediaStop,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    /// Android back button.
    Back,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Start,
    Move,
    End,
    Cancel,
}

/// Input and window events. Coordinates are in physical pixels relative to
/// the top-left corner of the drawable area.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    PointerMove { x: f32, y: f32 },
    PointerDown { x: f32, y: f32, button: Button },
    PointerUp { x: f32, y: f32, button: Button },
    /// The pointer left the window.
    PointerLeave,
    /// Scroll in "lines" (positive dy = scroll content up / wheel away).
    Wheel { dx: f32, dy: f32 },
    /// Pixel precise scroll (touchpads), physical pixels.
    WheelPixels { dx: f32, dy: f32 },
    Touch { id: u64, phase: TouchPhase, x: f32, y: f32 },
    Key { key: Key, pressed: bool, repeat: bool, mods: Mods },
    /// Committed text input (already composed by the IME).
    Text(String),
    Focus(bool),
    /// Files dropped onto the window (native paths).
    DropFiles(Vec<String>),
    /// Files dropped/picked in a browser (name, bytes).
    DropData(Vec<(String, Vec<u8>)>),
    /// The application is being paused (Android) or hidden.
    Suspend,
    Resume,
    /// The user requested to close the window.
    CloseRequested,
}

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
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Audio produced by the application and consumed by the platform audio
/// device (native platforms). Implementations must be real-time friendly.
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

/// Mouse cursor shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Pointer,
    Text,
    Hidden,
}

/// A directory entry for the in-app file browser.
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

    // ---- local file system (None on the web) -----------------------------
    fn has_filesystem(&self) -> bool;
    /// Platform specific roots/bookmarks for the file browser (home, music
    /// folder, drives, external storage).
    fn fs_roots(&self) -> Vec<(String, String)>;
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String>;
    fn fs_open(&self, path: &str) -> Result<Box<dyn oc_media::Source>, String>;
    fn fs_read(&self, path: &str) -> Result<Vec<u8>, String>;
    fn fs_metadata(&self, path: &str) -> Option<DirEntry>;

    // ---- network ----------------------------------------------------------
    /// Starts an asynchronous HTTP request; the result is returned by
    /// [`Host::http_poll`] with the same id.
    fn http_request(&mut self, req: HttpRequest) -> u64;
    fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)>;
    /// Opens a seekable, blocking HTTP stream (range requests). Only
    /// available on native platforms (used from decoder threads).
    fn http_open_stream(&self, url: &str, headers: &[(String, String)]) -> Result<Box<dyn oc_media::Source>, String>;

    // ---- background work --------------------------------------------------
    /// Whether [`Host::spawn`] runs work on other threads.
    fn has_threads(&self) -> bool;
    /// Runs `job` on a background thread (native). Platforms without
    /// threads run it immediately.
    fn spawn(&self, job: Box<dyn FnOnce() + Send>);

    // ---- audio -------------------------------------------------------------
    /// Opens the audio device and starts pulling audio from `source`
    /// (native platforms). Returns the device format.
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String>;
    /// Push based audio (web): queue interleaved stereo samples.
    fn audio_push(&mut self, _samples: &[f32]) {}
    /// Push based audio (web): frames queued but not yet played.
    fn audio_queued(&self) -> u32 {
        0
    }
    /// Push based audio (web): drop all queued audio.
    fn audio_clear(&mut self) {}
    /// Whether audio is pull based ([`Host::audio_start`]) or push based.
    fn audio_is_push(&self) -> bool {
        false
    }

    // ---- window ------------------------------------------------------------
    fn set_cursor(&mut self, _cursor: Cursor) {}
    fn set_fullscreen(&mut self, _on: bool) {}
    fn is_fullscreen(&self) -> bool {
        false
    }
    fn set_title(&mut self, _title: &str) {}
    /// Shows/hides the on-screen keyboard (mobile).
    fn set_text_input(&mut self, _active: bool) {}
    fn clipboard_get(&self) -> Option<String> {
        None
    }
    fn clipboard_set(&mut self, _text: &str) {}
    /// Requests access to media files (Android runtime permission).
    fn request_media_permission(&mut self) {}
    fn has_media_permission(&self) -> bool {
        true
    }
    /// Opens a native file picker for the web (results arrive as
    /// [`Event::DropData`]); `accept` like ".mp3,.flac" or "image/*".
    fn pick_files(&mut self, _accept: &str, _multiple: bool) {}
    /// Asks the platform to exit the main loop.
    fn quit(&mut self) {}
    /// Logs a message (stderr, logcat or the browser console).
    fn log(&self, msg: &str);
}

/// What the application wants after a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameOutput {
    /// Whether another frame should be rendered as soon as possible
    /// (animations running). If false the platform may sleep until the
    /// next event or `wake_in` elapses.
    pub animating: bool,
    /// Wake up after this many seconds at the latest.
    pub wake_in: f64,
}
