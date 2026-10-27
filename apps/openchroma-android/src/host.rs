//! [`Host`] implementation for Android: storage in the activity's internal
//! data directory, shared storage roots for the file browser, AAudio
//! output, and window services (keyboard, clipboard, immersive mode,
//! permissions, file picker) through the Java helper.

use crate::audio;
use crate::ffi;
use crate::java::Java;
use oc_app::host::*;
use oc_app::native::NativeServices;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct AndroidHost {
    pub services: NativeServices,
    pub java: Java,
    audio: Option<audio::AudioOutput>,
    /// Shared storage root, usually `/storage/emulated/0`.
    pub external_root: String,
    pub quit_requested: bool,
    fullscreen: bool,
    /// Cached runtime permission state (updated on resume and on the
    /// permission dialog result).
    pub media_permission: bool,
    // Soft keyboard state, see `sync_keyboard`.
    kb_wanted: bool,
    kb_shown: bool,
    kb_dismissed: bool,
    keep_screen_on: bool,
}

impl AndroidHost {
    pub fn new(data_dir: PathBuf, java: Java) -> AndroidHost {
        let external_root = java.external_storage().unwrap_or_else(|| "/storage/emulated/0".to_string());
        let media_permission = java.has_media_permission();
        AndroidHost {
            services: NativeServices::new(data_dir),
            java,
            audio: None,
            external_root,
            quit_requested: false,
            // MainActivity starts in immersive mode.
            fullscreen: true,
            media_permission,
            kb_wanted: false,
            kb_shown: false,
            kb_dismissed: false,
            keep_screen_on: false,
        }
    }

    /// Applies the keyboard state the app requested during the frame.
    /// `tapped`: a touch started this frame (re-shows a keyboard the user
    /// dismissed while a text field stays focused).
    pub fn sync_keyboard(&mut self, tapped: bool) {
        if !self.kb_wanted {
            self.kb_dismissed = false;
            if self.kb_shown {
                self.kb_shown = false;
                self.java.set_text_input(false);
            }
            return;
        }
        if self.kb_dismissed {
            if !tapped {
                return;
            }
            self.kb_dismissed = false;
        }
        if !self.kb_shown || tapped {
            self.kb_shown = true;
            self.java.set_text_input(true);
        }
    }

    /// Back key while the keyboard is visible: hide it and swallow the key.
    pub fn dismiss_keyboard(&mut self) -> bool {
        if self.kb_shown {
            self.kb_shown = false;
            self.kb_dismissed = true;
            self.java.set_text_input(false);
            return true;
        }
        false
    }

    pub fn set_keep_screen_on(&mut self, on: bool) {
        if on != self.keep_screen_on {
            self.keep_screen_on = on;
            self.java.set_keep_screen_on(on);
        }
    }

    pub fn refresh_permission(&mut self) {
        self.media_permission = self.java.has_media_permission();
    }

    /// Stops audio output (activity destruction).
    pub fn stop_audio(&mut self) {
        self.audio = None;
    }
}

impl Host for AndroidHost {
    fn now(&self) -> f64 {
        self.services.now()
    }
    fn unix_time(&self) -> f64 {
        self.services.unix_time()
    }
    fn random_seed(&self) -> u64 {
        self.services.random_seed()
    }
    fn platform(&self) -> &'static str {
        "android"
    }
    fn storage_read(&self, key: &str) -> Option<Vec<u8>> {
        self.services.storage_read(key)
    }
    fn storage_write(&self, key: &str, data: &[u8]) -> bool {
        self.services.storage_write(key, data)
    }
    fn storage_delete(&self, key: &str) {
        self.services.storage_delete(key)
    }
    fn storage_list(&self, prefix: &str) -> Vec<String> {
        self.services.storage_list(prefix)
    }
    fn has_filesystem(&self) -> bool {
        true
    }
    fn fs_roots(&self) -> Vec<(String, String)> {
        let mut roots = Vec::new();
        if self.media_permission {
            let ext = Path::new(&self.external_root);
            roots.push(("Storage".to_string(), self.external_root.clone()));
            for (label, sub) in [("Music", "Music"), ("Pictures", "Pictures"), ("Downloads", "Download")] {
                let p = ext.join(sub);
                if p.is_dir() {
                    roots.push((label.to_string(), p.to_string_lossy().into_owned()));
                }
            }
            // Removable volumes appear as /storage/XXXX-XXXX.
            if let Ok(rd) = std::fs::read_dir("/storage") {
                let mut extra: Vec<(String, String)> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n != "emulated" && n != "self" && !n.starts_with('.'))
                    .map(|n| (format!("SD {n}"), format!("/storage/{n}")))
                    .filter(|(_, p)| Path::new(p).is_dir())
                    .collect();
                extra.sort();
                roots.extend(extra);
            }
        }
        // Always reachable: the app's own files (presets export, picked
        // files) and its picked-files cache.
        roots.push(("App files".to_string(), self.services.data_dir.to_string_lossy().into_owned()));
        roots
    }
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        self.services.fs_list(path)
    }
    fn fs_open(&self, path: &str) -> Result<Box<dyn oc_media::Source>, String> {
        self.services.fs_open(path)
    }
    fn fs_read(&self, path: &str) -> Result<Vec<u8>, String> {
        self.services.fs_read(path)
    }
    fn fs_metadata(&self, path: &str) -> Option<DirEntry> {
        self.services.fs_metadata(path)
    }
    fn http_request(&mut self, req: HttpRequest) -> u64 {
        self.services.http_request(req)
    }
    fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)> {
        self.services.http_poll()
    }
    fn http_open_stream(&self, url: &str, headers: &[(String, String)]) -> Result<Box<dyn oc_media::Source>, String> {
        self.services.http_open_stream(url, headers)
    }
    fn has_threads(&self) -> bool {
        true
    }
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        self.services.spawn(job)
    }
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String> {
        self.audio = None;
        let (out, info) = audio::start(source)?;
        self.audio = Some(out);
        Ok(info)
    }
    fn set_fullscreen(&mut self, on: bool) {
        if on != self.fullscreen {
            self.fullscreen = on;
            self.java.set_immersive(on);
        }
    }
    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }
    fn set_text_input(&mut self, active: bool) {
        self.kb_wanted = active;
    }
    fn clipboard_get(&self) -> Option<String> {
        self.java.clipboard()
    }
    fn clipboard_set(&mut self, text: &str) {
        self.java.set_clipboard(text);
    }
    fn request_media_permission(&mut self) {
        self.java.request_media_permission();
    }
    fn has_media_permission(&self) -> bool {
        self.media_permission
    }
    fn pick_files(&mut self, accept: &str, multiple: bool) {
        self.java.pick_files(accept, multiple);
    }
    fn quit(&mut self) {
        self.quit_requested = true;
    }
    fn log(&self, msg: &str) {
        ffi::log(ffi::ANDROID_LOG_INFO, msg);
    }
}
