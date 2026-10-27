//! The app thread: owns the `oc_app::App`, the EGL context and the host,
//! processes lifecycle commands and input, and renders while the activity
//! is resumed and has a window.

use crate::activity::{Cmd, Shared};
use crate::egl::{Egl, Status};
use crate::ffi::*;
use crate::host::AndroidHost;
use crate::input::{self, keycode, PointerState};
use crate::java::Java;
use crate::jni::JniEnv;
use oc_app::host::{Event, Host, Key, Mods};
use oc_app::App;
use std::ffi::{c_int, CStr};
use std::path::PathBuf;

const LOOPER_ID_MAIN: c_int = 1;
const LOOPER_ID_INPUT: c_int = 2;

/// Everything the loop owns besides the app and host.
struct Platform {
    activity: *mut ANativeActivity,
    looper: *mut ALooper,
    window: *mut ANativeWindow,
    input_queue: *mut AInputQueue,
    egl: Option<Egl>,
    gl: Option<oc_gl::Gl>,
    /// `App::gl_init` succeeded for the current context.
    gl_ready: bool,
    resumed: bool,
    scale: f32,
    pointer: PointerState,
    touch_started: bool,
    last_frame: f64,
    swap_error_logged: bool,
    finishing: bool,
    first_run: bool,
    permission_asked: bool,
    /// Poll timeout while rendering: 0 when animating, else the app's
    /// requested wake-up delay.
    idle_timeout: c_int,
}

impl Platform {
    /// Reads the screen density (dpi / 160 = UI scale).
    fn update_density(&mut self) {
        // SAFETY: the asset manager belongs to the live activity.
        unsafe {
            let cfg = AConfiguration_new();
            if cfg.is_null() {
                return;
            }
            AConfiguration_fromAssetManager(cfg, (*self.activity).assetManager);
            let d = AConfiguration_getDensity(cfg);
            AConfiguration_delete(cfg);
            let dpi = match d {
                ACONFIGURATION_DENSITY_DEFAULT | ACONFIGURATION_DENSITY_ANY | ACONFIGURATION_DENSITY_NONE => 160,
                d => d,
            };
            self.scale = (dpi as f32 / 160.0).clamp(0.75, 6.0);
        }
    }

    /// (Re)creates the window surface and GL state.
    fn attach_window(&mut self, app: &mut App, host: &mut AndroidHost) {
        if self.window.is_null() {
            return;
        }
        if self.egl.is_none() {
            match Egl::new() {
                Ok(e) => self.egl = Some(e),
                Err(e) => {
                    log(ANDROID_LOG_ERROR, &format!("EGL: {e}"));
                    return;
                }
            }
        }
        let Some(egl) = self.egl.as_mut() else { return };
        // SAFETY: the window is acquired and stays valid until detach.
        let status = match unsafe { egl.attach(self.window) } {
            Ok(s) => s,
            Err(e) => {
                log(ANDROID_LOG_ERROR, &format!("EGL: {e}"));
                return;
            }
        };
        if status == Status::ContextRecreated && self.gl_ready {
            app.gl_lost();
            self.gl_ready = false;
            self.gl = None;
        }
        if !self.gl_ready {
            match egl.load_gl() {
                Ok(gl) => {
                    match app.gl_init(&gl, host) {
                        Ok(()) => self.gl_ready = true,
                        Err(e) => log(ANDROID_LOG_ERROR, &format!("GL init failed: {e}")),
                    }
                    self.gl = Some(gl);
                }
                Err(e) => log(ANDROID_LOG_ERROR, &format!("GL load failed: {e}")),
            }
        }
        self.last_frame = host.now();
    }

    fn detach_window(&mut self) {
        if let Some(egl) = self.egl.as_mut() {
            egl.detach();
        }
        if !self.window.is_null() {
            // SAFETY: acquired in WindowCreated.
            unsafe { ANativeWindow_release(self.window) };
            self.window = std::ptr::null_mut();
        }
    }

    fn detach_input_queue(&mut self) {
        if !self.input_queue.is_null() {
            // SAFETY: attached to our looper in InputQueueCreated.
            unsafe { AInputQueue_detachLooper(self.input_queue) };
            self.input_queue = std::ptr::null_mut();
        }
    }

    fn can_render(&self) -> bool {
        self.resumed && !self.window.is_null() && self.gl_ready && self.egl.as_ref().map(|e| e.has_surface()).unwrap_or(false)
    }
}

/// Seeds the settings on first launch: library folder = shared Music.
fn seed_first_run_settings(host: &AndroidHost) -> bool {
    use oc_app::settings::Settings;
    if host.storage_read(Settings::STORAGE_KEY).is_some() {
        return false;
    }
    let s = Settings { library_folders: vec![format!("{}/Music", host.external_root.trim_end_matches('/'))], ..Settings::default() };
    host.storage_write(Settings::STORAGE_KEY, s.to_json().to_json_pretty().as_bytes());
    true
}

