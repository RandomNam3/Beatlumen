//! C interface of the Beatlumen core, used by the Qt user interface
//! (`apps/beatlumen/src/core.h` declares the same functions).
//!
//! All functions except [`bl_audio_render`] must be called from the UI
//! thread that created the core. Strings returned by the core are UTF-8,
//! NUL terminated and must be released with [`bl_string_free`].

#![allow(clippy::missing_safety_doc)]

use bl_core::host::{AudioDeviceInfo, AudioSource, DirEntry, Host, HttpRequest, HttpResponse};
use bl_core::{Core, LaunchArgs};
use bl_gl::{Flavor, Gl};
use bl_json::Value;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Callbacks into the UI layer. Unused entries may be null.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlCallbacks {
    pub user: *mut c_void,
    /// Opens the audio output (stereo float, pull model through
    /// [`bl_audio_render`]); returns the device sample rate or 0 on failure.
    pub audio_start: Option<unsafe extern "C" fn(user: *mut c_void) -> u32>,
    /// Logs a message.
    pub log: Option<unsafe extern "C" fn(user: *mut c_void, msg: *const c_char)>,
    /// Starts an HTTP request (platforms without sockets); the UI answers
    /// with [`bl_core_http_complete`]. `headers` is a JSON array of pairs.
    pub http_request: Option<
        unsafe extern "C" fn(
            user: *mut c_void,
            id: u64,
            method: *const c_char,
            url: *const c_char,
            headers: *const c_char,
            body: *const u8,
            len: usize,
        ),
    >,
    /// Push audio (platforms without threads): queue interleaved stereo samples.
    pub audio_push:
        Option<unsafe extern "C" fn(user: *mut c_void, samples: *const f32, count: usize)>,
    pub audio_queued: Option<unsafe extern "C" fn(user: *mut c_void) -> u32>,
    pub audio_clear: Option<unsafe extern "C" fn(user: *mut c_void)>,
    pub audio_set_paused: Option<unsafe extern "C" fn(user: *mut c_void, paused: i32)>,
    /// Push audio: output gain (volume × normalization), applied smoothly.
    pub audio_set_gain: Option<unsafe extern "C" fn(user: *mut c_void, gain: f32)>,
}

/// The audio source pulled by the UI's audio thread.
static AUDIO: Mutex<Option<Arc<dyn AudioSource>>> = Mutex::new(None);

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: callers pass NUL terminated strings (documented C contract).
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn out_string(s: String) -> *mut c_char {
    CString::new(s.replace('\0', ""))
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

struct UiHost {
    cb: BlCallbacks,
    platform: &'static str,
    data_dir: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    native: bl_core::native::NativeServices,
    start: std::time::Instant,
    next_http: u64,
    http_done: Vec<(u64, Result<HttpResponse, String>)>,
    #[cfg(not(target_arch = "wasm32"))]
    use_ui_http: bool,
}

fn key_path(dir: &std::path::Path, key: &str) -> Option<PathBuf> {
    let mut p = dir.to_path_buf();
    for part in key.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains(':')
        {
            return None;
        }
        p.push(part);
    }
    Some(p)
}

impl UiHost {
    fn android_roots(&self) -> Vec<(String, String)> {
        let mut roots = Vec::new();
        let base = "/storage/emulated/0";
        roots.push(("Music".to_string(), format!("{base}/Music")));
        roots.push(("Downloads".to_string(), format!("{base}/Download")));
        roots.push(("Pictures".to_string(), format!("{base}/Pictures")));
        roots.push(("Storage".to_string(), base.to_string()));
        if let Ok(rd) = std::fs::read_dir("/storage") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name != "emulated" && name != "self" {
                    roots.push((
                        format!("SD {name}"),
                        e.path().to_string_lossy().into_owned(),
                    ));
                }
            }
        }
        roots
    }
}

