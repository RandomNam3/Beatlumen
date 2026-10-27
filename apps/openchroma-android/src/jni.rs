//! Minimal JNI access: calling methods of our `MainActivity` from native
//! threads and registering the native callbacks of the Java helper.
//!
//! The function table is indexed directly (indices from `<jni.h>`, which
//! are part of the stable JNI ABI). Strings cross the boundary as UTF-16
//! (`NewString`/`GetStringRegion`) so characters outside the BMP survive,
//! unlike JNI's "modified UTF-8".
//!
//! # Safety
//! A [`JniEnv`] is only valid on the thread it belongs to. All callers
//! pass references (jobject) obtained from the same env or global refs.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void};

pub type jobject = *mut c_void;
pub type jclass = jobject;
pub type jstring = jobject;
pub type jobjectArray = jobject;
pub type jmethodID = *mut c_void;
pub type jint = i32;
pub type jboolean = u8;
pub type RawEnv = *const *const c_void;
pub type RawVm = *const *const c_void;

/// `jvalue` union, used for the `Call*MethodA` variants.
#[repr(C)]
#[derive(Clone, Copy)]
pub union jvalue {
    pub z: jboolean,
    pub i: jint,
    pub j: i64,
    pub l: jobject,
}

/// `JNINativeMethod` for `RegisterNatives`.
#[repr(C)]
pub struct NativeMethod {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub fn_ptr: *mut c_void,
}

// Function table indices (jni.h, JNINativeInterface).
const EXCEPTION_DESCRIBE: usize = 16;
const EXCEPTION_CLEAR: usize = 17;
const DELETE_LOCAL_REF: usize = 23;
const GET_OBJECT_CLASS: usize = 31;
const GET_METHOD_ID: usize = 33;
const CALL_OBJECT_METHOD_A: usize = 36;
const CALL_BOOLEAN_METHOD_A: usize = 39;
const CALL_INT_METHOD_A: usize = 51;
const CALL_VOID_METHOD_A: usize = 63;
const NEW_STRING: usize = 163;
const GET_STRING_LENGTH: usize = 164;
const GET_ARRAY_LENGTH: usize = 171;
const GET_OBJECT_ARRAY_ELEMENT: usize = 173;
const REGISTER_NATIVES: usize = 215;
const GET_STRING_REGION: usize = 220;
const EXCEPTION_CHECK: usize = 228;

// JNIInvokeInterface indices.
const ATTACH_CURRENT_THREAD: usize = 4;
const DETACH_CURRENT_THREAD: usize = 5;

/// A thread's `JNIEnv*`.
#[derive(Clone, Copy)]
pub struct JniEnv(pub *mut RawEnv);

impl JniEnv {
    /// Fetches a function pointer from the table.
    ///
    /// # Safety
    /// `F` must be the exact C signature of entry `index`.
    unsafe fn f<F: Copy>(&self, index: usize) -> F {
        let table: *const *const c_void = *self.0;
        let p = *table.add(index);
        std::mem::transmute_copy::<*const c_void, F>(&p)
    }

    /// Logs and clears a pending Java exception. Returns true if there was
    /// one.
    pub fn check_exception(&self) -> bool {
        // SAFETY: signatures from jni.h.
        unsafe {
            let check: unsafe extern "C" fn(*mut RawEnv) -> jboolean = self.f(EXCEPTION_CHECK);
            if check(self.0) == 0 {
                return false;
            }
            let describe: unsafe extern "C" fn(*mut RawEnv) = self.f(EXCEPTION_DESCRIBE);
            describe(self.0);
            let clear: unsafe extern "C" fn(*mut RawEnv) = self.f(EXCEPTION_CLEAR);
            clear(self.0);
            true
        }
    }

