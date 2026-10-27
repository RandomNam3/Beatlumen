//! Runtime loading of system libraries (no link-time dependency on X11,
//! EGL or ALSA, so the binary starts and reports a clear error when a
//! library is missing).

use std::ffi::{c_char, c_int, c_void, CStr, CString};

#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

const RTLD_NOW: c_int = 2;

pub struct Library {
    handle: *mut c_void,
    pub name: String,
}

// The handle is only used for dlsym, which is thread safe.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Opens the first library that can be loaded from `names`.
    pub fn open(names: &[&str]) -> Result<Library, String> {
        let mut errors = Vec::new();
        for name in names {
            let c = CString::new(*name).map_err(|e| e.to_string())?;
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
            if !h.is_null() {
                return Ok(Library { handle: h, name: name.to_string() });
            }
            let err = unsafe { dlerror() };
            if !err.is_null() {
                errors.push(unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned());
            }
        }
        Err(format!("could not load any of {names:?}: {}", errors.join("; ")))
    }

    pub fn sym_ptr(&self, name: &str) -> *mut c_void {
        let c = match CString::new(name) {
            Ok(c) => c,
            Err(_) => return std::ptr::null_mut(),
        };
        unsafe { dlsym(self.handle, c.as_ptr()) }
    }

    /// Looks up a symbol and transmutes it to the function pointer type `T`.
    ///
    /// # Safety
    /// `T` must be a function pointer type matching the symbol's C signature.
    pub unsafe fn sym<T: Copy>(&self, name: &str) -> Result<T, String> {
        let p = self.sym_ptr(name);
        if p.is_null() {
            return Err(format!("{}: missing symbol {name}", self.name));
        }
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        Ok(std::mem::transmute_copy::<*mut c_void, T>(&p))
    }
}