/// Runs the application until the activity is destroyed.
///
/// # Safety
/// `activity` must stay valid until this returns (guaranteed by
/// `onDestroy` waiting for the thread); `env` belongs to this thread.
pub unsafe fn run(activity: *mut ANativeActivity, env: JniEnv, shared: &Shared) {
    let looper = ALooper_prepare(ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
    ALooper_addFd(looper, shared.wake_read, LOOPER_ID_MAIN, ALOOPER_EVENT_INPUT, std::ptr::null(), std::ptr::null_mut());

    let data_dir = if (*activity).internalDataPath.is_null() {
        PathBuf::from("/data/local/tmp/beatlumen")
    } else {
        PathBuf::from(CStr::from_ptr((*activity).internalDataPath).to_string_lossy().into_owned())
    };
    let java = Java::new(env, (*activity).clazz);
    let mut host = AndroidHost::new(data_dir, java);
    let first_run = seed_first_run_settings(&host);
    host.log(&format!("Beatlumen starting (SDK {}, data {})", (*activity).sdkVersion, host.services.data_dir.display()));
    let mut app = App::new(&mut host, oc_app::LaunchArgs::default());

    let mut p = Platform {
        activity,
        looper,
        window: std::ptr::null_mut(),
        input_queue: std::ptr::null_mut(),
        egl: None,
        gl: None,
        gl_ready: false,
        resumed: false,
        scale: 1.0,
        pointer: PointerState::default(),
        touch_started: false,
        last_frame: host.now(),
        swap_error_logged: false,
        finishing: false,
        first_run,
        permission_asked: false,
        idle_timeout: 0,
    };
    p.update_density();

    'main: loop {
        let timeout = if p.can_render() { p.idle_timeout } else { -1 };
        let mut fd = 0;
        let mut events = 0;
        let mut data = std::ptr::null_mut();
        ALooper_pollOnce(timeout, &mut fd, &mut events, &mut data);

        // Lifecycle and Java commands.
        for (seq, cmd) in shared.take() {
            let stop = handle_cmd(cmd, &mut p, &mut app, &mut host);
            shared.done(seq);
            if stop {
                break 'main;
            }
        }
        // Input.
        if !p.input_queue.is_null() {
            process_input(&mut p, &mut app, &mut host);
        }
        if host.quit_requested && !p.finishing {
            p.finishing = true;
            ANativeActivity_finish(p.activity);
        }
        if p.can_render() {
            render(&mut p, &mut app, &mut host);
        }
    }

    app.shutdown(&mut host);
    host.stop_audio();
    drop(app);
    p.gl = None;
    p.detach_input_queue();
    p.detach_window();
    p.egl = None;
    ALooper_removeFd(looper, shared.wake_read);
    host.log("Beatlumen stopped");
}

/// Handles one command. Returns true to leave the loop (destroy).
unsafe fn handle_cmd(cmd: Cmd, p: &mut Platform, app: &mut App, host: &mut AndroidHost) -> bool {
    match cmd {
        Cmd::WindowCreated(w) => {
            p.detach_window();
            if !w.0.is_null() {
                ANativeWindow_acquire(w.0);
                p.window = w.0;
                p.attach_window(app, host);
            }
        }
        Cmd::WindowDestroyed => p.detach_window(),
        Cmd::WindowResized => {}
        Cmd::InputQueueCreated(q) => {
            p.detach_input_queue();
            if !q.0.is_null() {
                AInputQueue_attachLooper(q.0, p.looper, LOOPER_ID_INPUT, std::ptr::null(), std::ptr::null_mut());
                p.input_queue = q.0;
            }
        }
        Cmd::InputQueueDestroyed => p.detach_input_queue(),
        Cmd::Start | Cmd::Stop | Cmd::LowMemory => {}
        Cmd::Resume => {
            p.resumed = true;
            p.last_frame = host.now();
            let had = host.media_permission;
            host.refresh_permission();
            if host.media_permission && !had {
                // Granted in the system settings while we were paused.
                app.rescan_local_library();
            }
            app.event(Event::Resume, host);
            if p.first_run && !p.permission_asked && !host.media_permission {
                p.permission_asked = true;
                host.request_media_permission();
            }
        }
        Cmd::Pause => {
            p.resumed = false;
            app.event(Event::Suspend, host);
        }
        Cmd::Focus(f) => app.event(Event::Focus(f), host),
        Cmd::ConfigChanged => p.update_density(),
        Cmd::Destroy => return true,
        Cmd::Text(t) => app.event(Event::Text(t), host),
        Cmd::ImeKey(code) => {
            let key = input::map_keycode(code);
            if key != Key::Unknown {
                let mods = Mods::default();
                app.event(Event::Key { key, pressed: true, repeat: false, mods }, host);
                app.event(Event::Key { key, pressed: false, repeat: false, mods }, host);
            }
        }
        Cmd::Permission(granted) => {
            let had = host.media_permission;
            host.media_permission = granted;
            if granted && !had {
                app.rescan_local_library();
            }
        }
        Cmd::Files(paths) => app.event(Event::DropFiles(paths), host),
    }
    false
}