impl Host for UiHost {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
    fn unix_time(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
    fn random_seed(&self) -> u64 {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        t ^ (&t as *const u64 as u64).rotate_left(29)
    }
    fn platform(&self) -> &'static str {
        self.platform
    }
    fn storage_read(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(key_path(&self.data_dir, key)?).ok()
    }
    fn storage_write(&self, key: &str, data: &[u8]) -> bool {
        let Some(path) = key_path(&self.data_dir, key) else {
            return false;
        };
        bl_core::host::write_private(&path, data)
    }
    fn storage_delete(&self, key: &str) {
        if let Some(p) = key_path(&self.data_dir, key) {
            let _ = std::fs::remove_file(p);
        }
    }
    fn storage_list(&self, prefix: &str) -> Vec<String> {
        let (dir, file_prefix) = match prefix.rfind('/') {
            Some(i) => (&prefix[..i], &prefix[i + 1..]),
            None => ("", prefix),
        };
        let base = if dir.is_empty() {
            Some(self.data_dir.clone())
        } else {
            key_path(&self.data_dir, dir)
        };
        let mut out = Vec::new();
        if let Some(Ok(rd)) = base.map(std::fs::read_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(file_prefix)
                    && e.file_type().map(|t| t.is_file()).unwrap_or(false)
                {
                    out.push(if dir.is_empty() {
                        name
                    } else {
                        format!("{dir}/{name}")
                    });
                }
            }
        }
        out.sort();
        out
    }
    fn storage_path(&self, key: &str) -> Option<String> {
        key_path(&self.data_dir, key).map(|p| p.to_string_lossy().into_owned())
    }
    fn has_filesystem(&self) -> bool {
        self.platform != "web"
    }
    fn fs_roots(&self) -> Vec<(String, String)> {
        match self.platform {
            "android" => self.android_roots(),
            "web" => Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            _ => bl_core::native::desktop_roots(),
            #[cfg(target_arch = "wasm32")]
            _ => Vec::new(),
        }
    }
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let rd = std::fs::read_dir(path).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            let Ok(md) = std::fs::metadata(&p) else {
                continue;
            };
            out.push(DirEntry {
                name,
                path: p.to_string_lossy().into_owned(),
                is_dir: md.is_dir(),
                size: md.len(),
                modified: 0,
            });
        }
        out.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }
    fn fs_open(&self, path: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
        Ok(Box::new(std::io::BufReader::with_capacity(64 * 1024, f)))
    }
    fn fs_read(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }
    fn fs_metadata(&self, path: &str) -> Option<DirEntry> {
        let md = std::fs::metadata(path).ok()?;
        let modified = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(DirEntry {
            name,
            path: path.to_string(),
            is_dir: md.is_dir(),
            size: md.len(),
            modified,
        })
    }
    fn http_request(&mut self, req: HttpRequest) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.use_ui_http {
            return self.native.http_request(req);
        }
        self.next_http += 1;
        let id = self.next_http | (1 << 62);
        match self.cb.http_request {
            Some(f) => {
                let headers = Value::Array(
                    req.headers
                        .iter()
                        .map(|(k, v)| {
                            Value::Array(vec![Value::from(k.as_str()), Value::from(v.as_str())])
                        })
                        .collect(),
                )
                .to_json();
                let (m, u, h) = (
                    CString::new(req.method).unwrap_or_default(),
                    CString::new(req.url).unwrap_or_default(),
                    CString::new(headers).unwrap_or_default(),
                );
                // SAFETY: valid pointers for the duration of the call.
                unsafe {
                    f(
                        self.cb.user,
                        id,
                        m.as_ptr(),
                        u.as_ptr(),
                        h.as_ptr(),
                        req.body.as_ptr(),
                        req.body.len(),
                    )
                };
            }
            None => self
                .http_done
                .push((id, Err("HTTP is not available".into()))),
        }
        id
    }
    fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)> {
        #[allow(unused_mut)]
        let mut out = std::mem::take(&mut self.http_done);
        #[cfg(not(target_arch = "wasm32"))]
        out.extend(self.native.http_poll());
        out
    }
    fn has_threads(&self) -> bool {
        cfg!(not(target_arch = "wasm32"))
    }
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        #[cfg(not(target_arch = "wasm32"))]
        self.native.spawn(job);
        #[cfg(target_arch = "wasm32")]
        job();
    }
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String> {
        *AUDIO.lock().map_err(|_| "audio lock poisoned")? = Some(source);
        let rate = match self.cb.audio_start {
            // SAFETY: UI callback contract.
            Some(f) => unsafe { f(self.cb.user) },
            None => 0,
        };
        if rate == 0 {
            return Err("no audio output".into());
        }
        Ok(AudioDeviceInfo {
            sample_rate: rate,
            channels: 2,
        })
    }
    fn audio_push(&mut self, samples: &[f32]) {
        if let Some(f) = self.cb.audio_push {
            // SAFETY: UI callback contract.
            unsafe { f(self.cb.user, samples.as_ptr(), samples.len()) }
        }
    }
    fn audio_queued(&self) -> u32 {
        // SAFETY: UI callback contract.
        self.cb
            .audio_queued
            .map(|f| unsafe { f(self.cb.user) })
            .unwrap_or(0)
    }
    fn audio_clear(&mut self) {
        if let Some(f) = self.cb.audio_clear {
            // SAFETY: UI callback contract.
            unsafe { f(self.cb.user) }
        }
    }
    fn audio_set_paused(&mut self, paused: bool) {
        if let Some(f) = self.cb.audio_set_paused {
            // SAFETY: UI callback contract.
            unsafe { f(self.cb.user, paused as i32) }
        }
    }
    fn audio_set_gain(&mut self, gain: f32) {
        if let Some(f) = self.cb.audio_set_gain {
            // SAFETY: UI callback contract.
            unsafe { f(self.cb.user, gain) }
        }
    }
    fn audio_is_push(&self) -> bool {
        self.cb.audio_push.is_some()
    }
    fn log(&self, msg: &str) {
        match self.cb.log {
            Some(f) => {
                let c = CString::new(msg.replace('\0', "")).unwrap_or_default();
                // SAFETY: UI callback contract.
                unsafe { f(self.cb.user, c.as_ptr()) }
            }
            None => eprintln!("{msg}"),
        }
    }
}

