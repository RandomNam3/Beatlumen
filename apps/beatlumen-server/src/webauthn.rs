//! WebAuthn (passkeys) on the server: checks registrations and sign-in
//! signatures (W3C Web Authentication Level 3). Only what passkeys need:
//! "none" attestation, user verification required, ES256, EdDSA and RS256
//! keys, signature counters. Signatures are checked with `ring`.

use crate::sha256::sha256;
use bl_net::base64_decode;

/// A decoded CBOR item (RFC 8949), as far as WebAuthn uses CBOR.
#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
}

impl Cbor {
    pub fn get(&self, key: &Cbor) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn int_key(&self, k: i64) -> Option<&Cbor> {
        self.get(&Cbor::Int(k))
    }
    fn text_key(&self, k: &str) -> Option<&Cbor> {
        self.get(&Cbor::Text(k.to_string()))
    }
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
    fn int(&self) -> Option<i64> {
        match self {
            Cbor::Int(i) => Some(*i),
            _ => None,
        }
    }
}

/// Decodes one CBOR item from the start of `data`; returns it and the
/// number of bytes used.
pub fn cbor_decode(data: &[u8]) -> Result<(Cbor, usize), &'static str> {
    fn item(d: &[u8], pos: &mut usize, depth: usize) -> Result<Cbor, &'static str> {
        if depth > 16 {
            return Err("CBOR nested too deeply");
        }
        let b = *d.get(*pos).ok_or("CBOR truncated")?;
        *pos += 1;
        let major = b >> 5;
        let info = b & 31;
        let arg = |pos: &mut usize| -> Result<u64, &'static str> {
            let n = match info {
                0..=23 => return Ok(info as u64),
                24 => 1,
                25 => 2,
                26 => 4,
                27 => 8,
                _ => return Err("CBOR: indefinite lengths are not supported"),
            };
            let s = d.get(*pos..*pos + n).ok_or("CBOR truncated")?;
            *pos += n;
            Ok(s.iter().fold(0u64, |a, x| a << 8 | *x as u64))
        };
        Ok(match major {
            0 => Cbor::Int(i64::try_from(arg(pos)?).map_err(|_| "CBOR integer too large")?),
            1 => Cbor::Int(-1 - i64::try_from(arg(pos)?).map_err(|_| "CBOR integer too large")?),
            2 | 3 => {
                let n = arg(pos)? as usize;
                let s = d
                    .get(*pos..pos.checked_add(n).ok_or("CBOR length")?)
                    .ok_or("CBOR truncated")?;
                *pos += n;
                if major == 2 {
                    Cbor::Bytes(s.to_vec())
                } else {
                    Cbor::Text(String::from_utf8(s.to_vec()).map_err(|_| "CBOR text is not UTF-8")?)
                }
            }
            4 => {
                let n = arg(pos)? as usize;
                if n > 1024 {
                    return Err("CBOR array too long");
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(item(d, pos, depth + 1)?);
                }
                Cbor::Array(v)
            }
            5 => {
                let n = arg(pos)? as usize;
                if n > 1024 {
                    return Err("CBOR map too long");
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = item(d, pos, depth + 1)?;
                    v.push((k, item(d, pos, depth + 1)?));
                }
                Cbor::Map(v)
            }
            7 => match info {
                20 => Cbor::Bool(false),
                21 => Cbor::Bool(true),
                22 | 23 => Cbor::Null,
                _ => return Err("CBOR: unsupported simple value"),
            },
            _ => return Err("CBOR: unsupported type"),
        })
    }
    let mut pos = 0;
    let v = item(data, &mut pos, 0)?;
    Ok((v, pos))
}

/// A credential public key (from its COSE encoding).
#[derive(Debug, Clone, PartialEq)]
pub enum PublicKey {
    /// Uncompressed P-256 point.
    Es256(Vec<u8>),
    Ed25519(Vec<u8>),
    Rs256 {
        n: Vec<u8>,
        e: Vec<u8>,
    },
}

