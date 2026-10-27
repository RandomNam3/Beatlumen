//! EGL display/context/surface management for the native window.
//!
//! The context is kept across window destruction (backgrounding, rotation
//! with a new surface) so GL resources survive. Only when EGL reports a
//! lost context (`EGL_CONTEXT_LOST`) is the context recreated, in which
//! case the caller must call `App::gl_lost` and `App::gl_init` again.

#![allow(non_snake_case, dead_code)]

use crate::ffi::{self, ANativeWindow};
use std::ffi::{c_char, c_void};

type EGLDisplay = *mut c_void;
type EGLConfig = *mut c_void;
type EGLContext = *mut c_void;
type EGLSurface = *mut c_void;
type EGLint = i32;
type EGLBoolean = u32;

const EGL_NONE: EGLint = 0x3038;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_GREEN_SIZE: EGLint = 0x3023;
const EGL_BLUE_SIZE: EGLint = 0x3022;
const EGL_ALPHA_SIZE: EGLint = 0x3021;
const EGL_DEPTH_SIZE: EGLint = 0x3025;
const EGL_STENCIL_SIZE: EGLint = 0x3026;
const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_WINDOW_BIT: EGLint = 0x0004;
const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_HEIGHT: EGLint = 0x3056;
const EGL_CONTEXT_LOST: EGLint = 0x300E;
const EGL_BAD_CONTEXT: EGLint = 0x3006;

#[link(name = "EGL")]
extern "C" {
    fn eglGetDisplay(native: *mut c_void) -> EGLDisplay;
    fn eglInitialize(dpy: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean;
    fn eglTerminate(dpy: EGLDisplay) -> EGLBoolean;
    fn eglChooseConfig(dpy: EGLDisplay, attribs: *const EGLint, configs: *mut EGLConfig, size: EGLint, n: *mut EGLint) -> EGLBoolean;
    fn eglGetConfigAttrib(dpy: EGLDisplay, config: EGLConfig, attr: EGLint, value: *mut EGLint) -> EGLBoolean;
    fn eglCreateWindowSurface(dpy: EGLDisplay, config: EGLConfig, win: *mut ANativeWindow, attribs: *const EGLint) -> EGLSurface;
    fn eglDestroySurface(dpy: EGLDisplay, surface: EGLSurface) -> EGLBoolean;
    fn eglCreateContext(dpy: EGLDisplay, config: EGLConfig, share: EGLContext, attribs: *const EGLint) -> EGLContext;
    fn eglDestroyContext(dpy: EGLDisplay, ctx: EGLContext) -> EGLBoolean;
    fn eglMakeCurrent(dpy: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext) -> EGLBoolean;
    fn eglSwapBuffers(dpy: EGLDisplay, surface: EGLSurface) -> EGLBoolean;
    fn eglSwapInterval(dpy: EGLDisplay, interval: EGLint) -> EGLBoolean;
    fn eglQuerySurface(dpy: EGLDisplay, surface: EGLSurface, attr: EGLint, value: *mut EGLint) -> EGLBoolean;
    fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    fn eglGetError() -> EGLint;
}

/// Result of [`Egl::attach`] / [`Egl::swap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The context was lost and has been recreated: GL objects are gone.
    ContextRecreated,
}

pub struct Egl {
    display: EGLDisplay,
    config: EGLConfig,
    context: EGLContext,
    surface: EGLSurface,
    /// `libGLESv3.so` handle for core GL entry points.
    gles: *mut c_void,
}

impl Egl {
    /// Initializes the display and creates a GLES 3 context (no surface).
    pub fn new() -> Result<Egl, String> {
        // SAFETY: plain EGL calls with valid out pointers; the default
        // display is used.
        unsafe {
            let display = eglGetDisplay(std::ptr::null_mut());
            if display.is_null() {
                return Err("eglGetDisplay failed".into());
            }
            let (mut major, mut minor) = (0, 0);
            if eglInitialize(display, &mut major, &mut minor) == 0 {
                return Err(format!("eglInitialize failed (0x{:x})", eglGetError()));
            }
            let attribs = [
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_ALPHA_SIZE,
                0,
                EGL_DEPTH_SIZE,
                0,
                EGL_STENCIL_SIZE,
                0,
                EGL_SURFACE_TYPE,
                EGL_WINDOW_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES3_BIT,
                EGL_NONE,
            ];
            // Pick the first config that is exactly RGB888 (some drivers
            // sort RGBA8888/RGB565 first).
            let mut configs = [std::ptr::null_mut::<c_void>(); 64];
            let mut n = 0;
            if eglChooseConfig(display, attribs.as_ptr(), configs.as_mut_ptr(), configs.len() as EGLint, &mut n) == 0 || n <= 0 {
                eglTerminate(display);
                return Err("no suitable EGL config (OpenGL ES 3 required)".into());
            }
            let candidates = &configs[..n as usize];
            let attr = |c: EGLConfig, a: EGLint| {
                let mut v = 0;
                eglGetConfigAttrib(display, c, a, &mut v);
                v
            };
            let config = candidates
                .iter()
                .copied()
                .find(|&c| attr(c, EGL_RED_SIZE) == 8 && attr(c, EGL_GREEN_SIZE) == 8 && attr(c, EGL_BLUE_SIZE) == 8)
                .unwrap_or(candidates[0]);
            let gles = ffi::dlopen(c"libGLESv3.so".as_ptr(), ffi::RTLD_NOW);
            let mut egl = Egl { display, config, context: std::ptr::null_mut(), surface: std::ptr::null_mut(), gles };
            egl.create_context()?;
            Ok(egl)
        }
    }

