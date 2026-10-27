//! Audio output through AAudio (API 26+): a shared, low-latency float
//! stereo stream whose data callback pulls samples from the application's
//! [`AudioSource`].
//!
//! When the device disconnects (headphones unplugged, Bluetooth switch),
//! AAudio invokes the error callback; the stream is then closed and
//! reopened on a helper thread (closing from inside the callback is not
//! allowed). The reopened stream requests the original sample rate so the
//! application's rate stays valid (AAudio resamples if needed).

#![allow(non_camel_case_types)]

use crate::ffi;
use oc_app::host::{AudioDeviceInfo, AudioSource};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

#[repr(C)]
struct AAudioStreamBuilder {
    _private: [u8; 0],
}
#[repr(C)]
struct AAudioStream {
    _private: [u8; 0],
}

type aaudio_result_t = i32;
type DataCallback = unsafe extern "C" fn(*mut AAudioStream, *mut c_void, *mut c_void, i32) -> i32;
type ErrorCallback = unsafe extern "C" fn(*mut AAudioStream, *mut c_void, aaudio_result_t);

const AAUDIO_OK: aaudio_result_t = 0;
const AAUDIO_DIRECTION_OUTPUT: i32 = 0;
const AAUDIO_FORMAT_PCM_FLOAT: i32 = 2;
const AAUDIO_SHARING_MODE_SHARED: i32 = 1;
const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: i32 = 12;
const AAUDIO_CALLBACK_RESULT_CONTINUE: i32 = 0;
const AAUDIO_ERROR_DISCONNECTED: aaudio_result_t = -899;

