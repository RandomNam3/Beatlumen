//! The cryptography RAOP needs: RSA with the protocol's key (challenge
//! response, unwrapping the stream key), AES-128-CBC for the audio, SHA-1
//! and MD5 for padding and password digests, Base64 for the RTSP headers.
//!
//! AirPlay 2 adds pairing and encrypted transport: SRP-6a with SHA-512
//! ([`srp`], [`sha512`]), X25519 and Ed25519 ([`curve25519`], [`ed25519`])
//! and ChaCha20-Poly1305 ([`chacha`]).

pub mod aes;
pub mod bigint;
pub mod chacha;
pub mod curve25519;
pub mod ed25519;
pub mod hash;
mod key;
pub mod sha512;
pub mod srp;

/// Signs the Apple-Challenge response (PKCS#1 v1.5 type 1 padding of the
/// raw message, no digest), as AirPort Express receivers do.
pub fn rsa_sign_raw(msg: &[u8]) -> Option<Vec<u8>> {
    let k = key::N.len();
    if msg.len() + 11 > k {
        return None;
    }
    let mut em = vec![0xffu8; k];
    em[0] = 0;
    em[1] = 1;
    em[k - msg.len() - 1] = 0;
    em[k - msg.len()..].copy_from_slice(msg);
    Some(bigint::mod_pow(&em, &key::D, &key::N))
}

/// Unwraps the stream's AES key (RSA-OAEP with SHA-1, RFC 8017).
pub fn rsa_decrypt_oaep(ct: &[u8]) -> Option<Vec<u8>> {
    let k = key::N.len();
    if ct.len() != k || ct >= &key::N[..] {
        return None;
    }
    let em = bigint::mod_pow(ct, &key::D, &key::N);
    let h = 20;
    if em[0] != 0 {
        return None;
    }
    let (masked_seed, masked_db) = em[1..].split_at(h);
    let seed: Vec<u8> = masked_seed
        .iter()
        .zip(mgf1(masked_db, h))
        .map(|(a, b)| a ^ b)
        .collect();
    let db: Vec<u8> = masked_db
        .iter()
        .zip(mgf1(&seed, masked_db.len()))
        .map(|(a, b)| a ^ b)
        .collect();
    if db[..h] != hash::sha1(b"")[..] {
        return None;
    }
    let rest = &db[h..];
    let one = rest.iter().position(|&b| b != 0)?;
    (rest[one] == 1).then(|| rest[one + 1..].to_vec())
}

/// Wraps a stream key for a receiver (RSA-OAEP with SHA-1 and the
/// protocol's public key, exponent 65537).
pub fn rsa_encrypt_oaep(msg: &[u8]) -> Option<Vec<u8>> {
    let k = key::N.len();
    let h = 20;
    if msg.len() + 2 * h + 2 > k {
        return None;
    }
    let mut db = hash::sha1(b"").to_vec();
    db.resize(k - h - 1 - msg.len() - 1, 0);
    db.push(1);
    db.extend_from_slice(msg);
    let seed = random_bytes(h);
    let masked_db: Vec<u8> = db
        .iter()
        .zip(mgf1(&seed, db.len()))
        .map(|(a, b)| a ^ b)
        .collect();
    let masked_seed: Vec<u8> = seed
        .iter()
        .zip(mgf1(&masked_db, h))
        .map(|(a, b)| a ^ b)
        .collect();
    let mut em = vec![0u8];
    em.extend_from_slice(&masked_seed);
    em.extend_from_slice(&masked_db);
    let c = bigint::mod_pow(&em, &[0x01, 0x00, 0x01], &key::N);
    // Left-pad to the modulus length.
    let mut out = vec![0u8; k - c.len().min(k)];
    out.extend_from_slice(&c[c.len().saturating_sub(k)..]);
    Some(out)
}

fn mgf1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut counter = 0u32;
    while out.len() < len {
        let mut input = seed.to_vec();
        input.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hash::sha1(&input));
        counter += 1;
    }
    out.truncate(len);
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 without padding (Apple-Response, nonces).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for chunk in data.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, &b| acc << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..chunk.len() + 1 {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Base64 with or without padding; whitespace is ignored.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
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

/// Random bytes: the operating system's generator (`/dev/urandom` where
/// it exists) combined with the hash keys the standard library seeds from
/// it, so either source alone keeps the output unpredictable.
pub fn random_bytes(n: usize) -> Vec<u8> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(n + 8);
    let mut i = 0u64;
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        out.extend_from_slice(&h.finish().to_le_bytes());
        i += 1;
    }
    out.truncate(n);
    if let Some(os) = os_random(n) {
        for (o, r) in out.iter_mut().zip(os) {
            *o ^= r;
        }
    }
    out
}

#[cfg(unix)]
fn os_random(n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .ok()?;
    Some(buf)
}

#[cfg(not(unix))]
fn os_random(_n: usize) -> Option<Vec<u8>> {
    None
}

/// Hex decoding for the tests' vectors.
#[cfg(test)]
pub(crate) mod test_util {
    pub fn unhex(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
        s.chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    pub fn unhex32(s: &str) -> [u8; 32] {
        unhex(s).try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oaep_roundtrip() {
        let key = random_bytes(16);
        let wrapped = rsa_encrypt_oaep(&key).unwrap();
        assert_eq!(wrapped.len(), 256);
        assert_eq!(rsa_decrypt_oaep(&wrapped).unwrap(), key);
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE");
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zm9v\r\nYmE").unwrap(), b"fooba");
        assert!(base64_decode("Zm9v*").is_none());
    }

    #[test]
    fn oaep_roundtrip_with_public_key() {
        // Encrypt with the public exponent (as senders do) and unwrap.
        let k = key::N.len();
        let msg = [7u8; 16];
        let seed = [0x42u8; 20];
        let mut db = hash::sha1(b"").to_vec();
        db.resize(k - 1 - 20 - msg.len() - 1, 0);
        db.push(1);
        db.extend_from_slice(&msg);
        let masked_db: Vec<u8> = db
            .iter()
            .zip(mgf1(&seed, db.len()))
            .map(|(a, b)| a ^ b)
            .collect();
        let masked_seed: Vec<u8> = seed
            .iter()
            .zip(mgf1(&masked_db, 20))
            .map(|(a, b)| a ^ b)
            .collect();
        let mut em = vec![0u8];
        em.extend_from_slice(&masked_seed);
        em.extend_from_slice(&masked_db);
        let ct = bigint::mod_pow(&em, &[1, 0, 1], &key::N);
        assert_eq!(rsa_decrypt_oaep(&ct).unwrap(), msg);
    }

    #[test]
    fn signature_verifies_with_public_exponent() {
        let sig = rsa_sign_raw(b"challenge").unwrap();
        let em = bigint::mod_pow(&sig, &[1, 0, 1], &key::N);
        assert_eq!(&em[..2], &[0, 1]);
        assert!(em.ends_with(b"\0challenge"));
    }
}
