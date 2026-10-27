//! Beatlumen for Android: the platform layer around `oc_app::App`.
//!
//! The library is built as `libbeatlumen.so` and loaded by
//! `io.github.randomnam3.beatlumen.MainActivity` (a thin subclass of
//! `android.app.NativeActivity`, see `java/`). Structure:
//!
//! * [`activity`] – `ANativeActivity_onCreate`, the lifecycle callbacks and
//!   the command queue to the app thread (no `android_native_app_glue`).
//! * [`main_loop`] – the app thread: ALooper, input queue, EGL, frames.
//! * [`host`] – the `oc_app::host::Host` implementation.
//! * [`audio`] – AAudio output.
//! * [`egl`], [`input`], [`java`], [`jni`], [`ffi`] – bindings/helpers.
//!
//! On other targets the crate is empty so `cargo build` of the whole
//! workspace keeps working on desktop hosts.
//!
//! `unsafe` is confined to FFI (NDK C APIs, EGL, AAudio, JNI); each block
//! documents the contract it relies on.

#[cfg(target_os = "android")]
mod activity;
#[cfg(target_os = "android")]
mod audio;
#[cfg(target_os = "android")]
mod egl;
#[cfg(target_os = "android")]
mod ffi;
#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
mod input;
#[cfg(target_os = "android")]
mod java;
#[cfg(target_os = "android")]
mod jni;
#[cfg(target_os = "android")]
mod main_loop;

#[cfg(target_os = "android")]
pub use activity::ANativeActivity_onCreate;

/// Routes panic messages to logcat (stderr is not visible on Android).
#[cfg(target_os = "android")]
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            ffi::log(ffi::ANDROID_LOG_ERROR, &format!("panic: {info}"));
        }));
    });
}