/// Drains the input queue.
unsafe fn process_input(p: &mut Platform, app: &mut App, host: &mut AndroidHost) {
    let q = p.input_queue;
    let mut ev: *mut AInputEvent = std::ptr::null_mut();
    while AInputQueue_getEvent(q, &mut ev) >= 0 {
        if ev.is_null() {
            break;
        }
        if AInputQueue_preDispatchEvent(q, ev) != 0 {
            // Consumed by the IME; it may come back later.
            continue;
        }
        let handled = match AInputEvent_getType(ev) {
            AINPUT_EVENT_TYPE_MOTION => {
                let m = input::translate_motion(ev, &mut p.pointer);
                p.touch_started |= m.touch_started;
                for e in m.events {
                    app.event(e, host);
                }
                true
            }
            AINPUT_EVENT_TYPE_KEY => handle_key(ev, app, host),
            _ => false,
        };
        AInputQueue_finishEvent(q, ev, handled as c_int);
    }
}

/// Handles a key event; returns whether it was consumed (unconsumed keys
/// go on to the system, e.g. volume keys change the media volume).
unsafe fn handle_key(ev: *const AInputEvent, app: &mut App, host: &mut AndroidHost) -> bool {
    let action = AKeyEvent_getAction(ev);
    if action != AKEY_EVENT_ACTION_DOWN && action != AKEY_EVENT_ACTION_UP {
        return false;
    }
    let code = AKeyEvent_getKeyCode(ev);
    let meta = AKeyEvent_getMetaState(ev);
    let pressed = action == AKEY_EVENT_ACTION_DOWN;
    let repeat = AKeyEvent_getRepeatCount(ev) > 0;
    match code {
        // Let the system adjust the media stream volume.
        keycode::VOLUME_UP | keycode::VOLUME_DOWN | keycode::VOLUME_MUTE => return false,
        keycode::BACK => {
            if AKeyEvent_getFlags(ev) & AKEY_EVENT_FLAG_CANCELED != 0 {
                return true;
            }
            // Back first hides the soft keyboard.
            if pressed && host.dismiss_keyboard() {
                return true;
            }
        }
        // Play/pause-only media keys depend on the current state.
        keycode::MEDIA_PLAY if app.is_playing() => return true,
        keycode::MEDIA_PAUSE if !app.is_playing() => return true,
        _ => {}
    }
    let key = input::map_keycode(code);
    let mods = input::mods(meta);
    let mut handled = false;
    if key != Key::Unknown {
        app.event(Event::Key { key, pressed, repeat, mods }, host);
        handled = true;
    }
    if pressed && !mods.ctrl && !mods.meta {
        let c = host.java.unicode_char(AInputEvent_getDeviceId(ev), code, meta);
        // Bit 31 marks a dead key (combining accent); ignored.
        if c > 0 {
            if let Some(ch) = char::from_u32(c as u32).filter(|c| !c.is_control()) {
                app.event(Event::Text(ch.to_string()), host);
                handled = true;
            }
        }
    }
    handled
}

unsafe fn render(p: &mut Platform, app: &mut App, host: &mut AndroidHost) {
    let (Some(egl), Some(gl)) = (p.egl.as_ref(), p.gl.as_ref()) else { return };
    let (w, h) = egl.surface_size();
    if w <= 0 || h <= 0 {
        return;
    }
    let now = host.now();
    let dt = (now - p.last_frame).clamp(0.0, 0.25);
    p.last_frame = now;
    let out = app.frame(gl, host, w as u32, h as u32, p.scale, dt);
    p.idle_timeout = if out.animating { 0 } else { (out.wake_in * 1000.0).clamp(0.0, 1000.0) as c_int };
    match egl.swap() {
        Ok(()) => p.swap_error_logged = false,
        Err(true) => {
            log(ANDROID_LOG_WARN, "EGL context lost; recreating");
            app.gl_lost();
            p.gl_ready = false;
            p.gl = None;
            if let Some(egl) = p.egl.as_mut() {
                if let Err(e) = egl.reset_context() {
                    log(ANDROID_LOG_ERROR, &format!("EGL: {e}"));
                    p.egl = None;
                }
            }
            p.attach_window(app, host);
        }
        Err(false) => {
            if !p.swap_error_logged {
                p.swap_error_logged = true;
                log(ANDROID_LOG_WARN, "eglSwapBuffers failed");
            }
        }
    }
    let tapped = std::mem::take(&mut p.touch_started);
    host.sync_keyboard(tapped);
    host.set_keep_screen_on(app.is_playing());
}
