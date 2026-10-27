//! Minimal Xlib bindings loaded at runtime.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use super::dl::Library;
use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};

pub type Display = c_void;
pub type Window = c_ulong;
pub type Atom = c_ulong;
pub type KeySym = c_ulong;
pub type Cursor = c_ulong;
pub type Pixmap = c_ulong;
pub type Time = c_ulong;
pub type Bool = c_int;
pub type Status = c_int;
pub type XIM = *mut c_void;
pub type XIC = *mut c_void;

pub const KeyPress: c_int = 2;
pub const KeyRelease: c_int = 3;
pub const ButtonPress: c_int = 4;
pub const ButtonRelease: c_int = 5;
pub const MotionNotify: c_int = 6;
pub const EnterNotify: c_int = 7;
pub const LeaveNotify: c_int = 8;
pub const FocusIn: c_int = 9;
pub const FocusOut: c_int = 10;
pub const Expose: c_int = 12;
pub const ConfigureNotify: c_int = 22;
pub const SelectionClear: c_int = 29;
pub const SelectionRequest: c_int = 30;
pub const SelectionNotify: c_int = 31;
pub const ClientMessage: c_int = 33;

pub const KeyPressMask: c_long = 1 << 0;
pub const KeyReleaseMask: c_long = 1 << 1;
pub const ButtonPressMask: c_long = 1 << 2;
pub const ButtonReleaseMask: c_long = 1 << 3;
pub const EnterWindowMask: c_long = 1 << 4;
pub const LeaveWindowMask: c_long = 1 << 5;
pub const PointerMotionMask: c_long = 1 << 6;
pub const ExposureMask: c_long = 1 << 15;
pub const StructureNotifyMask: c_long = 1 << 17;
pub const FocusChangeMask: c_long = 1 << 21;
pub const PropertyChangeMask: c_long = 1 << 22;
pub const SubstructureNotifyMask: c_long = 1 << 19;
pub const SubstructureRedirectMask: c_long = 1 << 20;

pub const ShiftMask: c_uint = 1;
pub const ControlMask: c_uint = 4;
pub const Mod1Mask: c_uint = 8;
pub const Mod4Mask: c_uint = 64;

pub const PropModeReplace: c_int = 0;
pub const XA_ATOM: Atom = 4;
pub const XA_CARDINAL: Atom = 6;
pub const XA_STRING: Atom = 31;
pub const CurrentTime: Time = 0;
pub const None_: c_ulong = 0;

/// XEvent union (24 longs).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XEvent {
    pub pad: [c_long; 24],
}

impl XEvent {
    pub fn kind(&self) -> c_int {
        unsafe { *(self as *const XEvent as *const c_int) }
    }
    /// Reinterprets the event as a specific event struct.
    ///
    /// # Safety
    /// `T` must be the struct matching `kind()`.
    pub unsafe fn cast<T>(&self) -> &T {
        &*(self as *const XEvent as *const T)
    }
}

#[repr(C)]
pub struct XKeyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub keycode: c_uint,
    pub same_screen: Bool,
}

#[repr(C)]
pub struct XButtonEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub button: c_uint,
    pub same_screen: Bool,
}

#[repr(C)]
pub struct XMotionEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub is_hint: c_char,
    pub same_screen: Bool,
}

#[repr(C)]
pub struct XConfigureEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub border_width: c_int,
    pub above: Window,
    pub override_redirect: Bool,
}

#[repr(C)]
pub struct XClientMessageEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub window: Window,
    pub message_type: Atom,
    pub format: c_int,
    pub data: [c_long; 5],
}

#[repr(C)]
pub struct XSelectionRequestEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub owner: Window,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
    pub time: Time,
}

#[repr(C)]
pub struct XSelectionEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: Bool,
    pub display: *mut Display,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
    pub time: Time,
}

#[repr(C)]
pub struct XClassHint {
    pub res_name: *mut c_char,
    pub res_class: *mut c_char,
}

