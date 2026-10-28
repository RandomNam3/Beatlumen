//! End-to-end encryption of user files (design: `docs/ENCRYPTION.md`).
//!
//! * [`keys`]: password and recovery code derivation, the master key and
//!   what the server stores for an account.
//! * [`identity`]: a user's Ed25519/X25519 key pair, safety numbers.
//! * [`sealed`]: file keys sealed to a recipient's public key (sharing).
//! * [`file`]: the encrypted file format (chunked, random access,
//!   encrypted metadata, owner signature).
//!
//! Everything here runs on the user's device; the server only ever stores
//! the outputs.

pub mod aead;
pub mod file;
pub mod identity;
pub mod keys;
pub mod recovery;
pub mod sealed;

use zeroize::{Zeroize, ZeroizeOnDrop};

/// Errors are deliberately coarse: they must not tell an attacker which
/// check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Wrong key or password, or the data was changed.
    Decrypt,
    /// Malformed input (lengths, versions, encodings).
    Format(&'static str),
    /// Parameters below the minimum this client accepts.
    WeakParameters,
    /// A signature does not verify.
    Signature,
    /// The operating system's random number generator failed.
    Random,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Decrypt => write!(f, "decryption failed (wrong key or changed data)"),
            Error::Format(m) => write!(f, "invalid data: {m}"),
            Error::WeakParameters => write!(f, "key derivation parameters are too weak"),
            Error::Signature => write!(f, "signature does not verify"),
            Error::Random => write!(f, "no random numbers available"),
        }
    }
}

impl std::error::Error for Error {}

/// A 256-bit secret, zeroed when dropped.
#[derive(Clone, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct Secret(pub(crate) [u8; 32]);

impl Secret {
    pub fn random() -> Result<Secret, Error> {
        let mut b = [0u8; 32];
        random(&mut b)?;
        Ok(Secret(b))
    }

    pub fn from_bytes(b: [u8; 32]) -> Secret {
        Secret(b)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Fills `buf` from the operating system's generator.
pub fn random(buf: &mut [u8]) -> Result<(), Error> {
    getrandom::getrandom(buf).map_err(|_| Error::Random)
}

/// HKDF-SHA-256 expansion of `ikm` for a fixed label.
pub fn derive(ikm: &[u8], label: &str) -> Secret {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm);
    let mut out = [0u8; 32];
    hk.expand(label.as_bytes(), &mut out)
        .expect("32 bytes is a valid HKDF length");
    Secret(out)
}

/// SHA-256 of the concatenated parts.
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Constant-time comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
