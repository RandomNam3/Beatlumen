//! Server host keys: checking the key exchange signature (Ed25519, ECDSA
//! P-256, RSA with SHA-2) and the fingerprint users compare and pin.
//!
//! RSA keys below 2048 bits and SHA-1 signatures (`ssh-rsa`) are refused.

use crate::wire::Reader;
use crate::Error;
use sha2::{Digest, Sha256};

/// Host key algorithms in order of preference.
pub(crate) const ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "rsa-sha2-512",
    "rsa-sha2-256",
];

/// The SHA-256 fingerprint of a public key blob as OpenSSH shows it
/// (`SHA256:` and unpadded base64).
pub fn fingerprint(blob: &[u8]) -> String {
    format!(
        "SHA256:{}",
        crate::base64::encode(&Sha256::digest(blob), false)
    )
}

/// A fingerprint as entered by a user, comparable to [`fingerprint`]'s:
/// with or without the `SHA256:` prefix and padding.
pub(crate) fn normalize_pin(pin: &str) -> String {
    let p = pin.trim();
    let p = p.strip_prefix("SHA256:").unwrap_or(p);
    format!("SHA256:{}", p.trim_end_matches('='))
}

fn bad(what: &str) -> Error {
    Error::Protocol(format!("invalid host key: {what}"))
}

fn signature_fails() -> Error {
    Error::Protocol("the server's host key signature does not verify".into())
}

/// Checks the server's signature `sig` (SSH encoding) of the exchange hash
/// `data` with the host key `blob` for the negotiated algorithm `alg`.
pub(crate) fn verify(alg: &str, blob: &[u8], sig: &[u8], data: &[u8]) -> Result<(), Error> {
    let mut k = Reader::new(blob);
    let key_type = k.str()?;
    let mut s = Reader::new(sig);
    let sig_alg = s.str()?;
    let sig = s.string()?;
    s.end()?;
    if sig_alg != alg {
        return Err(Error::Protocol(format!(
            "host key signature {sig_alg} instead of {alg}"
        )));
    }
    match alg {
        "ssh-ed25519" => {
            if key_type != alg {
                return Err(bad("type"));
            }
            let pk: [u8; 32] = k.string()?.try_into().map_err(|_| bad("length"))?;
            k.end()?;
            let sig: [u8; 64] = sig.try_into().map_err(|_| signature_fails())?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| bad("point"))?;
            key.verify_strict(data, &ed25519_dalek::Signature::from_bytes(&sig))
                .map_err(|_| signature_fails())
        }
        "ecdsa-sha2-nistp256" => {
            if key_type != alg || k.str()? != "nistp256" {
                return Err(bad("type"));
            }
            let q = k.string()?;
            k.end()?;
            // The signature holds r and s as mpints; ring wants them fixed.
            let mut rs = Reader::new(sig);
            let mut fixed = [0u8; 64];
            for half in fixed.chunks_mut(32) {
                let v = rs.mpint()?;
                if v.len() > 32 {
                    return Err(signature_fails());
                }
                half[32 - v.len()..].copy_from_slice(v);
            }
            rs.end()?;
            ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, q)
                .verify(data, &fixed)
                .map_err(|_| signature_fails())
        }
        "rsa-sha2-256" | "rsa-sha2-512" => {
            if key_type != "ssh-rsa" {
                return Err(bad("type"));
            }
            let e = k.mpint()?;
            let n = k.mpint()?;
            k.end()?;
            if n.len() < 256 {
                return Err(Error::Protocol(
                    "the server's RSA host key is shorter than 2048 bits".into(),
                ));
            }
            // Signatures may come without leading zeros (OpenSSH pads them).
            if sig.len() > n.len() {
                return Err(signature_fails());
            }
            let mut padded = vec![0u8; n.len()];
            padded[n.len() - sig.len()..].copy_from_slice(sig);
            let params = if alg == "rsa-sha2-512" {
                &ring::signature::RSA_PKCS1_2048_8192_SHA512
            } else {
                &ring::signature::RSA_PKCS1_2048_8192_SHA256
            };
            ring::signature::RsaPublicKeyComponents { n, e }
                .verify(params, data, &padded)
                .map_err(|_| signature_fails())
        }
        _ => Err(Error::Protocol(format!("unsupported host key type {alg}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Put;
    use ed25519_dalek::Signer;

    #[test]
    fn fingerprint_format() {
        // `ssh-keygen -lf` of the public key in key.rs's tests.
        let blob = crate::base64::decode(
            "AAAAC3NzaC1lZDI1NTE5AAAAIOcSoSFkMR632A++zjQuJtDKK9AuHarqw5+RxSU9X944",
        )
        .unwrap();
        assert_eq!(
            fingerprint(&blob),
            "SHA256:wI4KuqST49UnQBEb6o8oKBn0P1fXQ2em3W/kWQvZHGE"
        );
        assert_eq!(normalize_pin(" x7Vy8+AA= "), "SHA256:x7Vy8+AA");
        assert_eq!(normalize_pin("SHA256:abc"), "SHA256:abc");
    }

    #[test]
    fn ed25519_signatures() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let mut blob = Vec::new();
        blob.string(b"ssh-ed25519");
        blob.string(key.verifying_key().as_bytes());
        let mut sig = Vec::new();
        sig.string(b"ssh-ed25519");
        sig.string(&key.sign(b"exchange hash").to_bytes());
        verify("ssh-ed25519", &blob, &sig, b"exchange hash").unwrap();
        assert!(verify("ssh-ed25519", &blob, &sig, b"other hash").is_err());
        // The negotiated algorithm must match the signature's.
        assert!(verify("rsa-sha2-256", &blob, &sig, b"exchange hash").is_err());
        let mut short = Vec::new();
        short.string(b"ssh-ed25519");
        short.string(&[0u8; 10]);
        assert!(verify("ssh-ed25519", &blob, &short, b"exchange hash").is_err());
    }
}
