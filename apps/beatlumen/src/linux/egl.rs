//! EGL bindings (OpenGL ES 3 contexts on X11 windows).

#![allow(non_snake_case, dead_code)]

use super::dl::Library;
use std::ffi::{c_char, c_void};

pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLint = i32;
pub type EGLBoolean = u32;
pub type EGLenum = u32;

pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;
pub const EGL_DEPTH_SIZE: EGLint = 0x3025;
pub const EGL_STENCIL_SIZE: EGLint = 0x3026;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_WINDOW_BIT: EGLint = 0x0004;
pub const EGL_PBUFFER_BIT: EGLint = 0x0001;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
pub const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
pub const EGL_WIDTH: EGLint = 0x3057;
pub const EGL_HEIGHT: EGLint = 0x3056;
pub const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;

pub struct Egl {
    pub lib: Library,
    pub gles: Option<Library>,
    pub eglGetDisplay: unsafe extern "C" fn(*mut c_void) -> EGLDisplay,
    pub eglInitialize: unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean,
    pub eglTerminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
    pub eglBindAPI: unsafe extern "C" fn(EGLenum) -> EGLBoolean,
    pub eglChooseConfig: unsafe extern "C" fn(EGLDisplay, *const EGLint, *mut EGLConfig, EGLint, *mut EGLint) -> EGLBoolean,
    pub eglGetConfigAttrib: unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLint, *mut EGLint) -> EGLBoolean,
    pub eglCreateWindowSurface: unsafe extern "C" fn(EGLDisplay, EGLConfig, usize, *const EGLint) -> EGLSurface,
    pub eglCreatePbufferSurface: unsafe extern "C" fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLSurface,
    pub eglDestroySurface: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
    pub eglCreateContext: unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext,
    pub eglDestroyContext: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
    pub eglMakeCurrent: unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean,
    pub eglSwapBuffers: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
    pub eglSwapInterval: unsafe extern "C" fn(EGLDisplay, EGLint) -> EGLBoolean,
    pub eglGetProcAddress: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    pub eglGetError: unsafe extern "C" fn() -> EGLint,
    pub eglQuerySurface: unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLint, *mut EGLint) -> EGLBoolean,
}

impl Egl {
    pub fn load() -> Result<Egl, String> {
        let lib = Library::open(&["libEGL.so.1", "libEGL.so"])?;
        let gles = Library::open(&["libGLESv2.so.2", "libGLESv2.so"]).ok();
        unsafe {
            Ok(Egl {
                eglGetDisplay: lib.sym("eglGetDisplay")?,
                eglInitialize: lib.sym("eglInitialize")?,
                eglTerminate: lib.sym("eglTerminate")?,
                eglBindAPI: lib.sym("eglBindAPI")?,
                eglChooseConfig: lib.sym("eglChooseConfig")?,
                eglGetConfigAttrib: lib.sym("eglGetConfigAttrib")?,
                eglCreateWindowSurface: lib.sym("eglCreateWindowSurface")?,
                eglCreatePbufferSurface: lib.sym("eglCreatePbufferSurface")?,
                eglDestroySurface: lib.sym("eglDestroySurface")?,
                eglCreateContext: lib.sym("eglCreateContext")?,
                eglDestroyContext: lib.sym("eglDestroyContext")?,
                eglMakeCurrent: lib.sym("eglMakeCurrent")?,
                eglSwapBuffers: lib.sym("eglSwapBuffers")?,
                eglSwapInterval: lib.sym("eglSwapInterval")?,
                eglGetProcAddress: lib.sym("eglGetProcAddress")?,
                eglGetError: lib.sym("eglGetError")?,
                eglQuerySurface: lib.sym("eglQuerySurface")?,
                lib,
                gles,
            })
        }
    }

