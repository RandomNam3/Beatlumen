//! Integrity manifest of the web client (see "Web client integrity" in
//! `docs/ENCRYPTION.md`).
//!
//! `manifest.json` lists the SHA-256 of every file of the web client
//! (uncompressed content); `manifest.sig` holds the base64 Ed25519
//! signature of the exact bytes of `manifest.json`, made with the
//! project's release key. Official builds compile the public release key
//! in (`BEATLUMEN_RELEASE_KEY`, 64 hex digits, at build time).

use crate::{sha256, Error};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::collections::BTreeMap;

pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.sig";

/// The public release key of official builds, if this build has one.
pub fn release_key() -> Option<[u8; 32]> {
    parse_hex_key(option_env!("BEATLUMEN_RELEASE_KEY")?)
}

pub fn parse_hex_key(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let v: Option<Vec<u8>> = (0..32)
        .map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect();
    v?.try_into().ok()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub version: String,
    /// Path relative to the web root -> SHA-256 of the content.
    pub files: BTreeMap<String, [u8; 32]>,
}

impl Manifest {
    /// Builds a manifest from (path, content) pairs.
    pub fn new(version: &str, files: &[(String, Vec<u8>)]) -> Manifest {
        Manifest {
            version: version.to_string(),
            files: files
                .iter()
                .map(|(p, d)| (p.clone(), sha256(&[d])))
                .collect(),
        }
    }

    /// The canonical JSON text (sorted, one file per line).
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\n  \"version\": ");
        bl_json::write_string(&mut s, &self.version);
        s.push_str(",\n  \"files\": {");
        for (i, (p, h)) in self.files.iter().enumerate() {
            s.push_str(if i == 0 { "\n    " } else { ",\n    " });
            bl_json::write_string(&mut s, p);
            s.push_str(": \"");
            s.push_str(&hex(h));
            s.push('"');
        }
        s.push_str("\n  }\n}\n");
        s
    }

    pub fn parse(data: &[u8]) -> Result<Manifest, Error> {
        let v = bl_json::parse_bytes(data).map_err(|_| Error::Format("manifest is not JSON"))?;
        let mut files = BTreeMap::new();
        for (p, h) in v
            .get("files")
            .as_object()
            .ok_or(Error::Format("manifest without files"))?
            .iter()
        {
            let h = h
                .as_str()
                .and_then(parse_hex_key)
                .ok_or(Error::Format("manifest hash"))?;
            if p.is_empty() || p.starts_with('/') || p.split('/').any(|s| s == ".." || s == ".") {
                return Err(Error::Format("manifest path"));
            }
            files.insert(p.to_string(), h);
        }
        Ok(Manifest {
            version: v.str_or("version", "").to_string(),
            files,
        })
    }

    /// Whether `data` is the listed content of `path`.
    pub fn matches(&self, path: &str, data: &[u8]) -> bool {
        self.files
            .get(path)
            .is_some_and(|h| crate::ct_eq(h, &sha256(&[data])))
    }
}

/// Signs the manifest bytes with a release key (32-byte seed).
pub fn sign(seed: &[u8; 32], manifest: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed).sign(manifest).to_bytes()
}

/// The public key of a release key seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// Checks the signature (base64 text of `manifest.sig`) of the manifest bytes.
pub fn verify(public: &[u8; 32], manifest: &[u8], signature_b64: &str) -> Result<(), Error> {
    let sig = base64_decode(signature_b64.trim()).ok_or(Error::Signature)?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| Error::Signature)?;
    let key = VerifyingKey::from_bytes(public).map_err(|_| Error::Signature)?;
    key.verify_strict(manifest, &Signature::from_bytes(&sig))
        .map_err(|_| Error::Signature)
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sign_verify() {
        let files = vec![
            ("index.html".to_string(), b"<html>".to_vec()),
            ("beatlumen.wasm".to_string(), vec![0, 97, 115, 109]),
        ];
        let m = Manifest::new("1.0", &files);
        let json = m.to_json();
        assert_eq!(Manifest::parse(json.as_bytes()).unwrap(), m);
        assert!(m.matches("index.html", b"<html>"));
        assert!(!m.matches("index.html", b"<html> "));
        assert!(!m.matches("other.js", b""));

        let seed = [7u8; 32];
        let public = public_key(&seed);
        let sig = base64_encode(&sign(&seed, json.as_bytes()));
        verify(&public, json.as_bytes(), &sig).unwrap();
        let changed = json.replace("1.0", "1.1");
        assert!(verify(&public, changed.as_bytes(), &sig).is_err());
        assert!(verify(&public_key(&[8; 32]), json.as_bytes(), &sig).is_err());
        assert!(verify(&public, json.as_bytes(), "AAAA").is_err());
        assert_eq!(parse_hex_key(&hex(&public)), Some(public));
        assert!(Manifest::parse(br#"{"files":{"../x":"00"}}"#).is_err());
    }
}
