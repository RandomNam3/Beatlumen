//! `bl-vault` for the browser sign-in page (`apps/beatlumen/web/auth`):
//! the same key derivation and wrapping as the apps, compiled to
//! WebAssembly with a plain C ABI (no generated JavaScript bindings).
//!
//! Calls take (pointer, length) pairs in the module's memory (allocated
//! with `bl_alloc`) and return 0 on success, with the result readable at
//! `bl_out_ptr`/`bl_out_len`, or a negative number with an error message
//! there instead. The page provides `env.host_random(ptr, len)`
//! (`crypto.getRandomValues`).

use bl_vault::keys::{self, KdfParams, PasswordKeys};
use bl_vault::Secret;
use std::sync::Mutex;

static OUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Password keys between deriving them and unwrapping the master key.
static PASSWORD: Mutex<Option<PasswordKeys>> = Mutex::new(None);

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod rng {
    #[link(wasm_import_module = "env")]
    extern "C" {
        fn host_random(ptr: *mut u8, len: usize);
    }

    fn fill(buf: &mut [u8]) -> Result<(), getrandom::Error> {
        // SAFETY: the page writes exactly `len` bytes at `ptr`.
        unsafe { host_random(buf.as_mut_ptr(), buf.len()) };
        Ok(())
    }

    getrandom::register_custom_getrandom!(fill);
}

fn set_out(data: &[u8]) {
    let mut o = OUT.lock().unwrap_or_else(|e| e.into_inner());
    o.clear();
    o.extend_from_slice(data);
}

fn finish(r: Result<Vec<u8>, String>) -> i32 {
    match r {
        Ok(v) => {
            set_out(&v);
            0
        }
        Err(e) => {
            set_out(e.as_bytes());
            -1
        }
    }
}

/// # Safety
/// `ptr`/`len` must describe memory obtained from `bl_alloc`.
unsafe fn input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

unsafe fn text<'a>(ptr: *const u8, len: usize) -> Result<&'a str, String> {
    std::str::from_utf8(input(ptr, len)).map_err(|_| "not UTF-8".to_string())
}

fn secret(b: &[u8]) -> Result<Secret, String> {
    b.try_into()
        .map(Secret::from_bytes)
        .map_err(|_| "a key must be 32 bytes".to_string())
}

