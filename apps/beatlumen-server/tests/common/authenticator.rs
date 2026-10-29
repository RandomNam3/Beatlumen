//! A software WebAuthn authenticator for tests (P-256, "none"
//! attestation, rp id "localhost", PRF simulated by a fixed secret).

use p256::ecdsa::{signature::Signer, Signature, SigningKey};

/// Minimal CBOR encoder for the test authenticator.
pub fn cbor_head(major: u8, n: u64, out: &mut Vec<u8>) {
    match n {
        0..=23 => out.push(major << 5 | n as u8),
        24..=0xff => out.extend([major << 5 | 24, n as u8]),
        _ => {
            out.push(major << 5 | 25);
            out.extend((n as u16).to_be_bytes());
        }
    }
}
pub fn cbor_int(i: i64, out: &mut Vec<u8>) {
    if i >= 0 {
        cbor_head(0, i as u64, out)
    } else {
        cbor_head(1, (-1 - i) as u64, out)
    }
}
pub fn cbor_bytes(b: &[u8], out: &mut Vec<u8>) {
    cbor_head(2, b.len() as u64, out);
    out.extend_from_slice(b);
}
pub fn cbor_text(t: &str, out: &mut Vec<u8>) {
    cbor_head(3, t.len() as u64, out);
    out.extend_from_slice(t.as_bytes());
}

pub struct Authenticator {
    pub key: SigningKey,
    pub id: Vec<u8>,
    pub prf: [u8; 32],
    pub count: u32,
}

impl Authenticator {
    pub fn new() -> Authenticator {
        let mut seed = [0u8; 32];
        bl_vault::random(&mut seed).unwrap();
        let mut id = vec![0u8; 16];
        bl_vault::random(&mut id).unwrap();
        let mut prf = [0u8; 32];
        bl_vault::random(&mut prf).unwrap();
        Authenticator {
            key: SigningKey::from_slice(&seed).unwrap(),
            id,
            prf,
            count: 0,
        }
    }

    pub fn rp_hash() -> [u8; 32] {
        bl_vault::sha256(&[b"localhost"])
    }

    pub fn create(&self, challenge: &str, origin: &str) -> (Vec<u8>, Vec<u8>) {
        let client = format!(
            r#"{{"type":"webauthn.create","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        );
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut cose = Vec::new();
        cbor_head(5, 5, &mut cose);
        cbor_int(1, &mut cose);
        cbor_int(2, &mut cose);
        cbor_int(3, &mut cose);
        cbor_int(-7, &mut cose);
        cbor_int(-1, &mut cose);
        cbor_int(1, &mut cose);
        cbor_int(-2, &mut cose);
        cbor_bytes(point.x().unwrap(), &mut cose);
        cbor_int(-3, &mut cose);
        cbor_bytes(point.y().unwrap(), &mut cose);
        let mut auth = Self::rp_hash().to_vec();
        auth.push(0x45); // UP | UV | AT
        auth.extend(0u32.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.id.len() as u16).to_be_bytes());
        auth.extend(&self.id);
        auth.extend(cose);
        let mut att = Vec::new();
        cbor_head(5, 3, &mut att);
        cbor_text("fmt", &mut att);
        cbor_text("none", &mut att);
        cbor_text("attStmt", &mut att);
        cbor_head(5, 0, &mut att);
        cbor_text("authData", &mut att);
        cbor_bytes(&auth, &mut att);
        (client.into_bytes(), att)
    }

    pub fn get(&mut self, challenge: &str, origin: &str, flags: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        self.count += 1;
        let client =
            format!(r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"{origin}"}}"#);
        let mut auth = Self::rp_hash().to_vec();
        auth.push(flags);
        auth.extend(self.count.to_be_bytes());
        let mut msg = auth.clone();
        msg.extend(bl_vault::sha256(&[client.as_bytes()]));
        let sig: Signature = self.key.sign(&msg);
        (client.into_bytes(), auth, sig.to_der().as_bytes().to_vec())
    }
}
