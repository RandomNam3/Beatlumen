//! Calls into the Java helper `io.github.randomnam3.beatlumen.MainActivity`
//! (see `java/.../MainActivity.java`) from the app thread, and the native
//! methods the helper calls back.

use crate::activity;
use crate::jni::{jboolean, jint, jmethodID, jobject, jobjectArray, jstring, jvalue, JniEnv, NativeMethod, RawEnv};
use std::ffi::c_void;

/// Method ids of `MainActivity`, resolved once on the app thread.
pub struct Java {
    pub env: JniEnv,
    /// Global reference owned by NativeActivity (`ANativeActivity::clazz`).
    activity: jobject,
    set_text_input: jmethodID,
    set_keep_screen_on: jmethodID,
    set_immersive: jmethodID,
    has_media_permission: jmethodID,
    request_media_permission: jmethodID,
    pick_files: jmethodID,
    get_clipboard: jmethodID,
    set_clipboard: jmethodID,
    unicode_char: jmethodID,
    external_storage: jmethodID,
}

impl Java {
    /// # Safety
    /// `env` must belong to the current thread and `activity` must be a
    /// global reference to the `MainActivity` instance.
    pub unsafe fn new(env: JniEnv, activity: jobject) -> Java {
        let class = env.get_object_class(activity);
        let m = |name: &std::ffi::CStr, sig: &std::ffi::CStr| {
            let id = env.method_id(class, name, sig);
            if id.is_null() {
                crate::ffi::log(crate::ffi::ANDROID_LOG_ERROR, &format!("MainActivity method missing: {name:?} {sig:?}"));
            }
            id
        };
        let java = Java {
            env,
            activity,
            set_text_input: m(c"setTextInput", c"(Z)V"),
            set_keep_screen_on: m(c"setKeepScreenOn", c"(Z)V"),
            set_immersive: m(c"setFullscreenMode", c"(Z)V"),
            has_media_permission: m(c"hasMediaPermission", c"()Z"),
            request_media_permission: m(c"requestMediaPermission", c"()V"),
            pick_files: m(c"pickFiles", c"(Ljava/lang/String;Z)V"),
            get_clipboard: m(c"getClipboardText", c"()Ljava/lang/String;"),
            set_clipboard: m(c"setClipboardText", c"(Ljava/lang/String;)V"),
            unicode_char: m(c"getUnicodeChar", c"(III)I"),
            external_storage: m(c"getExternalStoragePath", c"()Ljava/lang/String;"),
        };
        env.delete_local_ref(class);
        java
    }

    fn bool_arg(b: bool) -> jvalue {
        jvalue { z: b as jboolean }
    }

    pub fn set_text_input(&self, on: bool) {
        self.env.call_void(self.activity, self.set_text_input, &[Self::bool_arg(on)]);
    }

    pub fn set_keep_screen_on(&self, on: bool) {
        self.env.call_void(self.activity, self.set_keep_screen_on, &[Self::bool_arg(on)]);
    }

    pub fn set_immersive(&self, on: bool) {
        self.env.call_void(self.activity, self.set_immersive, &[Self::bool_arg(on)]);
    }

    pub fn has_media_permission(&self) -> bool {
        self.env.call_bool(self.activity, self.has_media_permission, &[])
    }

    pub fn request_media_permission(&self) {
        self.env.call_void(self.activity, self.request_media_permission, &[]);
    }

    pub fn pick_files(&self, accept: &str, multiple: bool) {
        let s = self.env.new_string(accept);
        self.env.call_void(self.activity, self.pick_files, &[jvalue { l: s }, Self::bool_arg(multiple)]);
        self.env.delete_local_ref(s);
    }

    pub fn clipboard(&self) -> Option<String> {
        self.env.call_string(self.activity, self.get_clipboard, &[])
    }

    pub fn set_clipboard(&self, text: &str) {
        let s = self.env.new_string(text);
        self.env.call_void(self.activity, self.set_clipboard, &[jvalue { l: s }]);
        self.env.delete_local_ref(s);
    }

    /// The character a key produces with the device's key map (0 if none).
    pub fn unicode_char(&self, device_id: i32, key_code: i32, meta_state: i32) -> i32 {
        self.env.call_int(self.activity, self.unicode_char, &[jvalue { i: device_id }, jvalue { i: key_code }, jvalue { i: meta_state }])
    }

    pub fn external_storage(&self) -> Option<String> {
        self.env.call_string(self.activity, self.external_storage, &[]).filter(|s| !s.is_empty())
    }
}

// ---- native methods called by MainActivity ------------------------------------------
//
// These run on the Java UI thread (or the helper's file copy thread) and
// only post commands to the app thread.

unsafe extern "C" fn native_on_text(env: *mut RawEnv, _this: jobject, text: jstring) {
    let s = JniEnv(env).get_string(text);
    if !s.is_empty() {
        activity::post_from_java(activity::Cmd::Text(s));
    }
}

unsafe extern "C" fn native_on_key(_env: *mut RawEnv, _this: jobject, key_code: jint) {
    activity::post_from_java(activity::Cmd::ImeKey(key_code));
}

unsafe extern "C" fn native_on_permission_result(_env: *mut RawEnv, _this: jobject, granted: jboolean) {
    activity::post_from_java(activity::Cmd::Permission(granted != 0));
}

unsafe extern "C" fn native_on_files_picked(env: *mut RawEnv, _this: jobject, paths: jobjectArray) {
    let v = JniEnv(env).get_string_array(paths);
    if !v.is_empty() {
        activity::post_from_java(activity::Cmd::Files(v));
    }
}

/// Registers the native methods on the activity's class.
///
/// # Safety
/// Must be called on a thread owning `env` with a valid activity ref.
pub unsafe fn register_natives(env: JniEnv, activity: jobject) -> bool {
    let methods = [
        NativeMethod { name: c"nativeOnText".as_ptr(), signature: c"(Ljava/lang/String;)V".as_ptr(), fn_ptr: native_on_text as *mut c_void },
        NativeMethod { name: c"nativeOnKey".as_ptr(), signature: c"(I)V".as_ptr(), fn_ptr: native_on_key as *mut c_void },
        NativeMethod {
            name: c"nativeOnPermissionResult".as_ptr(),
            signature: c"(Z)V".as_ptr(),
            fn_ptr: native_on_permission_result as *mut c_void,
        },
        NativeMethod {
            name: c"nativeOnFilesPicked".as_ptr(),
            signature: c"([Ljava/lang/String;)V".as_ptr(),
            fn_ptr: native_on_files_picked as *mut c_void,
        },
    ];
    let class = env.get_object_class(activity);
    let ok = env.register_natives(class, &methods);
    env.delete_local_ref(class);
    ok
}