macro_rules! xlib_fns {
    ($( $name:ident : fn( $($arg:ty),* ) $(-> $ret:ty)? ; )*) => {
        pub struct Xlib {
            pub lib: Library,
            $( pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
            pub XCreateIC: unsafe extern "C" fn(XIM, ...) -> XIC,
        }
        impl Xlib {
            pub fn load() -> Result<Xlib, String> {
                let lib = Library::open(&["libX11.so.6", "libX11.so"])?;
                unsafe {
                    Ok(Xlib {
                        $( $name: lib.sym(stringify!($name))?, )*
                        XCreateIC: lib.sym("XCreateIC")?,
                        lib,
                    })
                }
            }
        }
    };
}

xlib_fns! {
    XInitThreads: fn() -> Status;
    XOpenDisplay: fn(*const c_char) -> *mut Display;
    XCloseDisplay: fn(*mut Display) -> c_int;
    XDefaultScreen: fn(*mut Display) -> c_int;
    XRootWindow: fn(*mut Display, c_int) -> Window;
    XBlackPixel: fn(*mut Display, c_int) -> c_ulong;
    XCreateSimpleWindow: fn(*mut Display, Window, c_int, c_int, c_uint, c_uint, c_uint, c_ulong, c_ulong) -> Window;
    XDestroyWindow: fn(*mut Display, Window) -> c_int;
    XStoreName: fn(*mut Display, Window, *const c_char) -> c_int;
    XSetClassHint: fn(*mut Display, Window, *mut XClassHint) -> c_int;
    XInternAtom: fn(*mut Display, *const c_char, Bool) -> Atom;
    XSetWMProtocols: fn(*mut Display, Window, *mut Atom, c_int) -> Status;
    XSelectInput: fn(*mut Display, Window, c_long) -> c_int;
    XMapWindow: fn(*mut Display, Window) -> c_int;
    XPending: fn(*mut Display) -> c_int;
    XNextEvent: fn(*mut Display, *mut XEvent) -> c_int;
    XFilterEvent: fn(*mut XEvent, Window) -> Bool;
    XLookupKeysym: fn(*mut XKeyEvent, c_int) -> KeySym;
    XLookupString: fn(*mut XKeyEvent, *mut c_char, c_int, *mut KeySym, *mut c_void) -> c_int;
    XFlush: fn(*mut Display) -> c_int;
    XSync: fn(*mut Display, Bool) -> c_int;
    XConnectionNumber: fn(*mut Display) -> c_int;
    XChangeProperty: fn(*mut Display, Window, Atom, Atom, c_int, c_int, *const c_uchar, c_int) -> c_int;
    XSendEvent: fn(*mut Display, Window, Bool, c_long, *mut XEvent) -> Status;
    XCreateFontCursor: fn(*mut Display, c_uint) -> Cursor;
    XDefineCursor: fn(*mut Display, Window, Cursor) -> c_int;
    XUndefineCursor: fn(*mut Display, Window) -> c_int;
    XCreateBitmapFromData: fn(*mut Display, Window, *const c_char, c_uint, c_uint) -> Pixmap;
    XCreatePixmapCursor: fn(*mut Display, Pixmap, Pixmap, *mut c_void, *mut c_void, c_uint, c_uint) -> Cursor;
    XFreePixmap: fn(*mut Display, Pixmap) -> c_int;
    XResourceManagerString: fn(*mut Display) -> *const c_char;
    XOpenIM: fn(*mut Display, *mut c_void, *mut c_char, *mut c_char) -> XIM;
    XCloseIM: fn(XIM) -> Status;
    XDestroyIC: fn(XIC);
    XSetICFocus: fn(XIC);
    XUnsetICFocus: fn(XIC);
    Xutf8LookupString: fn(XIC, *mut XKeyEvent, *mut c_char, c_int, *mut KeySym, *mut Status) -> c_int;
    XkbSetDetectableAutoRepeat: fn(*mut Display, Bool, *mut Bool) -> Bool;
    XSetSelectionOwner: fn(*mut Display, Atom, Window, Time) -> c_int;
    XGetSelectionOwner: fn(*mut Display, Atom) -> Window;
    XConvertSelection: fn(*mut Display, Atom, Atom, Atom, Window, Time) -> c_int;
    XGetWindowProperty: fn(*mut Display, Window, Atom, c_long, c_long, Bool, Atom, *mut Atom, *mut c_int, *mut c_ulong, *mut c_ulong, *mut *mut c_uchar) -> c_int;
    XFree: fn(*mut c_void) -> c_int;
    XDeleteProperty: fn(*mut Display, Window, Atom) -> c_int;
    XTranslateCoordinates: fn(*mut Display, Window, Window, c_int, c_int, *mut c_int, *mut c_int, *mut Window) -> Bool;
}

// Keysyms used by the application.
pub const XK_BackSpace: KeySym = 0xff08;
pub const XK_Tab: KeySym = 0xff09;
pub const XK_ISO_Left_Tab: KeySym = 0xfe20;
pub const XK_Return: KeySym = 0xff0d;
pub const XK_KP_Enter: KeySym = 0xff8d;
pub const XK_Escape: KeySym = 0xff1b;
pub const XK_Delete: KeySym = 0xffff;
pub const XK_Insert: KeySym = 0xff63;
pub const XK_Home: KeySym = 0xff50;
pub const XK_Left: KeySym = 0xff51;
pub const XK_Up: KeySym = 0xff52;
pub const XK_Right: KeySym = 0xff53;
pub const XK_Down: KeySym = 0xff54;
pub const XK_Page_Up: KeySym = 0xff55;
pub const XK_Page_Down: KeySym = 0xff56;
pub const XK_End: KeySym = 0xff57;
pub const XK_F1: KeySym = 0xffbe;
pub const XK_F12: KeySym = 0xffc9;
pub const XK_space: KeySym = 0x20;
pub const XK_minus: KeySym = 0x2d;
pub const XK_equal: KeySym = 0x3d;
pub const XK_plus: KeySym = 0x2b;
pub const XK_KP_Add: KeySym = 0xffab;
pub const XK_KP_Subtract: KeySym = 0xffad;
pub const XF86_AudioLowerVolume: KeySym = 0x1008ff11;
pub const XF86_AudioMute: KeySym = 0x1008ff12;
pub const XF86_AudioRaiseVolume: KeySym = 0x1008ff13;
pub const XF86_AudioPlay: KeySym = 0x1008ff14;
pub const XF86_AudioStop: KeySym = 0x1008ff15;
pub const XF86_AudioPrev: KeySym = 0x1008ff16;
pub const XF86_AudioNext: KeySym = 0x1008ff17;
pub const XF86_AudioPause: KeySym = 0x1008ff31;