impl PublicKey {
    pub fn from_cose(c: &Cbor) -> Result<PublicKey, &'static str> {
        let kty = c.int_key(1).and_then(Cbor::int).ok_or("COSE key type")?;
        let alg = c.int_key(3).and_then(Cbor::int).ok_or("COSE algorithm")?;
        match (kty, alg) {
            (2, -7) => {
                if c.int_key(-1).and_then(Cbor::int) != Some(1) {
                    return Err("COSE: only P-256 is supported");
                }
                let x = c
                    .int_key(-2)
                    .and_then(Cbor::bytes)
                    .filter(|x| x.len() == 32)
                    .ok_or("COSE x")?;
                let y = c
                    .int_key(-3)
                    .and_then(Cbor::bytes)
                    .filter(|y| y.len() == 32)
                    .ok_or("COSE y")?;
                let mut p = vec![4];
                p.extend_from_slice(x);
                p.extend_from_slice(y);
                Ok(PublicKey::Es256(p))
            }
            (1, -8) => {
                if c.int_key(-1).and_then(Cbor::int) != Some(6) {
                    return Err("COSE: only Ed25519 is supported");
                }
                let x = c
                    .int_key(-2)
                    .and_then(Cbor::bytes)
                    .filter(|x| x.len() == 32)
                    .ok_or("COSE x")?;
                Ok(PublicKey::Ed25519(x.to_vec()))
            }
            (3, -257) => {
                let n = c
                    .int_key(-1)
                    .and_then(Cbor::bytes)
                    .filter(|n| n.len() >= 256)
                    .ok_or("COSE n")?;
                let e = c.int_key(-2).and_then(Cbor::bytes).ok_or("COSE e")?;
                Ok(PublicKey::Rs256 {
                    n: n.to_vec(),
                    e: e.to_vec(),
                })
            }
            _ => Err("unsupported key algorithm"),
        }
    }

    /// Serialized form for storage: "alg:base64...".
    pub fn to_storage(&self) -> String {
        match self {
            PublicKey::Es256(p) => format!("es256:{}", bl_net::base64_encode(p)),
            PublicKey::Ed25519(p) => format!("ed25519:{}", bl_net::base64_encode(p)),
            PublicKey::Rs256 { n, e } => format!(
                "rs256:{}:{}",
                bl_net::base64_encode(n),
                bl_net::base64_encode(e)
            ),
        }
    }

    pub fn from_storage(s: &str) -> Option<PublicKey> {
        let mut it = s.split(':');
        let kind = it.next()?;
        let a = base64_decode(it.next()?)?;
        match kind {
            "es256" if a.len() == 65 => Some(PublicKey::Es256(a)),
            "ed25519" if a.len() == 32 => Some(PublicKey::Ed25519(a)),
            "rs256" => Some(PublicKey::Rs256 {
                n: a,
                e: base64_decode(it.next()?)?,
            }),
            _ => None,
        }
    }

    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        use ring::signature;
        match self {
            PublicKey::Es256(p) => {
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, p)
                    .verify(msg, sig)
                    .is_ok()
            }
            PublicKey::Ed25519(p) => signature::UnparsedPublicKey::new(&signature::ED25519, p)
                .verify(msg, sig)
                .is_ok(),
            PublicKey::Rs256 { n, e } => signature::RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                .is_ok(),
        }
    }
}

/// Parsed authenticator data.
#[derive(Debug)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    /// Credential id and public key (registration).
    pub credential: Option<(Vec<u8>, PublicKey)>,
}

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;

impl AuthData {
    pub fn parse(d: &[u8]) -> Result<AuthData, &'static str> {
        if d.len() < 37 {
            return Err("authenticator data too short");
        }
        let flags = d[32];
        let mut a = AuthData {
            rp_id_hash: d[..32].try_into().expect("32"),
            flags,
            sign_count: u32::from_be_bytes(d[33..37].try_into().expect("4")),
            credential: None,
        };
        if flags & FLAG_AT != 0 {
            let rest = &d[37..];
            if rest.len() < 18 {
                return Err("attested credential data too short");
            }
            let len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
            if len == 0 || len > 1023 {
                return Err("credential id length");
            }
            let id = rest
                .get(18..18 + len)
                .ok_or("credential id truncated")?
                .to_vec();
            let (cose, _) = cbor_decode(&rest[18 + len..])?;
            a.credential = Some((id, PublicKey::from_cose(&cose)?));
        }
        Ok(a)
    }
}

/// The relying party: the server's public address and allowed origins.
#[derive(Debug, Clone)]
pub struct RelyingParty {
    pub id: String,
    pub name: String,
    pub origins: Vec<String>,
}

