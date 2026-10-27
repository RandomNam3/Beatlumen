//! Hand-written bindings for the NDK C APIs used by the platform layer:
//! native activity, looper, input queue, native window, configuration,
//! logging, EGL and a few libc functions.
//!
//! Only the functions and constants that are actually used are declared.
//! Struct layouts mirror `<android/native_activity.h>` of NDK r27 (the
//! layout has been stable since API 9).
//!
//! # Safety
//! Everything here is raw FFI. Callers must uphold the documented NDK
//! contracts (valid pointers, calling thread restrictions).

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{c_char, c_int, c_void};

// ---- opaque handles ----------------------------------------------------------

#[repr(C)]
pub struct ANativeWindow {
    _private: [u8; 0],
}
#[repr(C)]
pub struct AInputQueue {
    _private: [u8; 0],
}
#[repr(C)]
pub struct AInputEvent {
    _private: [u8; 0],
}
#[repr(C)]
pub struct ALooper {
    _private: [u8; 0],
}
#[repr(C)]
pub struct AConfiguration {
    _private: [u8; 0],
}
#[repr(C)]
pub struct AAssetManager {
    _private: [u8; 0],
}

/// `JNIEnv` is a pointer to the function table; see [`crate::jni`].
pub type JNIEnv = *const *const c_void;
/// `JavaVM` is a pointer to the invoke interface table.
pub type JavaVM = *const *const c_void;
pub type jobject = *mut c_void;

#[repr(C)]
pub struct ARect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct ANativeActivityCallbacks {
    pub onStart: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onResume: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onSaveInstanceState: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut usize) -> *mut c_void>,
    pub onPause: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onStop: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onDestroy: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onWindowFocusChanged: Option<unsafe extern "C" fn(*mut ANativeActivity, c_int)>,
    pub onNativeWindowCreated: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut ANativeWindow)>,
    pub onNativeWindowResized: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut ANativeWindow)>,
    pub onNativeWindowRedrawNeeded: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut ANativeWindow)>,
    pub onNativeWindowDestroyed: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut ANativeWindow)>,
    pub onInputQueueCreated: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut AInputQueue)>,
    pub onInputQueueDestroyed: Option<unsafe extern "C" fn(*mut ANativeActivity, *mut AInputQueue)>,
    pub onContentRectChanged: Option<unsafe extern "C" fn(*mut ANativeActivity, *const ARect)>,
    pub onConfigurationChanged: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
    pub onLowMemory: Option<unsafe extern "C" fn(*mut ANativeActivity)>,
}

#[repr(C)]
pub struct ANativeActivity {
    pub callbacks: *mut ANativeActivityCallbacks,
    pub vm: *mut JavaVM,
    pub env: *mut JNIEnv,
    /// Global reference to the Java `NativeActivity` (our `MainActivity`).
    pub clazz: jobject,
    pub internalDataPath: *const c_char,
    pub externalDataPath: *const c_char,
    pub sdkVersion: i32,
    pub instance: *mut c_void,
    pub assetManager: *mut AAssetManager,
    pub obbPath: *const c_char,
}

// ---- constants -------------------------------------------------------------------

pub const ALOOPER_PREPARE_ALLOW_NON_CALLBACKS: c_int = 1;
pub const ALOOPER_EVENT_INPUT: c_int = 1;
pub const ALOOPER_POLL_WAKE: c_int = -1;
pub const ALOOPER_POLL_CALLBACK: c_int = -2;
pub const ALOOPER_POLL_TIMEOUT: c_int = -3;
pub const ALOOPER_POLL_ERROR: c_int = -4;

pub const AINPUT_EVENT_TYPE_KEY: i32 = 1;
pub const AINPUT_EVENT_TYPE_MOTION: i32 = 2;

pub const AKEY_EVENT_ACTION_DOWN: i32 = 0;
pub const AKEY_EVENT_ACTION_UP: i32 = 1;
pub const AKEY_EVENT_ACTION_MULTIPLE: i32 = 2;
pub const AKEY_EVENT_FLAG_CANCELED: i32 = 0x20;

pub const AMETA_SHIFT_ON: i32 = 0x01;
pub const AMETA_ALT_ON: i32 = 0x02;
pub const AMETA_CTRL_ON: i32 = 0x1000;
pub const AMETA_META_ON: i32 = 0x10000;