#[no_mangle]
pub extern "C" fn bl_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr`/`len` must come from `bl_alloc(len)`.
#[no_mangle]
pub unsafe extern "C" fn bl_free(ptr: *mut u8, len: usize) {
    // Wipe what may have held a password or key.
    std::ptr::write_bytes(ptr, 0, len);
    drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
}

#[no_mangle]
pub extern "C" fn bl_out_ptr() -> *const u8 {
    OUT.lock().unwrap_or_else(|e| e.into_inner()).as_ptr()
}

#[no_mangle]
pub extern "C" fn bl_out_len() -> usize {
    OUT.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Clears the result and the stored password keys.
#[no_mangle]
pub extern "C" fn bl_clear() {
    let mut o = OUT.lock().unwrap_or_else(|e| e.into_inner());
    o.iter_mut().for_each(|b| *b = 0);
    o.clear();
    *PASSWORD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Argon2id of the password; result: the auth key (the keys stay inside
/// for `bl_pw_unwrap`).
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_pw_derive(
    pw: *const u8,
    pw_len: usize,
    salt: *const u8,
    m: u32,
    t: u32,
    p: u32,
) -> i32 {
    finish((|| {
        let pw = text(pw, pw_len)?;
        let salt: [u8; 16] = input(salt, 16).try_into().map_err(|_| "salt")?;
        let k = PasswordKeys::derive(pw, &salt, KdfParams { m_kib: m, t, p })
            .map_err(|e| e.to_string())?;
        let auth = k.auth.as_bytes().to_vec();
        *PASSWORD.lock().unwrap_or_else(|e| e.into_inner()) = Some(k);
        Ok(auth)
    })())
}

/// The master key from its password wrapping; result: 32 bytes.
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_pw_unwrap(
    user: *const u8,
    user_len: usize,
    wrapped: *const u8,
    wrapped_len: usize,
) -> i32 {
    finish((|| {
        let g = PASSWORD.lock().unwrap_or_else(|e| e.into_inner());
        let k = g.as_ref().ok_or("derive the password keys first")?;
        let m = keys::unwrap_master_with_password(
            text(user, user_len)?,
            k,
            input(wrapped, wrapped_len),
        )
        .map_err(|e| e.to_string())?;
        Ok(m.as_bytes().to_vec())
    })())
}

/// The master key from a passkey's wrapping and PRF output.
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_passkey_unwrap(
    user: *const u8,
    user_len: usize,
    cred: *const u8,
    cred_len: usize,
    prf: *const u8,
    prf_len: usize,
    wrapped: *const u8,
    wrapped_len: usize,
) -> i32 {
    finish((|| {
        let m = keys::unwrap_master_with_passkey(
            text(user, user_len)?,
            input(cred, cred_len),
            input(prf, prf_len),
            input(wrapped, wrapped_len),
        )
        .map_err(|e| e.to_string())?;
        Ok(m.as_bytes().to_vec())
    })())
}

/// Wraps the master key for a new passkey.
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_passkey_wrap(
    user: *const u8,
    user_len: usize,
    master: *const u8,
    cred: *const u8,
    cred_len: usize,
    prf: *const u8,
    prf_len: usize,
) -> i32 {
    finish((|| {
        keys::wrap_master_for_passkey(
            text(user, user_len)?,
            &secret(input(master, 32))?,
            input(cred, cred_len),
            input(prf, prf_len),
        )
        .map_err(|e| e.to_string())
    })())
}

/// A new password for the master key; result: JSON for
/// `PUT /api/vault/account/keys` ("password" object).
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_password_set(
    user: *const u8,
    user_len: usize,
    master: *const u8,
    pw: *const u8,
    pw_len: usize,
) -> i32 {
    finish((|| {
        let c = keys::password_change(
            text(user, user_len)?,
            &secret(input(master, 32))?,
            text(pw, pw_len)?,
        )
        .map_err(|e| e.to_string())?;
        let b64 = |b: &[u8]| bl_vault::manifest::base64_encode(b);
        let v = bl_json::Value::object()
            .with("salt", b64(&c.salt))
            .with(
                "params",
                bl_json::Value::object()
                    .with("m", c.params.m_kib as u64)
                    .with("t", c.params.t as u64)
                    .with("p", c.params.p as u64),
            )
            .with("auth", b64(c.auth.as_bytes()))
            .with("masterByPassword", b64(&c.master_by_password));
        Ok(v.to_json().into_bytes())
    })())
}

/// Seals the sign-in result for an app's one-time key.
///
/// # Safety
/// Pointers from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_handoff_seal(
    app_key: *const u8,
    state: *const u8,
    state_len: usize,
    payload: *const u8,
    payload_len: usize,
) -> i32 {
    finish((|| {
        let k: [u8; 32] = input(app_key, 32).try_into().map_err(|_| "key")?;
        bl_vault::handoff::seal(&k, text(state, state_len)?, input(payload, payload_len))
            .map_err(|e| e.to_string())
    })())
}

/// The pairing code of an app's one-time key ("123 456").
///
/// # Safety
/// Pointer from `bl_alloc`.
#[no_mangle]
pub unsafe extern "C" fn bl_pairing_code(app_key: *const u8) -> i32 {
    finish((|| {
        let k: [u8; 32] = input(app_key, 32).try_into().map_err(|_| "key")?;
        Ok(bl_vault::handoff::pairing_code(&k).into_bytes())
    })())
}

/// The PRF input of passkeys (32 bytes).
#[no_mangle]
pub extern "C" fn bl_prf_input() -> i32 {
    finish(Ok(keys::passkey_prf_input().to_vec()))
}
