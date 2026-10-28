//! Web Push message encryption (RFC 8291, "aes128gcm" content coding of
//! RFC 8188) and VAPID (RFC 8292). Used for push notifications through
//! UnifiedPush: the server encrypts for the subscription's P-256 key and
//! auth secret, the push service only relays ciphertext, the app decrypts.

use crate::{random, Error};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// Record size written into the header (one record per message).
const RECORD_SIZE: u32 = 4096;
/// Largest plaintext of one message (record size minus tag and delimiter).
pub const MAX_PLAINTEXT: usize = RECORD_SIZE as usize - 16 - 1 - 86;

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .expect("valid HKDF length");
}

/// Content key and nonce (RFC 8291 section 3.4, RFC 8188 section 2.2).
fn keys(
    shared: &[u8],
    auth: &[u8; 16],
    ua_public: &[u8; 65],
    as_public: &[u8; 65],
    salt: &[u8; 16],
) -> ([u8; 16], [u8; 12]) {
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    hkdf(auth, shared, &info, &mut ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek);
    hkdf(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce);
    (cek, nonce)
}

fn point(key: &PublicKey) -> [u8; 65] {
    key.to_encoded_point(false)
        .as_bytes()
        .try_into()
        .expect("uncompressed P-256 point")
}

/// Encrypts `plaintext` for a subscription (its P-256 key `ua_public`,
/// uncompressed, and its 16-byte auth secret).
pub fn encrypt(ua_public: &[u8], auth: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let mut seed = [0u8; 32];
    random(&mut seed)?;
    let as_secret = SecretKey::from_slice(&seed).map_err(|_| Error::Random)?;
    let mut salt = [0u8; 16];
    random(&mut salt)?;
    encrypt_with(ua_public, auth, plaintext, &as_secret, &salt)
}

fn encrypt_with(
    ua_public: &[u8],
    auth: &[u8; 16],
    plaintext: &[u8],
    as_secret: &SecretKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, Error> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(Error::Format("push message too long"));
    }
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| Error::Format("push key"))?;
    let ua_bytes = point(&ua);
    let as_bytes = point(&as_secret.public_key());
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua.as_affine());
    let (cek, nonce) = keys(shared.raw_secret_bytes(), auth, &ua_bytes, &as_bytes, salt);
    let mut padded = plaintext.to_vec();
    padded.push(2); // last record
    let ct = Aes128Gcm::new(&cek.into())
        .encrypt(Nonce::from_slice(&nonce), padded.as_slice())
        .map_err(|_| Error::Decrypt)?;
    let mut out = Vec::with_capacity(86 + ct.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(65);
    out.extend_from_slice(&as_bytes);
    out.extend(ct);
    Ok(out)
}

/// A subscription's keys, kept by the app.
pub struct Subscription {
    secret: SecretKey,
    pub auth: [u8; 16],
}

impl Subscription {
    pub fn generate() -> Result<Subscription, Error> {
        let mut seed = [0u8; 32];
        random(&mut seed)?;
        let mut auth = [0u8; 16];
        random(&mut auth)?;
        Ok(Subscription {
            secret: SecretKey::from_slice(&seed).map_err(|_| Error::Random)?,
            auth,
        })
    }

    pub fn from_bytes(secret: &[u8], auth: [u8; 16]) -> Result<Subscription, Error> {
        Ok(Subscription {
            secret: SecretKey::from_slice(secret).map_err(|_| Error::Format("push key"))?,
            auth,
        })
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes().into()
    }

    /// Uncompressed public key ("p256dh").
    pub fn public(&self) -> [u8; 65] {
        point(&self.secret.public_key())
    }

    /// Decrypts a message body.
    pub fn decrypt(&self, body: &[u8]) -> Result<Vec<u8>, Error> {
        if body.len() < 21 + 65 + 17 || body[20] != 65 {
            return Err(Error::Format("push message header"));
        }
        let salt: [u8; 16] = body[..16].try_into().expect("16");
        let as_bytes: [u8; 65] = body[21..86].try_into().expect("65");
        let rs = u32::from_be_bytes(body[16..20].try_into().expect("4")) as usize;
        let ct = &body[86..];
        if rs < 18 || ct.len() > rs {
            return Err(Error::Format("push message size"));
        }
        let as_key = PublicKey::from_sec1_bytes(&as_bytes).map_err(|_| Error::Decrypt)?;
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), as_key.as_affine());
        let (cek, nonce) = keys(
            shared.raw_secret_bytes(),
            &self.auth,
            &self.public(),
            &as_bytes,
            &salt,
        );
        let mut plain = Aes128Gcm::new(&cek.into())
            .decrypt(Nonce::from_slice(&nonce), ct)
            .map_err(|_| Error::Decrypt)?;
        // Strip padding: zeros, then the delimiter 2 (single, last record).
        let end = plain.iter().rposition(|&b| b != 0).ok_or(Error::Decrypt)?;
        if plain[end] != 2 {
            return Err(Error::Decrypt);
        }
        plain.truncate(end);
        Ok(plain)
    }
}

