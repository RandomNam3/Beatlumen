//! No panic leaves through the C interface: a panic in a command, in a tick
//! or while rendering audio is caught, the call answers with an error or
//! silence, and the core keeps working.

use beatlumen_core::*;
use std::ffi::{CStr, CString};

fn call(core: *mut BlCore, json: &str) -> String {
    let c = CString::new(json).unwrap();
    // SAFETY: valid core and NUL terminated JSON; the result is freed here.
    unsafe {
        let r = bl_core_command(core, c.as_ptr());
        assert!(!r.is_null(), "{json}: no answer");
        let s = CStr::from_ptr(r).to_string_lossy().into_owned();
        bl_string_free(r);
        s
    }
}

fn tick(core: *mut BlCore) -> String {
    // SAFETY: valid core; the result is freed here.
    unsafe {
        let r = bl_core_tick(core);
        assert!(!r.is_null(), "tick: no answer");
        let s = CStr::from_ptr(r).to_string_lossy().into_owned();
        bl_string_free(r);
        s
    }
}

#[test]
fn panics_do_not_cross_the_c_interface() {
    // The test hook also answers in release builds in the app's test mode.
    std::env::set_var("BEATLUMEN_TEST_DUMP", "test");
    let dir = std::env::temp_dir().join(format!("bl-ffi-panics-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = CString::new(dir.to_string_lossy().as_bytes()).unwrap();
    let args = CString::new("[]").unwrap();
    let lang = CString::new("de").unwrap();
    let platform = CString::new("linux").unwrap();
    // SAFETY: every callback entry is an Option or a raw pointer: all-zero
    // means "none".
    let cb: BlCallbacks = unsafe { std::mem::zeroed() };
    // SAFETY: valid NUL terminated strings.
    let core = unsafe {
        bl_core_new(
            data.as_ptr(),
            args.as_ptr(),
            lang.as_ptr(),
            platform.as_ptr(),
            cb,
        )
    };
    assert!(!core.is_null());
    tick(core);

    // A command that panics answers with an error.
    let r = call(core, r#"{"cmd":"debug.panic"}"#);
    assert!(r.contains("\"error\""), "{r}");
    // The next tick tells the user, once.
    let events = tick(core);
    assert!(events.contains("interner Fehler") && events.contains("\"error\":true"), "{events}");
    assert!(!tick(core).contains("interner Fehler"));
    // And everything else still works.
    let q = call(core, r#"{"cmd":"queue.list"}"#);
    assert!(q.starts_with('['), "{q}");

    // A tick that panics still answers (with no events).
    call(core, r#"{"cmd":"debug.panic","in":"tick"}"#);
    assert_eq!(tick(core), "[]");
    tick(core);

    // Audio that panics gives silence for that buffer.
    call(core, r#"{"cmd":"debug.panic","in":"audio"}"#);
    let mut buf = vec![0.5f32; 512];
    // SAFETY: room for 256 stereo frames.
    unsafe { bl_audio_render(buf.as_mut_ptr(), 256, 0) };
    assert!(buf.iter().all(|s| *s == 0.0));
    call(core, r#"{"cmd":"debug.panic","in":"audio"}"#);
    let mut pcm = vec![7u8; 256 * 2 * 2];
    // SAFETY: room for 256 frames of 2 channels of 16-bit samples.
    unsafe { bl_audio_render_pcm(pcm.as_mut_ptr(), 256, 0, 2, 0) };
    assert!(pcm.iter().all(|b| *b == 0));

    // Null pointers and broken JSON are no reason to panic either.
    // SAFETY: the functions accept null.
    unsafe {
        assert!(bl_core_command(std::ptr::null_mut(), std::ptr::null()).is_null());
        assert!(bl_core_tick(std::ptr::null_mut()).is_null());
    }
    let r = call(core, "{\"cmd\": [1, 2");
    assert!(r.starts_with('{') || r.starts_with('['), "{r}");

    // SAFETY: created above, freed once.
    unsafe { bl_core_free(core) };
    let _ = std::fs::remove_dir_all(&dir);
}