    fn create_context(&mut self) -> Result<(), String> {
        let ctx_attribs = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
        // SAFETY: display/config are valid for the lifetime of `self`.
        let context = unsafe { eglCreateContext(self.display, self.config, std::ptr::null_mut(), ctx_attribs.as_ptr()) };
        if context.is_null() {
            return Err(format!("eglCreateContext failed (0x{:x})", unsafe { eglGetError() }));
        }
        self.context = context;
        Ok(())
    }

    /// Resolves a GL ES entry point (libGLESv3 exports, then
    /// `eglGetProcAddress` for extensions).
    pub fn get_proc(&self, name: &str) -> *const c_void {
        let Ok(c) = std::ffi::CString::new(name) else { return std::ptr::null() };
        // SAFETY: valid NUL terminated symbol name; `gles` may be null in
        // which case only eglGetProcAddress is used.
        unsafe {
            if !self.gles.is_null() {
                let p = ffi::dlsym(self.gles, c.as_ptr());
                if !p.is_null() {
                    return p;
                }
            }
            eglGetProcAddress(c.as_ptr()) as *const c_void
        }
    }

    /// Loads the GL function table (the context must be current).
    pub fn load_gl(&self) -> Result<oc_gl::Gl, String> {
        // SAFETY: get_proc returns entry points of the current GLES 3
        // implementation, matching oc-gl's declarations.
        unsafe { oc_gl::Gl::load(oc_gl::Flavor::Gles3, &mut |name| self.get_proc(name)) }
    }

    /// Creates a window surface for `window` and makes the context
    /// current. Recreates the context if it was lost.
    ///
    /// # Safety
    /// `window` must be a valid native window that stays alive until
    /// [`Egl::detach`] is called.
    pub unsafe fn attach(&mut self, window: *mut ANativeWindow) -> Result<Status, String> {
        self.detach();
        let mut format = 0;
        eglGetConfigAttrib(self.display, self.config, EGL_NATIVE_VISUAL_ID, &mut format);
        ffi::ANativeWindow_setBuffersGeometry(window, 0, 0, format);
        let surface = eglCreateWindowSurface(self.display, self.config, window, std::ptr::null());
        if surface.is_null() {
            return Err(format!("eglCreateWindowSurface failed (0x{:x})", eglGetError()));
        }
        self.surface = surface;
        let mut status = Status::Ok;
        if eglMakeCurrent(self.display, surface, surface, self.context) == 0 {
            let err = eglGetError();
            if err != EGL_CONTEXT_LOST && err != EGL_BAD_CONTEXT {
                return Err(format!("eglMakeCurrent failed (0x{err:x})"));
            }
            eglDestroyContext(self.display, self.context);
            self.create_context()?;
            if eglMakeCurrent(self.display, surface, surface, self.context) == 0 {
                return Err(format!("eglMakeCurrent failed after context recreation (0x{:x})", eglGetError()));
            }
            status = Status::ContextRecreated;
        }
        eglSwapInterval(self.display, 1);
        Ok(status)
    }

    /// Releases the window surface (keeps the context).
    pub fn detach(&mut self) {
        if self.surface.is_null() {
            return;
        }
        // SAFETY: surface belongs to this display.
        unsafe {
            eglMakeCurrent(self.display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            eglDestroySurface(self.display, self.surface);
        }
        self.surface = std::ptr::null_mut();
    }

    pub fn has_surface(&self) -> bool {
        !self.surface.is_null()
    }

    /// Current surface size in pixels.
    pub fn surface_size(&self) -> (i32, i32) {
        let (mut w, mut h) = (0, 0);
        if !self.surface.is_null() {
            // SAFETY: valid surface, valid out pointers.
            unsafe {
                eglQuerySurface(self.display, self.surface, EGL_WIDTH, &mut w);
                eglQuerySurface(self.display, self.surface, EGL_HEIGHT, &mut h);
            }
        }
        (w, h)
    }

    /// Presents the frame. Returns `Err(true)` when the context was lost
    /// (the caller must re-attach and reinitialize GL resources).
    pub fn swap(&self) -> Result<(), bool> {
        // SAFETY: valid display/surface.
        unsafe {
            if eglSwapBuffers(self.display, self.surface) != 0 {
                return Ok(());
            }
            let err = eglGetError();
            Err(err == EGL_CONTEXT_LOST)
        }
    }

    /// Destroys the (lost) context and creates a new one; must be
    /// followed by [`Egl::attach`].
    pub fn reset_context(&mut self) -> Result<(), String> {
        self.detach();
        // SAFETY: the context belongs to this display.
        unsafe { eglDestroyContext(self.display, self.context) };
        self.create_context()
    }
}

impl Drop for Egl {
    fn drop(&mut self) {
        self.detach();
        // SAFETY: tear down in reverse order of creation.
        unsafe {
            eglDestroyContext(self.display, self.context);
            eglTerminate(self.display);
        }
    }
}