pub const AMOTION_EVENT_ACTION_MASK: i32 = 0xff;
pub const AMOTION_EVENT_ACTION_POINTER_INDEX_MASK: i32 = 0xff00;
pub const AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT: i32 = 8;
pub const AMOTION_EVENT_ACTION_DOWN: i32 = 0;
pub const AMOTION_EVENT_ACTION_UP: i32 = 1;
pub const AMOTION_EVENT_ACTION_MOVE: i32 = 2;
pub const AMOTION_EVENT_ACTION_CANCEL: i32 = 3;
pub const AMOTION_EVENT_ACTION_POINTER_DOWN: i32 = 5;
pub const AMOTION_EVENT_ACTION_POINTER_UP: i32 = 6;
pub const AMOTION_EVENT_ACTION_HOVER_MOVE: i32 = 7;
pub const AMOTION_EVENT_ACTION_SCROLL: i32 = 8;
pub const AMOTION_EVENT_ACTION_HOVER_ENTER: i32 = 9;
pub const AMOTION_EVENT_ACTION_HOVER_EXIT: i32 = 10;
pub const AMOTION_EVENT_ACTION_BUTTON_PRESS: i32 = 11;
pub const AMOTION_EVENT_ACTION_BUTTON_RELEASE: i32 = 12;

pub const AMOTION_EVENT_BUTTON_PRIMARY: i32 = 1;
pub const AMOTION_EVENT_BUTTON_SECONDARY: i32 = 2;
pub const AMOTION_EVENT_BUTTON_TERTIARY: i32 = 4;

pub const AMOTION_EVENT_AXIS_VSCROLL: i32 = 9;
pub const AMOTION_EVENT_AXIS_HSCROLL: i32 = 10;

pub const AMOTION_EVENT_TOOL_TYPE_FINGER: i32 = 1;
pub const AMOTION_EVENT_TOOL_TYPE_STYLUS: i32 = 2;
pub const AMOTION_EVENT_TOOL_TYPE_MOUSE: i32 = 3;
pub const AMOTION_EVENT_TOOL_TYPE_ERASER: i32 = 4;

pub const AINPUT_SOURCE_CLASS_POINTER: i32 = 0x2;
pub const AINPUT_SOURCE_MOUSE: i32 = 0x2002;
pub const AINPUT_SOURCE_TOUCHPAD: i32 = 0x0010_0008;

pub const ACONFIGURATION_DENSITY_DEFAULT: i32 = 0;
pub const ACONFIGURATION_DENSITY_ANY: i32 = 0xfffe;
pub const ACONFIGURATION_DENSITY_NONE: i32 = 0xffff;

pub const ANDROID_LOG_INFO: c_int = 4;
pub const ANDROID_LOG_WARN: c_int = 5;
pub const ANDROID_LOG_ERROR: c_int = 6;

// ---- libandroid -------------------------------------------------------------------

