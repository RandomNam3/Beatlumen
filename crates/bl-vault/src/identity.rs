//! A user's identity: an Ed25519 key for signatures and an X25519 key to
//! receive sealed file keys. The X25519 public key is signed by the Ed25519
//! key, so both travel together and cannot be swapped separately.

use crate::{aead, random, sha256, Error, Secret};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const BINDING: &[u8] = b"beatlumen identity binding v1";
const WRAP_AAD: &[u8] = b"beatlumen identity v1";

/// The private half (kept on the device, stored on the server only wrapped
/// with the master key).
pub struct Identity {
    sign: SigningKey,
    dh: StaticSecret,
}

/// The public half, as published on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicIdentity {
    pub sign: [u8; 32],
    pub dh: [u8; 32],
    /// Ed25519 signature over the X25519 key.
    pub binding: [u8; 64],
}

impl Identity {
    pub fn generate() -> Result<Identity, Error> {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        random(&mut a)?;
        random(&mut b)?;
        let id = Identity {
            sign: SigningKey::from_bytes(&a),
            dh: StaticSecret::from(b),
        };
        a.zeroize();
        b.zeroize();
        Ok(id)
    }

    pub fn public(&self) -> PublicIdentity {
        let dh = PublicKey::from(&self.dh).to_bytes();
        let mut msg = BINDING.to_vec();
        msg.extend_from_slice(&dh);
        PublicIdentity {
            sign: self.sign.verifying_key().to_bytes(),
            dh,
            binding: self.sign.sign(&msg).to_bytes(),
        }
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.sign.sign(msg).to_bytes()
    }

    pub(crate) fn dh_secret(&self) -> &StaticSecret {
        &self.dh
    }

    /// Encrypts the private keys with the master key (for the server).
    pub fn wrap(&self, master: &Secret) -> Result<Vec<u8>, Error> {
        let mut raw = [0u8; 64];
        raw[..32].copy_from_slice(&self.sign.to_bytes());
        raw[32..].copy_from_slice(&self.dh.to_bytes());
        let out = aead::seal(master, WRAP_AAD, &raw);
        raw.zeroize();
        out
    }

    pub fn unwrap(master: &Secret, data: &[u8]) -> Result<Identity, Error> {
        let mut raw = aead::open(master, WRAP_AAD, data)?;
        if raw.len() != 64 {
            raw.zeroize();
            return Err(Error::Format("identity length"));
        }
        let mut a: [u8; 32] = raw[..32].try_into().expect("32");
        let mut b: [u8; 32] = raw[32..].try_into().expect("32");
        raw.zeroize();
        let id = Identity {
            sign: SigningKey::from_bytes(&a),
            dh: StaticSecret::from(b),
        };
        a.zeroize();
        b.zeroize();
        Ok(id)
    }
}

impl PublicIdentity {
    /// Checks that the X25519 key belongs to the Ed25519 key.
    pub fn verify(&self) -> Result<(), Error> {
        let mut msg = BINDING.to_vec();
        msg.extend_from_slice(&self.dh);
        self.verify_signature(&msg, &self.binding)
    }

    pub fn verify_signature(&self, msg: &[u8], sig: &[u8; 64]) -> Result<(), Error> {
        let key = VerifyingKey::from_bytes(&self.sign).map_err(|_| Error::Signature)?;
        key.verify(msg, &Signature::from_bytes(sig))
            .map_err(|_| Error::Signature)
    }

    /// 128 bytes: sign ‖ dh ‖ binding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(128);
        v.extend_from_slice(&self.sign);
        v.extend_from_slice(&self.dh);
        v.extend_from_slice(&self.binding);
        v
    }

    /// Parses and verifies the binding.
    pub fn from_bytes(b: &[u8]) -> Result<PublicIdentity, Error> {
        if b.len() != 128 {
            return Err(Error::Format("public identity length"));
        }
        let p = PublicIdentity {
            sign: b[..32].try_into().expect("32"),
            dh: b[32..64].try_into().expect("32"),
            binding: b[64..].try_into().expect("64"),
        };
        p.verify()?;
        Ok(p)
    }

    /// Short fingerprint of this identity (hex, for logs and key pinning).
    pub fn fingerprint(&self) -> String {
        sha256(&[b"beatlumen fingerprint v1", &self.sign])[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Safety number of two users: 60 digits in 12 groups of 5, the same on
/// both sides. Equal numbers mean neither key was substituted.
pub fn safety_number(a: &PublicIdentity, b: &PublicIdentity) -> String {
    let (x, y) = if a.sign <= b.sign { (a, b) } else { (b, a) };
    let mut h = sha256(&[b"beatlumen safety number v1", &x.sign, &y.sign]);
    // Iterate to slow down brute-force searches for colliding numbers.
    for _ in 0..4096 {
        h = sha256(&[&h, &x.sign, &y.sign]);
    }
    let mut groups = Vec::with_capacity(12);
    for i in 0..12 {
        // 5 digits from 30 bits (bias negligible for this purpose).
        let chunk = &h[i * 2 + (i / 2)..];
        let v = u32::from_be_bytes([0, chunk[0], chunk[1], chunk[2]]) % 100_000;
        groups.push(format!("{v:05}"));
    }
    groups.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_unwrap_and_binding() {
        let id = Identity::generate().unwrap();
        let p = id.public();
        p.verify().unwrap();
        assert_eq!(PublicIdentity::from_bytes(&p.to_bytes()).unwrap(), p);
        let master = Secret::random().unwrap();
        let w = id.wrap(&master).unwrap();
        let back = Identity::unwrap(&master, &w).unwrap();
        assert_eq!(back.public(), p);
        assert!(Identity::unwrap(&Secret::random().unwrap(), &w).is_err());
        // A swapped X25519 key breaks the binding.
        let other = Identity::generate().unwrap().public();
        let mut forged = p.clone();
        forged.dh = other.dh;
        assert_eq!(forged.verify(), Err(Error::Signature));
        assert!(PublicIdentity::from_bytes(&forged.to_bytes()).is_err());
    }

    #[test]
    fn signatures_and_safety_numbers() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let sig = a.sign(b"msg");
        a.public().verify_signature(b"msg", &sig).unwrap();
        assert!(a.public().verify_signature(b"msh", &sig).is_err());
        assert!(b.public().verify_signature(b"msg", &sig).is_err());
        let n1 = safety_number(&a.public(), &b.public());
        let n2 = safety_number(&b.public(), &a.public());
        assert_eq!(n1, n2);
        assert_eq!(n1.len(), 12 * 5 + 11);
        let c = Identity::generate().unwrap();
        assert_ne!(n1, safety_number(&a.public(), &c.public()));
    }
}