    pub fn delete_local_ref(&self, obj: jobject) {
        if obj.is_null() {
            return;
        }
        // SAFETY: signature from jni.h.
        unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject) = self.f(DELETE_LOCAL_REF);
            f(self.0, obj)
        }
    }

    pub fn get_object_class(&self, obj: jobject) -> jclass {
        // SAFETY: signature from jni.h.
        unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject) -> jclass = self.f(GET_OBJECT_CLASS);
            f(self.0, obj)
        }
    }

    /// Looks up an instance method; null if missing (exception cleared).
    pub fn method_id(&self, class: jclass, name: &std::ffi::CStr, sig: &std::ffi::CStr) -> jmethodID {
        // SAFETY: signature from jni.h; NUL terminated strings.
        let id = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jclass, *const c_char, *const c_char) -> jmethodID = self.f(GET_METHOD_ID);
            f(self.0, class, name.as_ptr(), sig.as_ptr())
        };
        self.check_exception();
        id
    }

    pub fn call_void(&self, obj: jobject, m: jmethodID, args: &[jvalue]) {
        if m.is_null() {
            return;
        }
        // SAFETY: `m` was resolved on obj's class with a matching signature.
        unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject, jmethodID, *const jvalue) = self.f(CALL_VOID_METHOD_A);
            f(self.0, obj, m, args.as_ptr());
        }
        self.check_exception();
    }

    pub fn call_bool(&self, obj: jobject, m: jmethodID, args: &[jvalue]) -> bool {
        if m.is_null() {
            return false;
        }
        // SAFETY: as in call_void.
        let r = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject, jmethodID, *const jvalue) -> jboolean = self.f(CALL_BOOLEAN_METHOD_A);
            f(self.0, obj, m, args.as_ptr())
        };
        !self.check_exception() && r != 0
    }

    pub fn call_int(&self, obj: jobject, m: jmethodID, args: &[jvalue]) -> i32 {
        if m.is_null() {
            return 0;
        }
        // SAFETY: as in call_void.
        let r = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject, jmethodID, *const jvalue) -> jint = self.f(CALL_INT_METHOD_A);
            f(self.0, obj, m, args.as_ptr())
        };
        if self.check_exception() {
            0
        } else {
            r
        }
    }

    /// Calls a method returning `String`; None for null or on exception.
    pub fn call_string(&self, obj: jobject, m: jmethodID, args: &[jvalue]) -> Option<String> {
        if m.is_null() {
            return None;
        }
        // SAFETY: as in call_void.
        let r = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jobject, jmethodID, *const jvalue) -> jobject = self.f(CALL_OBJECT_METHOD_A);
            f(self.0, obj, m, args.as_ptr())
        };
        if self.check_exception() || r.is_null() {
            return None;
        }
        let s = self.get_string(r);
        self.delete_local_ref(r);
        Some(s)
    }

    /// Creates a Java string (local reference) from UTF-8.
    pub fn new_string(&self, s: &str) -> jstring {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: signature from jni.h; buffer valid for len units.
        let r = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, *const u16, jint) -> jstring = self.f(NEW_STRING);
            f(self.0, utf16.as_ptr(), utf16.len() as jint)
        };
        self.check_exception();
        r
    }

    /// Converts a Java string to UTF-8 (lossy for unpaired surrogates).
    pub fn get_string(&self, s: jstring) -> String {
        if s.is_null() {
            return String::new();
        }
        // SAFETY: signatures from jni.h; the buffer holds `len` units.
        unsafe {
            let len_f: unsafe extern "C" fn(*mut RawEnv, jstring) -> jint = self.f(GET_STRING_LENGTH);
            let len = len_f(self.0, s).max(0) as usize;
            let mut buf = vec![0u16; len];
            let region: unsafe extern "C" fn(*mut RawEnv, jstring, jint, jint, *mut u16) = self.f(GET_STRING_REGION);
            region(self.0, s, 0, len as jint, buf.as_mut_ptr());
            if self.check_exception() {
                return String::new();
            }
            String::from_utf16_lossy(&buf)
        }
    }

    /// Converts a `String[]` to a vector.
    pub fn get_string_array(&self, arr: jobjectArray) -> Vec<String> {
        if arr.is_null() {
            return Vec::new();
        }
        // SAFETY: signatures from jni.h; indices are within the length.
        unsafe {
            let len_f: unsafe extern "C" fn(*mut RawEnv, jobject) -> jint = self.f(GET_ARRAY_LENGTH);
            let get: unsafe extern "C" fn(*mut RawEnv, jobjectArray, jint) -> jobject = self.f(GET_OBJECT_ARRAY_ELEMENT);
            let n = len_f(self.0, arr).max(0);
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let e = get(self.0, arr, i);
                if self.check_exception() {
                    break;
                }
                out.push(self.get_string(e));
                self.delete_local_ref(e);
            }
            out
        }
    }

    /// Registers native methods on `class`.
    pub fn register_natives(&self, class: jclass, methods: &[NativeMethod]) -> bool {
        // SAFETY: signature from jni.h; `methods` points to valid entries
        // whose strings are 'static.
        let r = unsafe {
            let f: unsafe extern "C" fn(*mut RawEnv, jclass, *const NativeMethod, jint) -> jint = self.f(REGISTER_NATIVES);
            f(self.0, class, methods.as_ptr(), methods.len() as jint)
        };
        !self.check_exception() && r == 0
    }
}

/// Attaches the current thread to the VM and returns its env.
///
/// # Safety
/// `vm` must be the process' valid `JavaVM*`.
pub unsafe fn attach_current_thread(vm: *mut RawVm) -> Option<JniEnv> {
    let table: *const *const c_void = *vm;
    let attach: unsafe extern "C" fn(*mut RawVm, *mut *mut RawEnv, *mut c_void) -> jint =
        std::mem::transmute_copy::<*const c_void, _>(&*table.add(ATTACH_CURRENT_THREAD));
    let mut env: *mut RawEnv = std::ptr::null_mut();
    if attach(vm, &mut env, std::ptr::null_mut()) != 0 || env.is_null() {
        return None;
    }
    Some(JniEnv(env))
}

/// Detaches the current thread from the VM.
///
/// # Safety
/// The thread must have been attached with [`attach_current_thread`] and
/// must not use its env afterwards.
pub unsafe fn detach_current_thread(vm: *mut RawVm) {
    let table: *const *const c_void = *vm;
    let detach: unsafe extern "C" fn(*mut RawVm) -> jint = std::mem::transmute_copy::<*const c_void, _>(&*table.add(DETACH_CURRENT_THREAD));
    detach(vm);
}