/// Opaque handle owned by the UI.
pub struct BlCore {
    host: UiHost,
    core: Core,
    gl: Option<Gl>,
}

/// Creates the core. `args_json` is an array of command line arguments,
/// `platform` one of "linux", "windows", "macos", "android", "web".
#[no_mangle]
pub unsafe extern "C" fn bl_core_new(
    data_dir: *const c_char,
    args_json: *const c_char,
    language: *const c_char,
    platform: *const c_char,
    cb: BlCallbacks,
) -> *mut BlCore {
    let platform: &'static str = match cstr(platform).as_str() {
        "android" => "android",
        "windows" => "windows",
        "macos" => "macos",
        "web" => "web",
        _ => "linux",
    };
    let data_dir = PathBuf::from(cstr(data_dir));
    let _ = std::fs::create_dir_all(&data_dir);
    let args: Vec<String> = bl_json::parse(&cstr(args_json))
        .ok()
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default();
    let launch = LaunchArgs::parse(args.into_iter()).unwrap_or_default();
    let mut host = UiHost {
        cb,
        platform,
        #[cfg(not(target_arch = "wasm32"))]
        native: bl_core::native::NativeServices::new(data_dir.clone()),
        data_dir,
        start: std::time::Instant::now(),
        next_http: 0,
        http_done: Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        use_ui_http: false,
    };
    let core = Core::new(&mut host, launch, &cstr(language));
    Box::into_raw(Box::new(BlCore {
        host,
        core,
        gl: None,
    }))
}

#[no_mangle]
pub unsafe extern "C" fn bl_core_free(c: *mut BlCore) {
    if !c.is_null() {
        let mut b = Box::from_raw(c);
        let BlCore { host, core, .. } = &mut *b;
        core.shutdown(host);
        if let Ok(mut a) = AUDIO.lock() {
            *a = None;
        }
    }
}

/// Executes a JSON command and returns the JSON result.
#[no_mangle]
pub unsafe extern "C" fn bl_core_command(c: *mut BlCore, json: *const c_char) -> *mut c_char {
    let Some(c) = c.as_mut() else {
        return std::ptr::null_mut();
    };
    let cmd = bl_json::parse(&cstr(json)).unwrap_or(Value::Null);
    let r = c.core.command(&mut c.host, &cmd);
    out_string(r.to_json())
}

/// Advances the core (network, playback, jobs) and returns the events since
/// the last call as a JSON array.
#[no_mangle]
pub unsafe extern "C" fn bl_core_tick(c: *mut BlCore) -> *mut c_char {
    let Some(c) = c.as_mut() else {
        return std::ptr::null_mut();
    };
    c.core.tick(&mut c.host);
    out_string(c.core.take_events().to_json())
}

/// Playback position in seconds.
#[no_mangle]
pub unsafe extern "C" fn bl_core_position(c: *mut BlCore) -> f64 {
    c.as_ref().map(|c| c.core.position(&c.host)).unwrap_or(0.0)
}

