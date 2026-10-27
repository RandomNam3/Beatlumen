//! Cryptographically secure random bytes from the operating system.
//!
//! * Unix: `/dev/urandom`.
//! * Windows: `BCryptGenRandom` with the system preferred RNG.
//!
//! Used for login tokens, playlist ids and salts. There is deliberately no
//! fallback to a weak generator: callers get an error instead.

use std::io;

#[cfg(unix)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::ffi::c_void;

    /// `BCRYPT_USE_SYSTEM_PREFERRED_RNG`: no algorithm handle needed.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, len: u32, flags: u32) -> i32;
    }

    pub fn fill(buf: &mut [u8]) -> std::io::Result<()> {
        for chunk in buf.chunks_mut(u32::MAX as usize) {
            // SAFETY: `chunk` is a valid, writable buffer of exactly
            // `chunk.len()` bytes for the duration of the call; a null
            // algorithm handle is allowed together with
            // BCRYPT_USE_SYSTEM_PREFERRED_RNG.
            let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), chunk.as_mut_ptr(), chunk.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
            if status != 0 {
                return Err(std::io::Error::other(format!("BCryptGenRandom failed with NTSTATUS {status:#x}")));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    win::fill(buf)
}

#[cfg(not(any(unix, windows)))]
pub fn fill(_buf: &mut [u8]) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no secure random source on this platform"))
}

/// `n` random bytes as lower case hex (2n characters).
pub fn hex(n: usize) -> io::Result<String> {
    let mut b = vec![0u8; n];
    fill(&mut b)?;
    Ok(crate::util::hex(&b))
}

#[cfg(test)]
mod tests {
    #[test]
    fn random_hex() {
        let a = super::hex(16).unwrap();
        let b = super::hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
