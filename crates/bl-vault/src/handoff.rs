//! Hand-over of an unlocked account from the browser sign-in page to an
//! app: the app makes a one-time X25519 key and shows a pairing code
//! derived from it; the page shows the same code, seals the master key and
//! session to that key and returns the box to the app. Nobody else (not
//! the server, not another app) can open it.

use crate::{sealed, sha256, Error};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

fn aad(state: &str) -> Vec<u8> {
    let mut a = b"beatlumen handoff v1\0".to_vec();
    a.extend_from_slice(state.as_bytes());
    a
}

/// The pairing code of a one-time key: 6 digits, "123 456".
pub fn pairing_code(public: &[u8; 32]) -> String {
    let h = sha256(&[b"beatlumen pairing v1", public]);
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Seals `payload` for the app (page side). `state` binds the box to the
/// sign-in request.
pub fn seal(app_public: &[u8; 32], state: &str, payload: &[u8]) -> Result<Vec<u8>, Error> {
    sealed::seal(app_public, &aad(state), payload)
}

/// The app's one-time key.
pub struct HandoffKey(StaticSecret);

impl HandoffKey {
    pub fn generate() -> Result<HandoffKey, Error> {
        let mut b = [0u8; 32];
        crate::random(&mut b)?;
        let k = HandoffKey(StaticSecret::from(b));
        b.zeroize();
        Ok(k)
    }

    pub fn public(&self) -> [u8; 32] {
        PublicKey::from(&self.0).to_bytes()
    }

    pub fn pairing_code(&self) -> String {
        pairing_code(&self.public())
    }

    pub fn open(&self, state: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
        sealed::open_with(&self.0, &aad(state), data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open() {
        let k = HandoffKey::generate().unwrap();
        let code = k.pairing_code();
        assert_eq!(code.len(), 7);
        assert_eq!(code, pairing_code(&k.public()));
        let boxed = seal(&k.public(), "state-1", b"master").unwrap();
        assert_eq!(k.open("state-1", &boxed).unwrap(), b"master");
        assert!(k.open("state-2", &boxed).is_err());
        assert!(HandoffKey::generate()
            .unwrap()
            .open("state-1", &boxed)
            .is_err());
    }
}
