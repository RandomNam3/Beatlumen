//! XChaCha20-Poly1305 with random nonces: `nonce (24) ‖ ciphertext ‖ tag`.

use crate::{random, Error, Secret};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
/// Bytes added to the plaintext.
pub const OVERHEAD: usize = NONCE_LEN + TAG_LEN;

/// Encrypts with a fresh random nonce.
pub fn seal(key: &Secret, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let mut nonce = [0u8; NONCE_LEN];
    random(&mut nonce)?;
    let mut out = nonce.to_vec();
    out.extend(seal_with_nonce(key, &nonce, aad, plaintext));
    Ok(out)
}

/// Decrypts `nonce ‖ ciphertext ‖ tag`.
pub fn open(key: &Secret, aad: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() < OVERHEAD {
        return Err(Error::Decrypt);
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    open_with_nonce(key, nonce.try_into().expect("24 bytes"), aad, ct)
}

/// Encrypts with a given nonce (the caller guarantees it is never reused
/// with this key).
pub(crate) fn seal_with_nonce(
    key: &Secret,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    XChaCha20Poly1305::new(key.as_bytes().into())
        .encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext, aad })
        .expect("encryption of in-memory data does not fail")
}

pub(crate) fn open_with_nonce(
    key: &Secret,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ct: &[u8],
) -> Result<Vec<u8>, Error> {
    XChaCha20Poly1305::new(key.as_bytes().into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| Error::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let k = Secret::random().unwrap();
        let c = seal(&k, b"aad", b"hello").unwrap();
        assert_eq!(c.len(), 5 + OVERHEAD);
        assert_eq!(open(&k, b"aad", &c).unwrap(), b"hello");
        assert_eq!(open(&k, b"other", &c), Err(Error::Decrypt));
        let mut t = c.clone();
        t[30] ^= 1;
        assert_eq!(open(&k, b"aad", &t), Err(Error::Decrypt));
        assert_eq!(open(&Secret::random().unwrap(), b"aad", &c), Err(Error::Decrypt));
        assert_eq!(open(&k, b"aad", &c[..10]), Err(Error::Decrypt));
        // Fresh nonces: the same message encrypts differently.
        assert_ne!(seal(&k, b"aad", b"hello").unwrap(), c);
    }

    #[test]
    fn xchacha20poly1305_test_vector() {
        // draft-irtf-cfrg-xchacha-03, appendix A.3.1.
        let hex = |s: &str| -> Vec<u8> {
            (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
        };
        let key = Secret::from_bytes(
            hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f")
                .try_into()
                .unwrap(),
        );
        let nonce: [u8; 24] = hex("404142434445464748494a4b4c4d4e4f5051525354555657")
            .try_into()
            .unwrap();
        let aad = hex("50515253c0c1c2c3c4c5c6c7");
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let ct = seal_with_nonce(&key, &nonce, &aad, pt);
        assert_eq!(
            &ct[..16],
            &hex("bd6d179d3e83d43b9576579493c0e939")[..],
        );
        assert_eq!(&ct[ct.len() - 16..], &hex("c0875924c1c7987947deafd8780acf49")[..]);
        assert_eq!(open_with_nonce(&key, &nonce, &aad, &ct).unwrap(), pt);
    }
}