/// Whether audio is playing (keep the screen awake, show media controls).
#[no_mangle]
pub unsafe extern "C" fn bl_core_is_playing(c: *mut BlCore) -> i32 {
    c.as_ref().map(|c| c.core.is_playing() as i32).unwrap_or(0)
}

/// Loads GL with `get_proc` (a context must be current). `flavor`: 0 =
/// desktop OpenGL 3.3 core, 1 = OpenGL ES 3 / WebGL 2. Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn bl_core_gl_init(
    c: *mut BlCore,
    get_proc: Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *const c_void>,
    ctx: *mut c_void,
    flavor: i32,
) -> i32 {
    let Some(c) = c.as_mut() else { return 1 };
    let Some(get_proc) = get_proc else { return 1 };
    let mut get = |name: &str| {
        let n = CString::new(name).unwrap_or_default();
        get_proc(ctx, n.as_ptr())
    };
    let gl = match Gl::load(
        if flavor == 0 {
            Flavor::Core33
        } else {
            Flavor::Gles3
        },
        &mut get,
    ) {
        Ok(gl) => gl,
        Err(e) => {
            c.host.log(&format!("OpenGL: {e}"));
            return 2;
        }
    };
    if let Err(e) = c.core.gl_init(&gl, &mut c.host) {
        c.host.log(&format!("visualizer: {e}"));
        return 3;
    }
    c.gl = Some(gl);
    0
}

/// The GL context was destroyed; GL resources are dropped without calls.
#[no_mangle]
pub unsafe extern "C" fn bl_core_gl_lost(c: *mut BlCore) {
    if let Some(c) = c.as_mut() {
        c.core.gl_lost();
        c.gl = None;
    }
}

/// Renders the visualizer into framebuffer `fbo` (physical pixel size).
#[no_mangle]
pub unsafe extern "C" fn bl_core_render(
    c: *mut BlCore,
    fbo: u32,
    w: u32,
    h: u32,
    dpr: f32,
    dt: f64,
) {
    let Some(c) = c.as_mut() else { return };
    let Some(gl) = &c.gl else { return };
    c.core.render(gl, &mut c.host, fbo, w, h, dpr, dt);
}

/// Completes a request started through `BlCallbacks::http_request`.
/// `status` 0 means a network error described by `error`.
#[no_mangle]
pub unsafe extern "C" fn bl_core_http_complete(
    c: *mut BlCore,
    id: u64,
    status: i32,
    headers_json: *const c_char,
    body: *const u8,
    len: usize,
    error: *const c_char,
) {
    let Some(c) = c.as_mut() else { return };
    let res = if status <= 0 {
        Err(cstr(error))
    } else {
        let headers = bl_json::parse(&cstr(headers_json))
            .ok()
            .and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .map(|p| {
                            (
                                p.at(0).as_str().unwrap_or("").to_string(),
                                p.at(1).as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        let body = if body.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(body, len).to_vec()
        };
        Ok(HttpResponse {
            status: status as u16,
            headers,
            body,
        })
    };
    c.host.http_done.push((id, res));
}

/// Opens a file given as bytes (browser uploads, Android content URIs).
#[no_mangle]
pub unsafe extern "C" fn bl_core_open_data(
    c: *mut BlCore,
    name: *const c_char,
    data: *const u8,
    len: usize,
) {
    let Some(c) = c.as_mut() else { return };
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    c.core.open_data(&mut c.host, cstr(name), bytes);
}

/// Saves settings (call when the app is suspended or closed).
#[no_mangle]
pub unsafe extern "C" fn bl_core_save(c: *mut BlCore) {
    if let Some(c) = c.as_mut() {
        c.core.save_settings(&mut c.host);
    }
}

/// Fills `out` with `frames` interleaved stereo frames. Called from the
/// audio thread; `latency_frames` is the output's queued audio.
#[no_mangle]
pub unsafe extern "C" fn bl_audio_render(out: *mut f32, frames: usize, latency_frames: u32) {
    if out.is_null() {
        return;
    }
    let buf = std::slice::from_raw_parts_mut(out, frames * 2);
    let src = AUDIO.lock().ok().and_then(|a| a.clone());
    match src {
        Some(s) => s.render(buf, latency_frames),
        None => buf.fill(0.0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bl_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
