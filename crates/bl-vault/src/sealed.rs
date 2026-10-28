//! Sealed boxes: a file key encrypted to a recipient's X25519 public key.
//!
//! `ephemeral public key (32) ‖ nonce (24) ‖ ciphertext ‖ tag`. The key is
//! HKDF over the X25519 shared secret and both public keys; the associated
//! data names the file and the recipient, so a sealed key cannot be moved
//! to another file or user.

use crate::identity::Identity;
use crate::{aead, derive, random, Error, Secret};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const LABEL: &str = "beatlumen seal v1";

fn box_key(shared: &[u8; 32], eph: &[u8; 32], recipient: &[u8; 32]) -> Secret {
    let mut ikm = Vec::with_capacity(96);
    ikm.extend_from_slice(shared);
    ikm.extend_from_slice(eph);
    ikm.extend_from_slice(recipient);
    let k = derive(&ikm, LABEL);
    ikm.zeroize();
    k
}

/// Seals `msg` for the holder of `recipient_dh`.
pub fn seal(recipient_dh: &[u8; 32], aad: &[u8], msg: &[u8]) -> Result<Vec<u8>, Error> {
    let mut e = [0u8; 32];
    random(&mut e)?;
    let eph = StaticSecret::from(e);
    e.zeroize();
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&PublicKey::from(*recipient_dh));
    if !shared.was_contributory() {
        return Err(Error::Format("invalid public key"));
    }
    let key = box_key(shared.as_bytes(), &eph_pub, recipient_dh);
    let mut out = eph_pub.to_vec();
    out.extend(aead::seal(&key, aad, msg)?);
    Ok(out)
}

/// Opens a box sealed to `identity`.
pub fn open(identity: &Identity, aad: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() < 32 + aead::OVERHEAD {
        return Err(Error::Decrypt);
    }
    let eph_pub: [u8; 32] = data[..32].try_into().expect("32");
    let shared = identity.dh_secret().diffie_hellman(&PublicKey::from(eph_pub));
    if !shared.was_contributory() {
        return Err(Error::Decrypt);
    }
    let me = PublicKey::from(identity.dh_secret()).to_bytes();
    let key = box_key(shared.as_bytes(), &eph_pub, &me);
    aead::open(&key, aad, &data[32..])
}

/// Associated data of a sealed file key.
pub fn file_key_aad(file_id: &[u8; 16], recipient: &str) -> Vec<u8> {
    let mut a = b"beatlumen file key v1\0".to_vec();
    a.extend_from_slice(file_id);
    a.extend_from_slice(recipient.as_bytes());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open() {
        let bob = Identity::generate().unwrap();
        let eve = Identity::generate().unwrap();
        let aad = file_key_aad(&[7; 16], "bob");
        let boxed = seal(&bob.public().dh, &aad, b"file key").unwrap();
        assert_eq!(open(&bob, &aad, &boxed).unwrap(), b"file key");
        // Other recipient, other file or other name: no.
        assert!(open(&eve, &aad, &boxed).is_err());
        assert!(open(&bob, &file_key_aad(&[8; 16], "bob"), &boxed).is_err());
        assert!(open(&bob, &file_key_aad(&[7; 16], "eve"), &boxed).is_err());
        let mut t = boxed.clone();
        t[5] ^= 1; // changed ephemeral key
        assert!(open(&bob, &aad, &t).is_err());
        // Low-order public keys are refused.
        assert!(seal(&[0; 32], &aad, b"x").is_err());
    }
}
