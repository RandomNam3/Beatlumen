//! Finds the system's Vulkan loader at run time (no link-time
//! dependency): `libvulkan.so.1` / `libvulkan.so` on Linux and Android,
//! `vulkan-1.dll` from the Windows system folder. The library stays
//! loaded for the rest of the process (drivers do not all cope with
//! being unloaded), so the function pointers from it stay valid.

use crate::vk;
use std::sync::OnceLock;

/// `vkGetInstanceProcAddr` of the system's loader, or `None` without
/// Vulkan. Looked up once.
pub fn get_instance_proc_addr() -> Option<vk::GetInstanceProcAddr> {
    static ENTRY: OnceLock<Option<vk::GetInstanceProcAddr>> = OnceLock::new();
    *ENTRY.get_or_init(sys::load)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use crate::vk;
    use std::ffi::{c_char, c_int, c_void, CStr};

    // Lazy binding, symbols kept local: the same values in glibc, musl and
    // bionic.
    const RTLD_LAZY: c_int = 1;
    const RTLD_LOCAL: c_int = 0;

    #[cfg_attr(
        any(target_os = "android", all(target_os = "linux", target_env = "gnu")),
        link(name = "dl")
    )]
    extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[cfg(target_os = "android")]
    const NAMES: &[&CStr] = &[c"libvulkan.so"];
    #[cfg(not(target_os = "android"))]
    const NAMES: &[&CStr] = &[c"libvulkan.so.1", c"libvulkan.so"];

    pub fn load() -> Option<vk::GetInstanceProcAddr> {
        for name in NAMES {
            // SAFETY: `name` is a NUL-terminated string; dlopen has no
            // other preconditions. The handle is never closed.
            let lib = unsafe { dlopen(name.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
            if lib.is_null() {
                continue;
            }
            // SAFETY: `lib` is a live handle from dlopen and the symbol
            // name is NUL-terminated.
            let f = unsafe { dlsym(lib, c"vkGetInstanceProcAddr".as_ptr()) };
            if f.is_null() {
                continue;
            }
            // SAFETY: the Vulkan loader exports vkGetInstanceProcAddr with
            // this signature, and the library stays loaded.
            return Some(unsafe { std::mem::transmute::<*mut c_void, vk::GetInstanceProcAddr>(f) });
        }
        None
    }
}

#[cfg(windows)]
mod sys {
    use crate::vk;
    use std::ffi::{c_char, c_void};

    /// Searches only the system folder (no DLL planted next to the
    /// program or in the working folder is picked up).
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    pub fn load() -> Option<vk::GetInstanceProcAddr> {
        let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain([0]).collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string; the file
        // handle must be null. The module is never freed.
        let lib = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if lib.is_null() {
            return None;
        }
        // SAFETY: `lib` is a loaded module and the name NUL-terminated.
        let f = unsafe { GetProcAddress(lib, c"vkGetInstanceProcAddr".as_ptr()) };
        if f.is_null() {
            return None;
        }
        // SAFETY: the Vulkan loader exports vkGetInstanceProcAddr with
        // this signature, and the module stays loaded.
        Some(unsafe { std::mem::transmute::<*mut c_void, vk::GetInstanceProcAddr>(f) })
    }
}

/// No Vulkan on other systems (macOS needs MoltenVK, the browser has
/// none).
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
mod sys {
    use crate::vk;

    pub fn load() -> Option<vk::GetInstanceProcAddr> {
        None
    }
}