    /// Resolves a GL ES function: libGLESv2 exports first, then
    /// eglGetProcAddress.
    pub fn get_proc(&self, name: &str) -> *const c_void {
        if let Some(g) = &self.gles {
            let p = g.sym_ptr(name);
            if !p.is_null() {
                return p;
            }
        }
        let Ok(c) = std::ffi::CString::new(name) else { return std::ptr::null() };
        unsafe { (self.eglGetProcAddress)(c.as_ptr()) as *const c_void }
    }
}

/// An initialised EGL display + context + surface.
pub struct EglContext {
    pub egl: Egl,
    pub display: EGLDisplay,
    pub config: EGLConfig,
    pub context: EGLContext,
    pub surface: EGLSurface,
}

impl EglContext {
    /// Creates a GLES 3 context. `native_display` is the X11 Display (or
    /// null for the default display); `window` the native window id, or 0
    /// for an offscreen pbuffer of `pbuffer_size`.
    pub fn new(native_display: *mut c_void, window: usize, pbuffer_size: (i32, i32)) -> Result<EglContext, String> {
        let egl = Egl::load()?;
        unsafe {
            let display = (egl.eglGetDisplay)(native_display);
            if display.is_null() {
                return Err("eglGetDisplay failed".into());
            }
            let (mut major, mut minor) = (0, 0);
            if (egl.eglInitialize)(display, &mut major, &mut minor) == 0 {
                return Err(format!("eglInitialize failed (0x{:x})", (egl.eglGetError)()));
            }
            (egl.eglBindAPI)(EGL_OPENGL_ES_API);
            let surface_type = if window != 0 { EGL_WINDOW_BIT } else { EGL_PBUFFER_BIT };
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
                EGL_SURFACE_TYPE,
                surface_type,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES3_BIT,
                EGL_NONE,
            ];
            let mut config: EGLConfig = std::ptr::null_mut();
            let mut n = 0;
            if (egl.eglChooseConfig)(display, attribs.as_ptr(), &mut config, 1, &mut n) == 0 || n == 0 {
                return Err("no suitable EGL config (OpenGL ES 3 required)".into());
            }
            let surface = if window != 0 {
                (egl.eglCreateWindowSurface)(display, config, window, std::ptr::null())
            } else {
                let pb = [EGL_WIDTH, pbuffer_size.0, EGL_HEIGHT, pbuffer_size.1, EGL_NONE];
                (egl.eglCreatePbufferSurface)(display, config, pb.as_ptr())
            };
            if surface.is_null() {
                return Err(format!("eglCreate*Surface failed (0x{:x})", (egl.eglGetError)()));
            }
            let ctx_attribs = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
            let context = (egl.eglCreateContext)(display, config, std::ptr::null_mut(), ctx_attribs.as_ptr());
            if context.is_null() {
                return Err(format!("eglCreateContext failed (0x{:x})", (egl.eglGetError)()));
            }
            if (egl.eglMakeCurrent)(display, surface, surface, context) == 0 {
                return Err(format!("eglMakeCurrent failed (0x{:x})", (egl.eglGetError)()));
            }
            Ok(EglContext { egl, display, config, context, surface })
        }
    }

    pub fn native_visual(&self) -> i32 {
        let mut v = 0;
        unsafe { (self.egl.eglGetConfigAttrib)(self.display, self.config, EGL_NATIVE_VISUAL_ID, &mut v) };
        v
    }

    pub fn swap(&self) -> bool {
        unsafe { (self.egl.eglSwapBuffers)(self.display, self.surface) != 0 }
    }

    pub fn set_vsync(&self, on: bool) {
        unsafe { (self.egl.eglSwapInterval)(self.display, on as EGLint) };
    }

    pub fn load_gl(&self) -> Result<bl_gl::Gl, String> {
        unsafe { bl_gl::Gl::load(bl_gl::Flavor::Gles3, &mut |name| self.egl.get_proc(name)) }
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        unsafe {
            (self.egl.eglMakeCurrent)(self.display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            (self.egl.eglDestroyContext)(self.display, self.context);
            (self.egl.eglDestroySurface)(self.display, self.surface);
            (self.egl.eglTerminate)(self.display);
        }
    }
}