#[link(name = "aaudio")]
extern "C" {
    fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> aaudio_result_t;
    fn AAudio_convertResultToText(r: aaudio_result_t) -> *const std::ffi::c_char;
    fn AAudioStreamBuilder_setDirection(b: *mut AAudioStreamBuilder, d: i32);
    fn AAudioStreamBuilder_setFormat(b: *mut AAudioStreamBuilder, f: i32);
    fn AAudioStreamBuilder_setChannelCount(b: *mut AAudioStreamBuilder, n: i32);
    fn AAudioStreamBuilder_setSampleRate(b: *mut AAudioStreamBuilder, rate: i32);
    fn AAudioStreamBuilder_setSharingMode(b: *mut AAudioStreamBuilder, m: i32);
    fn AAudioStreamBuilder_setPerformanceMode(b: *mut AAudioStreamBuilder, m: i32);
    fn AAudioStreamBuilder_setDataCallback(b: *mut AAudioStreamBuilder, cb: DataCallback, user: *mut c_void);
    fn AAudioStreamBuilder_setErrorCallback(b: *mut AAudioStreamBuilder, cb: ErrorCallback, user: *mut c_void);
    fn AAudioStreamBuilder_openStream(b: *mut AAudioStreamBuilder, s: *mut *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStreamBuilder_delete(b: *mut AAudioStreamBuilder) -> aaudio_result_t;
    fn AAudioStream_requestStart(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_requestStop(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_close(s: *mut AAudioStream) -> aaudio_result_t;
    fn AAudioStream_getSampleRate(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getChannelCount(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getFramesPerBurst(s: *mut AAudioStream) -> i32;
    fn AAudioStream_setBufferSizeInFrames(s: *mut AAudioStream, n: i32) -> aaudio_result_t;
    fn AAudioStream_getBufferSizeInFrames(s: *mut AAudioStream) -> i32;
    fn AAudioStream_getFramesWritten(s: *mut AAudioStream) -> i64;
    fn AAudioStream_getTimestamp(s: *mut AAudioStream, clock: i32, frame_pos: *mut i64, time_ns: *mut i64) -> aaudio_result_t;
}

fn result_text(r: aaudio_result_t) -> String {
    // SAFETY: AAudio returns a static string.
    unsafe {
        let p = AAudio_convertResultToText(r);
        if p.is_null() {
            format!("AAudio error {r}")
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// State shared with the AAudio callbacks. Lives in an `Arc` whose raw
/// pointer is the callbacks' user data; it outlives every stream because
/// streams are closed before the last reference is dropped.
struct Inner {
    source: Arc<dyn AudioSource>,
    stream: Mutex<StreamPtr>,
    /// Sample rate promised to the application (set once after opening).
    rate: AtomicI32,
    /// A reopen is in progress (avoid spawning several).
    reopening: AtomicBool,
    closed: AtomicBool,
}

struct StreamPtr(*mut AAudioStream);
// SAFETY: AAudio stream handles may be used from any thread; access is
// serialized by the mutex.
unsafe impl Send for StreamPtr {}

/// Running audio output; closing happens on drop.
pub struct AudioOutput {
    inner: Arc<Inner>,
}

/// Opens a stream for `inner`. `rate` 0 = device default.
fn open_stream(inner: &Arc<Inner>, rate: i32) -> Result<*mut AAudioStream, String> {
    // SAFETY: standard AAudio builder sequence; the user data pointer is
    // the `Inner` kept alive by `AudioOutput` until all streams are closed.
    unsafe {
        let mut b: *mut AAudioStreamBuilder = std::ptr::null_mut();
        let r = AAudio_createStreamBuilder(&mut b);
        if r != AAUDIO_OK || b.is_null() {
            return Err(format!("AAudio_createStreamBuilder: {}", result_text(r)));
        }
        AAudioStreamBuilder_setDirection(b, AAUDIO_DIRECTION_OUTPUT);
        AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_FLOAT);
        AAudioStreamBuilder_setChannelCount(b, 2);
        if rate > 0 {
            AAudioStreamBuilder_setSampleRate(b, rate);
        }
        AAudioStreamBuilder_setSharingMode(b, AAUDIO_SHARING_MODE_SHARED);
        AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
        let user = Arc::as_ptr(inner) as *mut c_void;
        AAudioStreamBuilder_setDataCallback(b, data_callback, user);
        AAudioStreamBuilder_setErrorCallback(b, error_callback, user);
        let mut s: *mut AAudioStream = std::ptr::null_mut();
        let r = AAudioStreamBuilder_openStream(b, &mut s);
        AAudioStreamBuilder_delete(b);
        if r != AAUDIO_OK || s.is_null() {
            return Err(format!("AAudioStreamBuilder_openStream: {}", result_text(r)));
        }
        // Double buffering of bursts: low latency without glitches on most
        // devices.
        let burst = AAudioStream_getFramesPerBurst(s);
        if burst > 0 {
            AAudioStream_setBufferSizeInFrames(s, burst * 2);
        }
        Ok(s)
    }
}

/// Opens the default output device and starts pulling from `source`.
pub fn start(source: Arc<dyn AudioSource>) -> Result<(AudioOutput, AudioDeviceInfo), String> {
    let inner = Arc::new(Inner {
        source,
        stream: Mutex::new(StreamPtr(std::ptr::null_mut())),
        rate: AtomicI32::new(0),
        reopening: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    let s = open_stream(&inner, 0)?;
    // SAFETY: valid stream.
    let (rate, channels) = unsafe { (AAudioStream_getSampleRate(s), AAudioStream_getChannelCount(s)) };
    inner.rate.store(rate, Ordering::SeqCst);
    // SAFETY: valid stream.
    let r = unsafe { AAudioStream_requestStart(s) };
    if r != AAUDIO_OK {
        // SAFETY: valid stream, not used afterwards.
        unsafe { AAudioStream_close(s) };
        return Err(format!("AAudioStream_requestStart: {}", result_text(r)));
    }
    if let Ok(mut g) = inner.stream.lock() {
        g.0 = s;
    }
    ffi::log(ffi::ANDROID_LOG_INFO, &format!("AAudio: {rate} Hz, {channels} channels"));
    Ok((AudioOutput { inner }, AudioDeviceInfo { sample_rate: rate.max(1) as u32, channels: 2 }))
}

/// Estimated frames between the write position and the speaker.
///
/// # Safety
/// `s` must be a valid, open stream.
unsafe fn latency_frames(s: *mut AAudioStream, rate: i32) -> u32 {
    let written = AAudioStream_getFramesWritten(s);
    let (mut pos, mut t) = (0i64, 0i64);
    if AAudioStream_getTimestamp(s, ffi::CLOCK_MONOTONIC, &mut pos, &mut t) == AAUDIO_OK && rate > 0 {
        let elapsed = (ffi::monotonic_nanos() - t).max(0);
        let presented_now = pos + elapsed * rate as i64 / 1_000_000_000;
        return (written - presented_now).clamp(0, rate as i64) as u32;
    }
    AAudioStream_getBufferSizeInFrames(s).max(0) as u32
}

unsafe extern "C" fn data_callback(stream: *mut AAudioStream, user: *mut c_void, data: *mut c_void, frames: i32) -> i32 {
    // SAFETY: `user` is the `Inner` that outlives the stream; `data` holds
    // `frames * channels` floats as requested in the builder.
    let inner = &*(user as *const Inner);
    let frames = frames.max(0) as usize;
    let channels = AAudioStream_getChannelCount(stream).max(1) as usize;
    let out = std::slice::from_raw_parts_mut(data as *mut f32, frames * channels);
    if inner.closed.load(Ordering::Relaxed) {
        out.fill(0.0);
        return AAUDIO_CALLBACK_RESULT_CONTINUE;
    }
    let latency = latency_frames(stream, inner.rate.load(Ordering::Relaxed));
    // A panic must not unwind into AAudio's thread.
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if channels == 2 {
            inner.source.render(out, latency);
        } else {
            // Render stereo in chunks and map to the device layout.
            let mut tmp = [0.0f32; 512];
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(tmp.len() / 2);
                inner.source.render(&mut tmp[..n * 2], latency);
                for i in 0..n {
                    let (l, r) = (tmp[i * 2], tmp[i * 2 + 1]);
                    let o = &mut out[(done + i) * channels..(done + i + 1) * channels];
                    if channels == 1 {
                        o[0] = 0.5 * (l + r);
                    } else {
                        o.fill(0.0);
                        o[0] = l;
                        o[1] = r;
                    }
                }
                done += n;
            }
        }
    }));
    if ok.is_err() {
        out.fill(0.0);
    }
    AAUDIO_CALLBACK_RESULT_CONTINUE
}

unsafe extern "C" fn error_callback(_stream: *mut AAudioStream, user: *mut c_void, error: aaudio_result_t) {
    // SAFETY: see data_callback.
    let inner = &*(user as *const Inner);
    let what = if error == AAUDIO_ERROR_DISCONNECTED { "device disconnected".to_string() } else { result_text(error) };
    ffi::log(ffi::ANDROID_LOG_WARN, &format!("AAudio error callback: {what}; reopening the stream"));
    if inner.closed.load(Ordering::SeqCst) || inner.reopening.swap(true, Ordering::SeqCst) {
        return;
    }
    // Re-acquire a strong reference: the pointer comes from an Arc that
    // is alive (AudioOutput::drop sets `closed` and closes streams under
    // the mutex before releasing it).
    Arc::increment_strong_count(user as *const Inner);
    let inner: Arc<Inner> = Arc::from_raw(user as *const Inner);
    let _ = std::thread::Builder::new().name("aaudio-reopen".into()).spawn(move || reopen(inner));
}

fn reopen(inner: Arc<Inner>) {
    let Ok(mut g) = inner.stream.lock() else { return };
    if inner.closed.load(Ordering::SeqCst) {
        inner.reopening.store(false, Ordering::SeqCst);
        return;
    }
    if !g.0.is_null() {
        // SAFETY: the stream belongs to us; not inside a callback.
        unsafe {
            AAudioStream_requestStop(g.0);
            AAudioStream_close(g.0);
        }
        g.0 = std::ptr::null_mut();
    }
    // Retry a few times: the new route may need a moment.
    for attempt in 0..5 {
        match open_stream(&inner, inner.rate.load(Ordering::SeqCst)) {
            Ok(s) => {
                // SAFETY: valid stream.
                let r = unsafe { AAudioStream_requestStart(s) };
                if r == AAUDIO_OK {
                    g.0 = s;
                    ffi::log(ffi::ANDROID_LOG_INFO, "AAudio stream reopened");
                    break;
                }
                // SAFETY: valid stream, not used afterwards.
                unsafe { AAudioStream_close(s) };
            }
            Err(e) => ffi::log(ffi::ANDROID_LOG_WARN, &format!("AAudio reopen attempt {attempt}: {e}")),
        }
        std::thread::sleep(std::time::Duration::from_millis(200 * (attempt + 1)));
    }
    inner.reopening.store(false, Ordering::SeqCst);
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.inner.stream.lock() {
            if !g.0.is_null() {
                // SAFETY: stop + close are allowed outside the callbacks;
                // after close no callback references `inner` any more.
                unsafe {
                    AAudioStream_requestStop(g.0);
                    AAudioStream_close(g.0);
                }
                g.0 = std::ptr::null_mut();
            }
        }
    }
}