impl RelyingParty {
    /// From the public URL (`https://music.example.org`) and Android
    /// signing certificate fingerprints (SHA-256, hex).
    pub fn new(
        public_url: &str,
        name: &str,
        android_certs: &[String],
    ) -> Result<RelyingParty, String> {
        let u = bl_net::client::Url::parse(public_url).map_err(|e| format!("--public-url: {e}"))?;
        let local = u.host == "localhost";
        if u.scheme != "https" && !local {
            return Err("--public-url must be an https:// address (passkeys need HTTPS)".into());
        }
        let host = u.host.to_ascii_lowercase();
        let default_port = if u.scheme == "https" { 443 } else { 80 };
        let origin = if u.port == default_port {
            format!("{}://{host}", u.scheme)
        } else {
            format!("{}://{host}:{}", u.scheme, u.port)
        };
        let mut origins = vec![origin];
        for c in android_certs {
            let bytes: Option<Vec<u8>> = (0..c.len() / 2)
                .map(|i| u8::from_str_radix(c.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect();
            match bytes.filter(|b| b.len() == 32 && c.len() == 64) {
                Some(b) => origins.push(format!(
                    "android:apk-key-hash:{}",
                    bl_vault::webpush::base64url(&b)
                )),
                None => {
                    return Err(format!(
                        "Android certificate fingerprint {c:?}: expected 64 hex digits"
                    ))
                }
            }
        }
        Ok(RelyingParty {
            id: host,
            name: name.to_string(),
            origins,
        })
    }

    /// Checks clientDataJSON: type, challenge and origin.
    pub fn check_client_data(
        &self,
        client_data: &[u8],
        kind: &str,
        challenge: &[u8],
    ) -> Result<(), &'static str> {
        let v = bl_json::parse_bytes(client_data).map_err(|_| "clientDataJSON is not JSON")?;
        if v.str_or("type", "") != kind {
            return Err("wrong ceremony type");
        }
        let got = base64_decode(v.str_or("challenge", "")).ok_or("challenge")?;
        if !crate::util::ct_eq(&got, challenge) {
            return Err("challenge does not match");
        }
        if !self.origins.iter().any(|o| o == v.str_or("origin", "")) {
            return Err("origin not allowed");
        }
        if v.bool_or("crossOrigin", false) {
            return Err("cross-origin request");
        }
        Ok(())
    }

    fn check_auth_data(&self, a: &AuthData) -> Result<(), &'static str> {
        if !crate::util::ct_eq(&a.rp_id_hash, &sha256(self.id.as_bytes())) {
            return Err("relying party does not match");
        }
        if a.flags & FLAG_UP == 0 || a.flags & FLAG_UV == 0 {
            return Err("user presence and verification are required");
        }
        Ok(())
    }

    /// Checks a registration; returns the credential id, key and counter.
    pub fn register(
        &self,
        client_data: &[u8],
        attestation_object: &[u8],
        challenge: &[u8],
    ) -> Result<(Vec<u8>, PublicKey, u32), &'static str> {
        self.check_client_data(client_data, "webauthn.create", challenge)?;
        let (att, _) = cbor_decode(attestation_object)?;
        let auth = att
            .text_key("authData")
            .and_then(Cbor::bytes)
            .ok_or("authData")?;
        let a = AuthData::parse(auth)?;
        self.check_auth_data(&a)?;
        let (id, key) = a.credential.ok_or("no credential in the registration")?;
        Ok((id, key, a.sign_count))
    }

    /// Checks a sign-in signature with the stored key; returns the new
    /// signature counter.
    pub fn authenticate(
        &self,
        key: &PublicKey,
        stored_count: u32,
        client_data: &[u8],
        authenticator_data: &[u8],
        signature: &[u8],
        challenge: &[u8],
    ) -> Result<u32, &'static str> {
        self.check_client_data(client_data, "webauthn.get", challenge)?;
        let a = AuthData::parse(authenticator_data)?;
        self.check_auth_data(&a)?;
        let mut msg = authenticator_data.to_vec();
        msg.extend_from_slice(&sha256(client_data));
        if !key.verify(&msg, signature) {
            return Err("signature does not verify");
        }
        // A counter that does not grow points to a cloned authenticator
        // (passkeys that sync report 0).
        if (a.sign_count != 0 || stored_count != 0) && a.sign_count <= stored_count {
            return Err("signature counter did not increase");
        }
        Ok(a.sign_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbor() {
        // {1: 2, 3: -7, -1: 1, -2: h'0102', "a": [true, null]}
        let d = [
            0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x42, 1, 2, 0x61, b'a', 0x82, 0xf5,
            0xf6,
        ];
        let (v, n) = cbor_decode(&d).unwrap();
        assert_eq!(n, d.len());
        assert_eq!(v.int_key(3), Some(&Cbor::Int(-7)));
        assert_eq!(v.int_key(-2), Some(&Cbor::Bytes(vec![1, 2])));
        assert_eq!(
            v.text_key("a"),
            Some(&Cbor::Array(vec![Cbor::Bool(true), Cbor::Null]))
        );
        assert!(cbor_decode(&[0x5f]).is_err());
        assert!(cbor_decode(&[0x42, 1]).is_err());
        let deep = vec![0x81; 40];
        assert!(cbor_decode(&deep).is_err());
    }

    #[test]
    fn relying_party() {
        let rp = RelyingParty::new("https://Music.Example.org", "B", &["00".repeat(32)]).unwrap();
        assert_eq!(rp.id, "music.example.org");
        assert_eq!(rp.origins[0], "https://music.example.org");
        assert!(rp.origins[1].starts_with("android:apk-key-hash:"));
        assert!(RelyingParty::new("http://music.example.org", "B", &[]).is_err());
        assert!(RelyingParty::new("http://localhost:7878", "B", &[]).is_ok());
    }
}
