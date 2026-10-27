//! Linux platform layer: X11 window, EGL/GLES 3 context, ALSA audio.
//! All system libraries are loaded at runtime with `dlopen`.

mod alsa;
mod dl;
mod egl;
mod x11;

use bl_app::host::*;
use bl_app::native::NativeServices;
use std::ffi::{c_char, c_int, c_long, c_uchar, c_ulong, CStr, CString};
use std::sync::Arc;
use x11::*;

struct LinuxHost {
    services: NativeServices,
    audio: Option<alsa::AudioOutput>,
    xlib: *const Xlib,
    display: *mut Display,
    window: Window,
    atoms: Atoms,
    fullscreen: bool,
    hidden_cursor: x11::Cursor,
    cursors: [x11::Cursor; 3],
    current_cursor: bl_app::host::Cursor,
    quit: bool,
    clipboard: String,
}

#[derive(Clone, Copy)]
struct Atoms {
    wm_delete: Atom,
    net_wm_state: Atom,
    net_wm_state_fullscreen: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_wm_icon: Atom,
    clipboard: Atom,
    targets: Atom,
    bl_selection: Atom,
}

impl LinuxHost {
    fn x(&self) -> &Xlib {
        unsafe { &*self.xlib }
    }
}

impl Host for LinuxHost {
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
        "linux"
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
        bl_app::native::desktop_roots()
    }
    fn fs_list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        self.services.fs_list(path)
    }
    fn fs_open(&self, path: &str) -> Result<Box<dyn bl_media::Source>, String> {
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
    fn http_open_stream(&self, url: &str, headers: &[(String, String)]) -> Result<Box<dyn bl_media::Source>, String> {
        self.services.http_open_stream(url, headers)
    }
    fn has_threads(&self) -> bool {
        true
    }
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        self.services.spawn(job)
    }
    fn audio_start(&mut self, source: Arc<dyn AudioSource>) -> Result<AudioDeviceInfo, String> {
        let (out, info) = alsa::start(source, &|m| eprintln!("{m}"));
        self.audio = Some(out);
        Ok(info)
    }
    fn set_cursor(&mut self, cursor: bl_app::host::Cursor) {
        if cursor == self.current_cursor || self.display.is_null() {
            return;
        }
        self.current_cursor = cursor;
        let c = match cursor {
            bl_app::host::Cursor::Default => self.cursors[0],
            bl_app::host::Cursor::Pointer => self.cursors[1],
            bl_app::host::Cursor::Text => self.cursors[2],
            bl_app::host::Cursor::Hidden => self.hidden_cursor,
        };
        unsafe { (self.x().XDefineCursor)(self.display, self.window, c) };
    }
    fn set_fullscreen(&mut self, on: bool) {
        if self.display.is_null() || on == self.fullscreen {
            return;
        }
        self.fullscreen = on;
        let x = self.x();
        unsafe {
            let mut ev = XEvent { pad: [0; 24] };
            let cm = &mut *(&mut ev as *mut XEvent as *mut XClientMessageEvent);
            cm.type_ = ClientMessage;
            cm.window = self.window;
            cm.message_type = self.atoms.net_wm_state;
            cm.format = 32;
            cm.data[0] = if on { 1 } else { 0 };
            cm.data[1] = self.atoms.net_wm_state_fullscreen as c_long;
            cm.data[3] = 1;
            let root = (x.XRootWindow)(self.display, (x.XDefaultScreen)(self.display));
            (x.XSendEvent)(self.display, root, 0, SubstructureNotifyMask | SubstructureRedirectMask, &mut ev);
            (x.XFlush)(self.display);
        }
    }
    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }
    fn set_title(&mut self, title: &str) {
        if self.display.is_null() {
            return;
        }
        let Ok(c) = CString::new(title) else { return };
        unsafe {
            (self.x().XStoreName)(self.display, self.window, c.as_ptr());
            (self.x().XChangeProperty)(
                self.display,
                self.window,
                self.atoms.net_wm_name,
                self.atoms.utf8_string,
                8,
                PropModeReplace,
                title.as_ptr(),
                title.len() as c_int,
            );
        }
    }
    fn clipboard_get(&self) -> Option<String> {
        if self.display.is_null() {
            return None;
        }
        let x = self.x();
        unsafe {
            let owner = (x.XGetSelectionOwner)(self.display, self.atoms.clipboard);
            if owner == self.window {
                return Some(self.clipboard.clone());
            }
            if owner == 0 {
                return None;
            }
            (x.XConvertSelection)(self.display, self.atoms.clipboard, self.atoms.utf8_string, self.atoms.bl_selection, self.window, CurrentTime);
            (x.XFlush)(self.display);
            // Wait briefly for the SelectionNotify.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while std::time::Instant::now() < deadline {
                while (x.XPending)(self.display) > 0 {
                    let mut ev = XEvent { pad: [0; 24] };
                    (x.XNextEvent)(self.display, &mut ev);
                    if ev.kind() == SelectionNotify {
                        let mut ty: Atom = 0;
                        let mut fmt: c_int = 0;
                        let mut n: c_ulong = 0;
                        let mut after: c_ulong = 0;
                        let mut data: *mut c_uchar = std::ptr::null_mut();
                        (x.XGetWindowProperty)(
                            self.display,
                            self.window,
                            self.atoms.bl_selection,
                            0,
                            1 << 20,
                            1,
                            0,
                            &mut ty,
                            &mut fmt,
                            &mut n,
                            &mut after,
                            &mut data,
                        );
                        if data.is_null() {
                            return None;
                        }
                        let s = String::from_utf8_lossy(std::slice::from_raw_parts(data, n as usize)).into_owned();
                        (x.XFree)(data as *mut _);
                        return Some(s);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            None
        }
    }
    fn clipboard_set(&mut self, text: &str) {
        self.clipboard = text.to_string();
        if !self.display.is_null() {
            unsafe {
                (self.x().XSetSelectionOwner)(self.display, self.atoms.clipboard, self.window, CurrentTime);
            }
        }
    }
    fn quit(&mut self) {
        self.quit = true;
    }
    fn log(&self, msg: &str) {
        eprintln!("[beatlumen] {msg}");
    }
}

fn map_key(sym: KeySym) -> Key {
    match sym {
        XK_Escape => Key::Escape,
        XK_Return | XK_KP_Enter => Key::Enter,
        XK_Tab | XK_ISO_Left_Tab => Key::Tab,
        XK_BackSpace => Key::Backspace,
        XK_Delete => Key::Delete,
        XK_Insert => Key::Insert,
        XK_Left => Key::Left,
        XK_Right => Key::Right,
        XK_Up => Key::Up,
        XK_Down => Key::Down,
        XK_Home => Key::Home,
        XK_End => Key::End,
        XK_Page_Up => Key::PageUp,
        XK_Page_Down => Key::PageDown,
        XK_space => Key::Space,
        XK_minus | XK_KP_Subtract => Key::Minus,
        XK_equal | XK_plus | XK_KP_Add => Key::Equals,
        XF86_AudioPlay | XF86_AudioPause => Key::MediaPlayPause,
        XF86_AudioNext => Key::MediaNext,
        XF86_AudioPrev => Key::MediaPrev,
        XF86_AudioStop => Key::MediaStop,
        XF86_AudioRaiseVolume => Key::VolumeUp,
        XF86_AudioLowerVolume => Key::VolumeDown,
        XF86_AudioMute => Key::VolumeMute,
        s if (XK_F1..=XK_F12).contains(&s) => Key::F((s - XK_F1 + 1) as u8),
        s if (0x61..=0x7a).contains(&s) => Key::Char((s as u8 - 0x20) as char),
        s if (0x41..=0x5a).contains(&s) || (0x30..=0x39).contains(&s) => Key::Char(s as u8 as char),
        _ => Key::Unknown,
    }
}

fn mods(state: u32) -> Mods {
    Mods { shift: state & ShiftMask != 0, ctrl: state & ControlMask != 0, alt: state & Mod1Mask != 0, meta: state & Mod4Mask != 0 }
}

/// Reads Xft.dpi from the X resource database (defaults to 96).
fn dpi_scale(x: &Xlib, display: *mut Display) -> f32 {
    if let Ok(s) = std::env::var("BEATLUMEN_SCALE") {
        if let Ok(v) = s.parse::<f32>() {
            return v.clamp(0.5, 4.0);
        }
    }
    unsafe {
        let rms = (x.XResourceManagerString)(display);
        if !rms.is_null() {
            let s = CStr::from_ptr(rms).to_string_lossy();
            for line in s.lines() {
                if let Some(v) = line.strip_prefix("Xft.dpi:") {
                    if let Ok(dpi) = v.trim().parse::<f32>() {
                        return (dpi / 96.0).clamp(0.5, 4.0);
                    }
                }
            }
        }
    }
    1.0
}

fn atom(x: &Xlib, d: *mut Display, name: &str) -> Atom {
    let c = CString::new(name).unwrap();
    unsafe { (x.XInternAtom)(d, c.as_ptr(), 0) }
}

#[allow(dead_code)]
extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

pub fn run(args: bl_app::LaunchArgs) -> Result<(), String> {
    let xlib = Box::new(Xlib::load()?);
    let x: &Xlib = &xlib;
    unsafe { (x.XInitThreads)() };
    let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
    if display.is_null() {
        return Err("cannot open X display (is DISPLAY set? Wayland users need XWayland)".into());
    }
    let screen = unsafe { (x.XDefaultScreen)(display) };
    let root = unsafe { (x.XRootWindow)(display, screen) };
    let scale = dpi_scale(x, display);
    let (w, h) = args.window_size.unwrap_or(((1280.0 * scale) as u32, (760.0 * scale) as u32));
    let window = unsafe { (x.XCreateSimpleWindow)(display, root, 0, 0, w, h, 0, 0, (x.XBlackPixel)(display, screen)) };
    let atoms = Atoms {
        wm_delete: atom(x, display, "WM_DELETE_WINDOW"),
        net_wm_state: atom(x, display, "_NET_WM_STATE"),
        net_wm_state_fullscreen: atom(x, display, "_NET_WM_STATE_FULLSCREEN"),
        net_wm_name: atom(x, display, "_NET_WM_NAME"),
        utf8_string: atom(x, display, "UTF8_STRING"),
        net_wm_icon: atom(x, display, "_NET_WM_ICON"),
        clipboard: atom(x, display, "CLIPBOARD"),
        targets: atom(x, display, "TARGETS"),
        bl_selection: atom(x, display, "BEATLUMEN_SELECTION"),
    };
    unsafe {
        let mut protocols = [atoms.wm_delete];
        (x.XSetWMProtocols)(display, window, protocols.as_mut_ptr(), 1);
        let name = CString::new("beatlumen").unwrap();
        let class = CString::new("Beatlumen").unwrap();
        let mut hint = XClassHint { res_name: name.as_ptr() as *mut c_char, res_class: class.as_ptr() as *mut c_char };
        (x.XSetClassHint)(display, window, &mut hint);
        (x.XSelectInput)(
            display,
            window,
            KeyPressMask
                | KeyReleaseMask
                | ButtonPressMask
                | ButtonReleaseMask
                | PointerMotionMask
                | LeaveWindowMask
                | EnterWindowMask
                | ExposureMask
                | StructureNotifyMask
                | FocusChangeMask,
        );
        let icon = bl_app::icon_rgba(64);
        let mut data: Vec<c_ulong> = vec![64, 64];
        for p in icon.chunks_exact(4) {
            data.push(((p[3] as c_ulong) << 24) | ((p[0] as c_ulong) << 16) | ((p[1] as c_ulong) << 8) | p[2] as c_ulong);
        }
        (x.XChangeProperty)(display, window, atoms.net_wm_icon, XA_CARDINAL, 32, PropModeReplace, data.as_ptr() as *const c_uchar, data.len() as c_int);
        (x.XMapWindow)(display, window);
        let mut supported = 0;
        (x.XkbSetDetectableAutoRepeat)(display, 1, &mut supported);
    }
    // Input method for proper text input.
    let im = unsafe { (x.XOpenIM)(display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
    let ic = if im.is_null() {
        std::ptr::null_mut()
    } else {
        let style = CString::new("inputStyle").unwrap();
        let client = CString::new("clientWindow").unwrap();
        let focus = CString::new("focusWindow").unwrap();
        const XIM_PREEDIT_NOTHING: c_long = 0x0008;
        const XIM_STATUS_NOTHING: c_long = 0x0400;
        unsafe {
            (x.XCreateIC)(
                im,
                style.as_ptr(),
                XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
                client.as_ptr(),
                window,
                focus.as_ptr(),
                window,
                std::ptr::null::<c_char>(),
            )
        }
    };

    let ctx = egl::EglContext::new(display, window as usize, (0, 0))?;
    ctx.set_vsync(true);
    let gl = ctx.load_gl()?;

    // Cursors.
    let cursors = unsafe { [(x.XCreateFontCursor)(display, 68), (x.XCreateFontCursor)(display, 60), (x.XCreateFontCursor)(display, 152)] };
    let hidden_cursor = unsafe {
        let data = [0 as c_char; 8];
        let pm = (x.XCreateBitmapFromData)(display, window, data.as_ptr(), 8, 8);
        let mut black = [0u16; 6];
        let c = (x.XCreatePixmapCursor)(display, pm, pm, black.as_mut_ptr() as *mut _, black.as_mut_ptr() as *mut _, 0, 0);
        (x.XFreePixmap)(display, pm);
        c
    };

    let data_dir = args.data_dir.clone().unwrap_or_else(bl_app::native::default_data_dir);
    let mut host = LinuxHost {
        services: NativeServices::new(data_dir),
        audio: None,
        xlib: &*xlib as *const Xlib,
        display,
        window,
        atoms,
        fullscreen: false,
        hidden_cursor,
        cursors,
        current_cursor: bl_app::host::Cursor::Default,
        quit: false,
        clipboard: String::new(),
    };
    host.set_title("Beatlumen");
    let mut app = bl_app::App::new(&mut host, args);
    app.gl_init(&gl, &mut host)?;

    let mut size = (w as i32, h as i32);
    let mut last = host.now();
    let fd = unsafe { (x.XConnectionNumber)(display) };
    let mut frames: u64 = 0;
    'main: loop {
        // Events.
        while unsafe { (x.XPending)(display) } > 0 {
            let mut ev = XEvent { pad: [0; 24] };
            unsafe { (x.XNextEvent)(display, &mut ev) };
            if !ic.is_null() && unsafe { (x.XFilterEvent)(&mut ev, 0) } != 0 {
                continue;
            }
            match ev.kind() {
                ClientMessage => {
                    let cm: &XClientMessageEvent = unsafe { ev.cast() };
                    if cm.data[0] as Atom == atoms.wm_delete {
                        app.event(Event::CloseRequested, &mut host);
                    }
                }
                ConfigureNotify => {
                    let c: &XConfigureEvent = unsafe { ev.cast() };
                    size = (c.width, c.height);
                }
                MotionNotify => {
                    let m: &XMotionEvent = unsafe { ev.cast() };
                    app.event(Event::PointerMove { x: m.x as f32, y: m.y as f32 }, &mut host);
                }
                LeaveNotify => app.event(Event::PointerLeave, &mut host),
                ButtonPress | ButtonRelease => {
                    let b: &XButtonEvent = unsafe { ev.cast() };
                    let pressed = ev.kind() == ButtonPress;
                    let (px, py) = (b.x as f32, b.y as f32);
                    match b.button {
                        1 | 2 | 3 => {
                            let button = match b.button {
                                1 => Button::Left,
                                2 => Button::Middle,
                                _ => Button::Right,
                            };
                            app.event(
                                if pressed { Event::PointerDown { x: px, y: py, button } } else { Event::PointerUp { x: px, y: py, button } },
                                &mut host,
                            );
                        }
                        4 if pressed => app.event(Event::Wheel { dx: 0.0, dy: 1.0 }, &mut host),
                        5 if pressed => app.event(Event::Wheel { dx: 0.0, dy: -1.0 }, &mut host),
                        6 if pressed => app.event(Event::Wheel { dx: 1.0, dy: 0.0 }, &mut host),
                        7 if pressed => app.event(Event::Wheel { dx: -1.0, dy: 0.0 }, &mut host),
                        _ => {}
                    }
                }
                KeyPress | KeyRelease => {
                    let pressed = ev.kind() == KeyPress;
                    let k: &mut XKeyEvent = unsafe { &mut *(&mut ev as *mut XEvent as *mut XKeyEvent) };
                    let sym = unsafe { (x.XLookupKeysym)(k, 0) };
                    let m = mods(k.state);
                    app.event(Event::Key { key: map_key(sym), pressed, repeat: false, mods: m }, &mut host);
                    if pressed && !m.ctrl && !m.meta {
                        let mut buf = [0 as c_char; 64];
                        let mut ks: KeySym = 0;
                        let n = unsafe {
                            if !ic.is_null() {
                                let mut status = 0;
                                (x.Xutf8LookupString)(ic, k, buf.as_mut_ptr(), buf.len() as c_int, &mut ks, &mut status)
                            } else {
                                (x.XLookupString)(k, buf.as_mut_ptr(), buf.len() as c_int, &mut ks, std::ptr::null_mut())
                            }
                        };
                        if n > 0 {
                            let bytes: Vec<u8> = buf[..n as usize].iter().map(|&c| c as u8).collect();
                            let text = String::from_utf8_lossy(&bytes).into_owned();
                            if text.chars().all(|c| !c.is_control()) {
                                app.event(Event::Text(text), &mut host);
                            }
                        }
                    }
                }
                FocusIn => {
                    if !ic.is_null() {
                        unsafe { (x.XSetICFocus)(ic) };
                    }
                    app.event(Event::Focus(true), &mut host);
                }
                FocusOut => {
                    if !ic.is_null() {
                        unsafe { (x.XUnsetICFocus)(ic) };
                    }
                    app.event(Event::Focus(false), &mut host);
                }
                SelectionRequest => unsafe {
                    let r: &XSelectionRequestEvent = ev.cast();
                    let mut reply = XEvent { pad: [0; 24] };
                    let s = &mut *(&mut reply as *mut XEvent as *mut XSelectionEvent);
                    s.type_ = SelectionNotify;
                    s.requestor = r.requestor;
                    s.selection = r.selection;
                    s.target = r.target;
                    s.time = r.time;
                    s.property = 0;
                    if r.target == atoms.utf8_string || r.target == XA_STRING {
                        let text = host.clipboard.as_bytes();
                        (x.XChangeProperty)(display, r.requestor, r.property, r.target, 8, PropModeReplace, text.as_ptr(), text.len() as c_int);
                        s.property = r.property;
                    } else if r.target == atoms.targets {
                        let targets = [atoms.utf8_string, XA_STRING];
                        (x.XChangeProperty)(display, r.requestor, r.property, XA_ATOM, 32, PropModeReplace, targets.as_ptr() as *const c_uchar, 2);
                        s.property = r.property;
                    }
                    (x.XSendEvent)(display, r.requestor, 0, 0, &mut reply);
                },
                _ => {}
            }
        }
        if host.quit {
            break 'main;
        }
        let now = host.now();
        let dt = (now - last).clamp(0.0, 0.25);
        last = now;
        let out = app.frame(&gl, &mut host, size.0.max(1) as u32, size.1.max(1) as u32, scale, dt);
        ctx.swap();
        frames += 1;
        if let Some(n) = args_exit_after(frames) {
            if frames >= n {
                break 'main;
            }
        }
        if !out.animating {
            let mut pfd = PollFd { fd, events: 1, revents: 0 };
            let timeout = (out.wake_in * 1000.0).clamp(0.0, 1000.0) as c_int;
            unsafe { poll(&mut pfd, 1, timeout) };
        }
    }
    app.shutdown(&mut host);
    drop(app);
    unsafe {
        if !ic.is_null() {
            (x.XDestroyIC)(ic);
        }
        if !im.is_null() {
            (x.XCloseIM)(im);
        }
    }
    drop(ctx);
    unsafe {
        (x.XDestroyWindow)(display, window);
        (x.XCloseDisplay)(display);
    }
    Ok(())
}

/// `BEATLUMEN_EXIT_AFTER_FRAMES` (testing): exit after N frames.
fn args_exit_after(_frames: u64) -> Option<u64> {
    static VALUE: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| std::env::var("BEATLUMEN_EXIT_AFTER_FRAMES").ok().and_then(|v| v.parse().ok()))
}