#[link(name = "android")]
extern "C" {
    pub fn ANativeActivity_finish(activity: *mut ANativeActivity);

    pub fn ALooper_prepare(opts: c_int) -> *mut ALooper;
    pub fn ALooper_pollOnce(timeout_ms: c_int, out_fd: *mut c_int, out_events: *mut c_int, out_data: *mut *mut c_void) -> c_int;
    pub fn ALooper_wake(looper: *mut ALooper);
    pub fn ALooper_addFd(
        looper: *mut ALooper,
        fd: c_int,
        ident: c_int,
        events: c_int,
        callback: *const c_void,
        data: *mut c_void,
    ) -> c_int;
    pub fn ALooper_removeFd(looper: *mut ALooper, fd: c_int) -> c_int;

    pub fn AInputQueue_attachLooper(queue: *mut AInputQueue, looper: *mut ALooper, ident: c_int, callback: *const c_void, data: *mut c_void);
    pub fn AInputQueue_detachLooper(queue: *mut AInputQueue);
    pub fn AInputQueue_getEvent(queue: *mut AInputQueue, out_event: *mut *mut AInputEvent) -> i32;
    pub fn AInputQueue_preDispatchEvent(queue: *mut AInputQueue, event: *mut AInputEvent) -> i32;
    pub fn AInputQueue_finishEvent(queue: *mut AInputQueue, event: *mut AInputEvent, handled: c_int);

    pub fn AInputEvent_getType(event: *const AInputEvent) -> i32;
    pub fn AInputEvent_getSource(event: *const AInputEvent) -> i32;
    pub fn AInputEvent_getDeviceId(event: *const AInputEvent) -> i32;

    pub fn AKeyEvent_getAction(event: *const AInputEvent) -> i32;
    pub fn AKeyEvent_getFlags(event: *const AInputEvent) -> i32;
    pub fn AKeyEvent_getKeyCode(event: *const AInputEvent) -> i32;
    pub fn AKeyEvent_getMetaState(event: *const AInputEvent) -> i32;
    pub fn AKeyEvent_getRepeatCount(event: *const AInputEvent) -> i32;

    pub fn AMotionEvent_getAction(event: *const AInputEvent) -> i32;
    pub fn AMotionEvent_getButtonState(event: *const AInputEvent) -> i32;
    pub fn AMotionEvent_getPointerCount(event: *const AInputEvent) -> usize;
    pub fn AMotionEvent_getPointerId(event: *const AInputEvent, pointer_index: usize) -> i32;
    pub fn AMotionEvent_getToolType(event: *const AInputEvent, pointer_index: usize) -> i32;
    pub fn AMotionEvent_getX(event: *const AInputEvent, pointer_index: usize) -> f32;
    pub fn AMotionEvent_getY(event: *const AInputEvent, pointer_index: usize) -> f32;
    pub fn AMotionEvent_getAxisValue(event: *const AInputEvent, axis: i32, pointer_index: usize) -> f32;

    pub fn ANativeWindow_acquire(window: *mut ANativeWindow);
    pub fn ANativeWindow_release(window: *mut ANativeWindow);
    pub fn ANativeWindow_getWidth(window: *mut ANativeWindow) -> i32;
    pub fn ANativeWindow_getHeight(window: *mut ANativeWindow) -> i32;
    pub fn ANativeWindow_setBuffersGeometry(window: *mut ANativeWindow, width: i32, height: i32, format: i32) -> i32;

    pub fn AConfiguration_new() -> *mut AConfiguration;
    pub fn AConfiguration_delete(config: *mut AConfiguration);
    pub fn AConfiguration_fromAssetManager(out: *mut AConfiguration, am: *mut AAssetManager);
    pub fn AConfiguration_getDensity(config: *mut AConfiguration) -> i32;
}

// ---- liblog -------------------------------------------------------------------------

#[link(name = "log")]
extern "C" {
    pub fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// Writes one line to logcat with the tag "Beatlumen".
pub fn log(prio: c_int, msg: &str) {
    // Interior NULs would truncate the message; replace them.
    let mut bytes: Vec<u8> = msg.bytes().map(|b| if b == 0 { b'?' } else { b }).collect();
    bytes.push(0);
    // SAFETY: both strings are NUL terminated and live for the call.
    unsafe { __android_log_write(prio, c"Beatlumen".as_ptr(), bytes.as_ptr() as *const c_char) };
}

// ---- libc ---------------------------------------------------------------------------

pub const O_NONBLOCK: c_int = 0o4000;
pub const O_CLOEXEC: c_int = 0o2000000;
pub const CLOCK_MONOTONIC: c_int = 1;

#[repr(C)]
pub struct timespec {
    pub tv_sec: std::ffi::c_long,
    pub tv_nsec: std::ffi::c_long,
}

extern "C" {
    pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    pub fn close(fd: c_int) -> c_int;
    pub fn clock_gettime(clock: c_int, ts: *mut timespec) -> c_int;
}

#[link(name = "dl")]
extern "C" {
    pub fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
pub const RTLD_NOW: c_int = 2;

/// Monotonic clock in nanoseconds (the clock AAudio timestamps use).
// `c_long` is 32 bits on armv7, so the conversions are needed there.
#[allow(clippy::useless_conversion)]
pub fn monotonic_nanos() -> i64 {
    let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out pointer.
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    i64::from(ts.tv_sec) * 1_000_000_000 + i64::from(ts.tv_nsec)
}
