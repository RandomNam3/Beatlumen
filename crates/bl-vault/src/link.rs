//! Public links to a file (`https://<server>/share/<id>#<secret>`).
//!
//! The link's 32-byte secret only exists in the URL fragment, which
//! browsers never send to the server. The server stores the file key
//! wrapped under a key derived from that secret (and, for links with a
//! password, from Argon2id of the password); it cannot unwrap it. For
//! password links it also stores a hash of a proof derived from both, and
//! hands out the wrapped key and the file only for the right proof, so a
//! leaked link without the password gives nothing away. The proof includes
//! the secret, so the server cannot guess the password from its hash.

use crate::keys::{argon2id, KdfParams, SALT_LEN};
use crate::{aead, derive, random, Error, Secret};
use zeroize::Zeroize;

pub const SECRET_LEN: usize = 32;
/// Length of a wrapped file key.
pub const WRAPPED_LEN: usize = 32 + aead::OVERHEAD;

/// Keys a link's secret (and password) derive.
pub struct LinkKeys {
    key: Secret,
    /// Proof of the password (password links only).
    pub auth: Option<Secret>,
}

impl LinkKeys {
    /// Derives the keys; with a password this runs Argon2id (slow by design).
    pub fn derive(
        secret: &[u8; SECRET_LEN],
        password: Option<(&str, &[u8; SALT_LEN], KdfParams)>,
    ) -> Result<LinkKeys, Error> {
        let mut ikm = secret.to_vec();
        if let Some((pw, salt, params)) = password {
            let mut root = argon2id(pw, salt, params)?;
            ikm.extend_from_slice(&root);
            root.zeroize();
        }
        let keys = LinkKeys {
            key: derive(&ikm, "beatlumen link key v1"),
            auth: password.map(|_| derive(&ikm, "beatlumen link auth v1")),
        };
        ikm.zeroize();
        Ok(keys)
    }

    /// The file key, unwrapped.
    pub fn unwrap(&self, file_id: &str, wrapped: &[u8]) -> Result<Secret, Error> {
        let mut raw = aead::open(&self.key, &aad(file_id), wrapped)?;
        let out: Result<[u8; 32], _> = raw.as_slice().try_into();
        raw.zeroize();
        out.map(Secret).map_err(|_| Error::Decrypt)
    }
}

fn aad(file_id: &str) -> Vec<u8> {
    let mut a = b"beatlumen link v1 ".to_vec();
    a.extend_from_slice(file_id.as_bytes());
    a
}

/// A new link: `secret` goes into the URL fragment, the rest to the server.
pub struct NewLink {
    pub secret: [u8; SECRET_LEN],
    pub wrapped: Vec<u8>,
    /// Salt and parameters of the password (password links only).
    pub password: Option<([u8; SALT_LEN], KdfParams)>,
    /// SHA-256 of the password proof (password links only).
    pub auth_hash: Option<[u8; 32]>,
}

impl Drop for NewLink {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

/// Creates a link to the file `file_id` with key `file_key`.
pub fn create(file_id: &str, file_key: &Secret, password: Option<&str>) -> Result<NewLink, Error> {
    let mut secret = [0u8; SECRET_LEN];
    random(&mut secret)?;
    let pw = match password.filter(|p| !p.is_empty()) {
        Some(p) => {
            let mut salt = [0u8; SALT_LEN];
            random(&mut salt)?;
            Some((p, salt, KdfParams::DEFAULT))
        }
        None => None,
    };
    let keys = LinkKeys::derive(&secret, pw.as_ref().map(|(p, s, k)| (*p, s, *k)))?;
    let wrapped = aead::seal(&keys.key, &aad(file_id), file_key.as_bytes())?;
    Ok(NewLink {
        secret,
        wrapped,
        password: pw.map(|(_, s, k)| (s, k)),
        auth_hash: keys.auth.as_ref().map(|a| crate::sha256(&[a.as_bytes()])),
    })
}

/// Hex encoding of the secret for the URL fragment.
pub fn secret_to_text(s: &[u8; SECRET_LEN]) -> String {
    s.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn secret_from_text(t: &str) -> Result<[u8; SECRET_LEN], Error> {
    let t = t.trim();
    if t.len() != SECRET_LEN * 2 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Format("link secret"));
    }
    let mut out = [0u8; SECRET_LEN];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&t[2 * i..2 * i + 2], 16)
            .map_err(|_| Error::Format("link secret"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_with_and_without_password() {
        let key = Secret::random().unwrap();
        let plain = create("f1", &key, None).unwrap();
        assert!(plain.password.is_none() && plain.auth_hash.is_none());
        let k = LinkKeys::derive(&plain.secret, None).unwrap();
        assert_eq!(
            k.unwrap("f1", &plain.wrapped).unwrap().as_bytes(),
            key.as_bytes()
        );
        // Bound to the file id.
        assert_eq!(k.unwrap("f2", &plain.wrapped).err(), Some(Error::Decrypt));

        let pw = create("f1", &key, Some("geheim")).unwrap();
        let (salt, params) = pw.password.unwrap();
        let good = LinkKeys::derive(&pw.secret, Some(("geheim", &salt, params))).unwrap();
        assert_eq!(
            good.unwrap("f1", &pw.wrapped).unwrap().as_bytes(),
            key.as_bytes()
        );
        assert_eq!(
            crate::sha256(&[good.auth.as_ref().unwrap().as_bytes()]),
            pw.auth_hash.unwrap()
        );
        let bad = LinkKeys::derive(&pw.secret, Some(("falsch", &salt, params))).unwrap();
        assert!(bad.unwrap("f1", &pw.wrapped).is_err());
        assert_ne!(
            crate::sha256(&[bad.auth.unwrap().as_bytes()]),
            pw.auth_hash.unwrap()
        );
        // The password alone does not unwrap without the secret.
        let other = LinkKeys::derive(&[7; 32], Some(("geheim", &salt, params))).unwrap();
        assert!(other.unwrap("f1", &pw.wrapped).is_err());

        let t = secret_to_text(&pw.secret);
        assert_eq!(secret_from_text(&t).unwrap(), pw.secret);
        assert!(secret_from_text("xyz").is_err());
    }
}