pub fn base64url(data: &[u8]) -> String {
    crate::manifest::base64_encode(data)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// The server's VAPID key (RFC 8292).
pub struct Vapid(p256::ecdsa::SigningKey);

impl Vapid {
    pub fn generate() -> Result<Vapid, Error> {
        let mut seed = [0u8; 32];
        random(&mut seed)?;
        Vapid::from_bytes(&seed)
    }

    pub fn from_bytes(b: &[u8]) -> Result<Vapid, Error> {
        p256::ecdsa::SigningKey::from_slice(b)
            .map(Vapid)
            .map_err(|_| Error::Format("VAPID key"))
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// Public key, uncompressed (the `applicationServerKey`).
    pub fn public(&self) -> [u8; 65] {
        self.0
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .try_into()
            .expect("uncompressed P-256 point")
    }

    /// The `Authorization` header for a push to `endpoint` (origin
    /// `audience`, e.g. "https://ntfy.sh"), valid until `expires` (Unix
    /// seconds).
    pub fn authorization(&self, audience: &str, expires: u64, subject: &str) -> String {
        let mut claims = String::from("{\"aud\":");
        bl_json::write_string(&mut claims, audience);
        claims.push_str(&format!(",\"exp\":{expires},\"sub\":"));
        bl_json::write_string(&mut claims, subject);
        claims.push('}');
        let signing_input = format!(
            "{}.{}",
            base64url(br#"{"typ":"JWT","alg":"ES256"}"#),
            base64url(claims.as_bytes())
        );
        let sig: p256::ecdsa::Signature = self.0.sign(signing_input.as_bytes());
        format!(
            "vapid t={signing_input}.{}, k={}",
            base64url(&sig.to_bytes()),
            base64url(&self.public())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unb64url(s: &str) -> Vec<u8> {
        let std = s.replace('-', "+").replace('_', "/");
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in std.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            acc = acc << 6 | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    /// RFC 8291, appendix A.
    #[test]
    fn rfc8291_example() {
        let plaintext = b"When I grow up, I want to be a watermelon";
        let as_private = unb64url("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
        let ua_private = unb64url("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94");
        let ua_public = unb64url("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth: [u8; 16] = unb64url("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let salt: [u8; 16] = unb64url("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let expected = unb64url("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
        let as_secret = SecretKey::from_slice(&as_private).unwrap();
        let body = encrypt_with(&ua_public, &auth, plaintext, &as_secret, &salt).unwrap();
        assert_eq!(body, expected);
        let sub = Subscription::from_bytes(&ua_private, auth).unwrap();
        assert_eq!(sub.public().to_vec(), ua_public);
        assert_eq!(sub.decrypt(&expected).unwrap(), plaintext);
    }

    #[test]
    fn roundtrip_and_tamper() {
        let sub = Subscription::generate().unwrap();
        let msg = br#"{"type":"share","from":"alice"}"#;
        let body = encrypt(&sub.public(), &sub.auth, msg).unwrap();
        assert_eq!(sub.decrypt(&body).unwrap(), msg);
        let mut t = body.clone();
        let n = t.len();
        t[n - 1] ^= 1;
        assert!(sub.decrypt(&t).is_err());
        let other = Subscription::generate().unwrap();
        assert!(other.decrypt(&body).is_err());
        assert!(encrypt(&sub.public(), &sub.auth, &vec![0; MAX_PLAINTEXT + 1]).is_err());
    }

    #[test]
    fn vapid_jwt_verifies() {
        use p256::ecdsa::signature::Verifier;
        let v = Vapid::generate().unwrap();
        let h = v.authorization(
            "https://push.example.net",
            1_900_000_000,
            "mailto:admin@example.org",
        );
        let t = h
            .strip_prefix("vapid t=")
            .unwrap()
            .split(", k=")
            .next()
            .unwrap();
        let (input, sig) = t.rsplit_once('.').unwrap();
        let sig = p256::ecdsa::Signature::from_slice(&unb64url(sig)).unwrap();
        let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&v.public()).unwrap();
        key.verify(input.as_bytes(), &sig).unwrap();
        let claims = String::from_utf8(unb64url(input.split('.').nth(1).unwrap())).unwrap();
        assert!(claims.contains("\"aud\":\"https://push.example.net\""));
        let again = Vapid::from_bytes(&v.to_bytes()).unwrap();
        assert_eq!(again.public(), v.public());
    }
}
