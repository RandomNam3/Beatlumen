//! AirPlay 2 pairing on the receiver side (HomeKit-style, TLV8 messages):
//!
//! * pair-setup: SRP-6a with the user "Pair-Setup" and the PIN ("3939"
//!   for transient pairing, or the receiver's password). Transient pairing
//!   ends after M4 and the SRP session key encrypts the connection; full
//!   pairing exchanges long-term Ed25519 keys in M5/M6 (kept in memory).
//! * pair-verify: X25519 key exchange, authenticated with the long-term
//!   keys of both sides; the shared secret encrypts the connection.

use super::tlv8::{self, Tlv};
use crate::crypto::chacha::{label_nonce, open, seal};
use crate::crypto::curve25519::{x25519_base, x25519_shared};
use crate::crypto::ed25519::{self, SigningKey};
use crate::crypto::sha512::hkdf_key;
use crate::crypto::{random_bytes, srp};
use std::sync::Mutex;

/// Most remembered controllers (full pairings).
const MAX_PAIRINGS: usize = 16;

/// The receiver's long-term identity.
pub struct Identity {
    key: SigningKey,
    public: [u8; 32],
    /// Pairing identifier ("AA:BB:CC:DD:EE:FF").
    pub id: String,
}

impl Identity {
    pub fn new(id: String, seed: &[u8; 32]) -> Identity {
        let key = SigningKey::from_seed(seed);
        Identity {
            public: key.public_key(),
            key,
            id,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    pub fn public_key_ref(&self) -> &[u8; 32] {
        &self.public
    }
}

/// Controllers that completed a full pair-setup: identifier and long-term
/// public key.
#[derive(Default)]
pub struct Pairings {
    list: Mutex<Vec<(Vec<u8>, [u8; 32])>>,
}

impl Pairings {
    fn add(&self, id: &[u8], key: [u8; 32]) {
        let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        l.retain(|(i, _)| i != id);
        if l.len() >= MAX_PAIRINGS {
            l.remove(0);
        }
        l.push((id.to_vec(), key));
    }

    fn get(&self, id: &[u8]) -> Option<[u8; 32]> {
        let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        l.iter().find(|(i, _)| i == id).map(|(_, k)| *k)
    }
}

/// Result of handling one pairing message.
pub struct Reply {
    pub body: Vec<u8>,
    /// Pairing completed: the secret the connection is encrypted with
    /// from the next request on.
    pub secret: Option<Vec<u8>>,
    /// Authentication failed (wrong PIN or signature).
    pub failed: bool,
}

impl Reply {
    fn ok(body: Tlv) -> Reply {
        Reply {
            body: body.encode(),
            secret: None,
            failed: false,
        }
    }

    fn error(state: u8, code: u8) -> Reply {
        Reply {
            body: Tlv::new()
                .with(tlv8::STATE, &[state])
                .with(tlv8::ERROR, &[code])
                .encode(),
            secret: None,
            failed: code == tlv8::ERROR_AUTHENTICATION,
        }
    }
}

#[derive(Default)]
enum Setup {
    #[default]
    Idle,
    /// M2 sent.
    Challenged {
        srp: Box<srp::Server>,
        transient: bool,
    },
    /// M4 sent (full pairing): waiting for M5.
    Proven { key: Vec<u8> },
}

/// pair-setup of one connection.
#[derive(Default)]
pub struct PairSetup {
    state: Setup,
}

impl PairSetup {
    pub fn handle(
        &mut self,
        body: &[u8],
        pin: &str,
        identity: &Identity,
        pairings: &Pairings,
    ) -> Reply {
        let Some(req) = Tlv::decode(body) else {
            return Reply::error(2, tlv8::ERROR_UNAVAILABLE);
        };
        let state = std::mem::take(&mut self.state);
        match (req.byte(tlv8::STATE), state) {
            (Some(1), _) => self.m1(&req, pin),
            (Some(3), Setup::Challenged { srp, transient }) => self.m3(&req, &srp, transient),
            (Some(5), Setup::Proven { key }) => m5(&req, &key, identity, pairings),
            (s, _) => Reply::error(s.unwrap_or(0).wrapping_add(1), tlv8::ERROR_UNAVAILABLE),
        }
    }

    fn m1(&mut self, req: &Tlv, pin: &str) -> Reply {
        if req.byte(tlv8::METHOD).unwrap_or(0) != 0 {
            return Reply::error(2, tlv8::ERROR_UNAVAILABLE);
        }
        let transient = req.byte(tlv8::FLAGS) == Some(0x10);
        let mut salt = random_bytes(16);
        salt[0] |= 1; // no leading zero (the salt enters hashes stripped)
        let b = random_bytes(32);
        let server = srp::Server::new(
            &srp::GROUP_3072,
            srp::sha512,
            "Pair-Setup",
            pin.as_bytes(),
            &salt,
            &b,
        );
        let reply = Tlv::new()
            .with(tlv8::STATE, &[2])
            .with(tlv8::SALT, server.salt())
            .with(tlv8::PUBLIC_KEY, server.public_key());
        self.state = Setup::Challenged {
            srp: Box::new(server),
            transient,
        };
        Reply::ok(reply)
    }

    fn m3(&mut self, req: &Tlv, server: &srp::Server, transient: bool) -> Reply {
        let (Some(a), Some(proof)) = (req.get(tlv8::PUBLIC_KEY), req.get(tlv8::PROOF)) else {
            return Reply::error(4, tlv8::ERROR_UNAVAILABLE);
        };
        let Some(session) = server.verify(a, proof) else {
            return Reply::error(4, tlv8::ERROR_AUTHENTICATION);
        };
        let mut reply = Reply::ok(
            Tlv::new()
                .with(tlv8::STATE, &[4])
                .with(tlv8::PROOF, &session.proof),
        );
        if transient {
            reply.secret = Some(session.key);
        } else {
            self.state = Setup::Proven { key: session.key };
        }
        reply
    }
}

/// M5: the controller's long-term key, signed; answered with ours (M6).
fn m5(req: &Tlv, key: &[u8], identity: &Identity, pairings: &Pairings) -> Reply {
    let enc_key = hkdf_key(key, "Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info");
    let Some(inner) = req
        .get(tlv8::ENCRYPTED_DATA)
        .and_then(|d| open(&enc_key, &label_nonce(b"PS-Msg05"), &[], d))
        .and_then(|p| Tlv::decode(&p))
    else {
        return Reply::error(6, tlv8::ERROR_AUTHENTICATION);
    };
    let (Some(id), Some(ltpk), Some(sig)) = (
        inner.get(tlv8::IDENTIFIER),
        inner.get(tlv8::PUBLIC_KEY),
        inner.get(tlv8::SIGNATURE),
    ) else {
        return Reply::error(6, tlv8::ERROR_AUTHENTICATION);
    };
    let Ok(ltpk) = <[u8; 32]>::try_from(ltpk) else {
        return Reply::error(6, tlv8::ERROR_AUTHENTICATION);
    };
    let x = hkdf_key(
        key,
        "Pair-Setup-Controller-Sign-Salt",
        "Pair-Setup-Controller-Sign-Info",
    );
    if id.len() > 64 || !ed25519::verify(&ltpk, &[&x[..], id, &ltpk].concat(), sig) {
        return Reply::error(6, tlv8::ERROR_AUTHENTICATION);
    }
    pairings.add(id, ltpk);
    let x = hkdf_key(
        key,
        "Pair-Setup-Accessory-Sign-Salt",
        "Pair-Setup-Accessory-Sign-Info",
    );
    let pk = identity.public_key();
    let sig = identity
        .key
        .sign(&[&x[..], identity.id.as_bytes(), &pk].concat());
    let inner = Tlv::new()
        .with(tlv8::IDENTIFIER, identity.id.as_bytes())
        .with(tlv8::SIGNATURE, &sig)
        .with(tlv8::PUBLIC_KEY, &pk);
    let sealed = seal(&enc_key, &label_nonce(b"PS-Msg06"), &[], &inner.encode());
    Reply::ok(
        Tlv::new()
            .with(tlv8::STATE, &[6])
            .with(tlv8::ENCRYPTED_DATA, &sealed),
    )
}

/// pair-verify of one connection.
#[derive(Default)]
pub struct PairVerify {
    /// After M2: (our ephemeral public key, peer's, shared secret).
    pending: Option<([u8; 32], [u8; 32], [u8; 32])>,
}

impl PairVerify {
    pub fn handle(&mut self, body: &[u8], identity: &Identity, pairings: &Pairings) -> Reply {
        let Some(req) = Tlv::decode(body) else {
            return Reply::error(2, tlv8::ERROR_UNAVAILABLE);
        };
        match (req.byte(tlv8::STATE), self.pending.take()) {
            (Some(1), _) => self.m1(&req, identity),
            (Some(3), Some(p)) => m3(&req, p, pairings),
            (s, _) => Reply::error(s.unwrap_or(0).wrapping_add(1), tlv8::ERROR_UNAVAILABLE),
        }
    }

    fn m1(&mut self, req: &Tlv, identity: &Identity) -> Reply {
        let Some(peer) = req
            .get(tlv8::PUBLIC_KEY)
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
        else {
            return Reply::error(2, tlv8::ERROR_UNAVAILABLE);
        };
        let secret: [u8; 32] = random_bytes(32).try_into().expect("32 bytes");
        let ours = x25519_base(&secret);
        let Some(shared) = x25519_shared(&secret, &peer) else {
            return Reply::error(2, tlv8::ERROR_AUTHENTICATION);
        };
        let sig = identity
            .key
            .sign(&[&ours[..], identity.id.as_bytes(), &peer].concat());
        let inner = Tlv::new()
            .with(tlv8::IDENTIFIER, identity.id.as_bytes())
            .with(tlv8::SIGNATURE, &sig);
        let key = hkdf_key(
            &shared,
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        );
        let sealed = seal(&key, &label_nonce(b"PV-Msg02"), &[], &inner.encode());
        self.pending = Some((ours, peer, shared));
        Reply::ok(
            Tlv::new()
                .with(tlv8::STATE, &[2])
                .with(tlv8::PUBLIC_KEY, &ours)
                .with(tlv8::ENCRYPTED_DATA, &sealed),
        )
    }
}

/// M3: the controller proves its identity; only controllers that paired
/// with us before are accepted.
fn m3(
    req: &Tlv,
    (ours, peer, shared): ([u8; 32], [u8; 32], [u8; 32]),
    pairings: &Pairings,
) -> Reply {
    let key = hkdf_key(
        &shared,
        "Pair-Verify-Encrypt-Salt",
        "Pair-Verify-Encrypt-Info",
    );
    let Some(inner) = req
        .get(tlv8::ENCRYPTED_DATA)
        .and_then(|d| open(&key, &label_nonce(b"PV-Msg03"), &[], d))
        .and_then(|p| Tlv::decode(&p))
    else {
        return Reply::error(4, tlv8::ERROR_AUTHENTICATION);
    };
    let (Some(id), Some(sig)) = (inner.get(tlv8::IDENTIFIER), inner.get(tlv8::SIGNATURE)) else {
        return Reply::error(4, tlv8::ERROR_AUTHENTICATION);
    };
    let Some(ltpk) = pairings.get(id) else {
        return Reply::error(4, tlv8::ERROR_AUTHENTICATION);
    };
    if !ed25519::verify(&ltpk, &[&peer[..], id, &ours].concat(), sig) {
        return Reply::error(4, tlv8::ERROR_AUTHENTICATION);
    }
    Reply {
        body: Tlv::new().with(tlv8::STATE, &[4]).encode(),
        secret: Some(shared.to_vec()),
        failed: false,
    }
}

/// The sender side, for the tests.
#[cfg(test)]
pub mod client {
    use super::*;

    /// Transient pair-setup with PIN 3939 against `exchange` (which posts a
    /// body to /pair-setup and returns the response); the shared secret.
    pub fn transient_setup(
        exchange: &mut dyn FnMut(&[u8]) -> Vec<u8>,
        pin: &str,
    ) -> Option<Vec<u8>> {
        let m2 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[1])
                .with(tlv8::METHOD, &[0])
                .with(tlv8::FLAGS, &[0x10])
                .encode(),
        ))?;
        let c = srp::Client::new(
            &srp::GROUP_3072,
            srp::sha512,
            "Pair-Setup",
            pin.as_bytes(),
            m2.get(tlv8::SALT)?,
            m2.get(tlv8::PUBLIC_KEY)?,
            &random_bytes(32),
        )?;
        let m4 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[3])
                .with(tlv8::PUBLIC_KEY, &c.a_pub)
                .with(tlv8::PROOF, &c.proof)
                .encode(),
        ))?;
        (m4.get(tlv8::PROOF)? == c.server_proof.as_slice()).then_some(c.key)
    }

    /// Full pair-setup (M1-M6) with the controller key `ltsk`; returns
    /// the accessory's long-term public key.
    pub fn full_setup(
        exchange: &mut dyn FnMut(&[u8]) -> Vec<u8>,
        pin: &str,
        id: &[u8],
        ltsk: &SigningKey,
    ) -> Option<[u8; 32]> {
        let m2 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[1])
                .with(tlv8::METHOD, &[0])
                .encode(),
        ))?;
        let c = srp::Client::new(
            &srp::GROUP_3072,
            srp::sha512,
            "Pair-Setup",
            pin.as_bytes(),
            m2.get(tlv8::SALT)?,
            m2.get(tlv8::PUBLIC_KEY)?,
            &random_bytes(32),
        )?;
        let m4 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[3])
                .with(tlv8::PUBLIC_KEY, &c.a_pub)
                .with(tlv8::PROOF, &c.proof)
                .encode(),
        ))?;
        if m4.get(tlv8::PROOF)? != c.server_proof.as_slice() {
            return None;
        }
        let x = hkdf_key(
            &c.key,
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
        );
        let pk = ltsk.public_key();
        let sig = ltsk.sign(&[&x[..], id, &pk].concat());
        let inner = Tlv::new()
            .with(tlv8::IDENTIFIER, id)
            .with(tlv8::PUBLIC_KEY, &pk)
            .with(tlv8::SIGNATURE, &sig);
        let enc = hkdf_key(&c.key, "Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info");
        let sealed = seal(&enc, &label_nonce(b"PS-Msg05"), &[], &inner.encode());
        let m6 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[5])
                .with(tlv8::ENCRYPTED_DATA, &sealed)
                .encode(),
        ))?;
        let inner = Tlv::decode(&open(
            &enc,
            &label_nonce(b"PS-Msg06"),
            &[],
            m6.get(tlv8::ENCRYPTED_DATA)?,
        )?)?;
        let acc_id = inner.get(tlv8::IDENTIFIER)?;
        let acc_pk: [u8; 32] = inner.get(tlv8::PUBLIC_KEY)?.try_into().ok()?;
        let x = hkdf_key(
            &c.key,
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
        );
        ed25519::verify(
            &acc_pk,
            &[&x[..], acc_id, &acc_pk].concat(),
            inner.get(tlv8::SIGNATURE)?,
        )
        .then_some(acc_pk)
    }

    /// pair-verify; returns the shared secret.
    pub fn verify(
        exchange: &mut dyn FnMut(&[u8]) -> Vec<u8>,
        id: &[u8],
        ltsk: &SigningKey,
        accessory_pk: &[u8; 32],
    ) -> Option<Vec<u8>> {
        let secret: [u8; 32] = random_bytes(32).try_into().ok()?;
        let ours = x25519_base(&secret);
        let m2 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[1])
                .with(tlv8::PUBLIC_KEY, &ours)
                .encode(),
        ))?;
        let theirs: [u8; 32] = m2.get(tlv8::PUBLIC_KEY)?.try_into().ok()?;
        let shared = x25519_shared(&secret, &theirs)?;
        let key = hkdf_key(
            &shared,
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        );
        let inner = Tlv::decode(&open(
            &key,
            &label_nonce(b"PV-Msg02"),
            &[],
            m2.get(tlv8::ENCRYPTED_DATA)?,
        )?)?;
        let acc_id = inner.get(tlv8::IDENTIFIER)?;
        if !ed25519::verify(
            accessory_pk,
            &[&theirs[..], acc_id, &ours].concat(),
            inner.get(tlv8::SIGNATURE)?,
        ) {
            return None;
        }
        let sig = ltsk.sign(&[&ours[..], id, &theirs].concat());
        let inner = Tlv::new()
            .with(tlv8::IDENTIFIER, id)
            .with(tlv8::SIGNATURE, &sig);
        let sealed = seal(&key, &label_nonce(b"PV-Msg03"), &[], &inner.encode());
        let m4 = Tlv::decode(&exchange(
            &Tlv::new()
                .with(tlv8::STATE, &[3])
                .with(tlv8::ENCRYPTED_DATA, &sealed)
                .encode(),
        ))?;
        (m4.byte(tlv8::STATE) == Some(4) && m4.get(tlv8::ERROR).is_none()).then(|| shared.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity::new("02:11:22:33:44:55".into(), &[9; 32])
    }

    #[test]
    fn transient_pairing_agrees_on_the_secret() {
        let id = identity();
        let pairings = Pairings::default();
        let mut setup = PairSetup::default();
        let mut secret = None;
        let key = client::transient_setup(
            &mut |body| {
                let r = setup.handle(body, "3939", &id, &pairings);
                if r.secret.is_some() {
                    secret = r.secret;
                }
                r.body
            },
            "3939",
        )
        .unwrap();
        assert_eq!(Some(key), secret);
    }

    #[test]
    fn wrong_pin_fails() {
        let id = identity();
        let pairings = Pairings::default();
        let mut setup = PairSetup::default();
        let mut failed = false;
        let key = client::transient_setup(
            &mut |body| {
                let r = setup.handle(body, "secret", &id, &pairings);
                failed |= r.failed;
                assert!(r.secret.is_none());
                r.body
            },
            "3939",
        );
        assert!(key.is_none() && failed);
    }

    #[test]
    fn full_pairing_then_verify() {
        let id = identity();
        let pairings = Pairings::default();
        let ltsk = SigningKey::from_seed(&[5; 32]);
        let mut setup = PairSetup::default();
        let acc = client::full_setup(
            &mut |b| setup.handle(b, "1234", &id, &pairings).body,
            "1234",
            b"controller-1",
            &ltsk,
        )
        .unwrap();
        assert_eq!(acc, id.public_key());
        let mut verify = PairVerify::default();
        let mut server_secret = None;
        let secret = client::verify(
            &mut |b| {
                let r = verify.handle(b, &id, &pairings);
                if r.secret.is_some() {
                    server_secret = r.secret;
                }
                r.body
            },
            b"controller-1",
            &ltsk,
            &acc,
        )
        .unwrap();
        assert_eq!(Some(secret), server_secret);

        // An unknown controller is refused.
        let other = SigningKey::from_seed(&[6; 32]);
        let mut verify = PairVerify::default();
        let mut got = None;
        let r = client::verify(
            &mut |b| {
                let r = verify.handle(b, &id, &pairings);
                got = got.take().or(r.secret);
                r.body
            },
            b"controller-2",
            &other,
            &acc,
        );
        assert!(r.is_none() && got.is_none());
    }

    #[test]
    fn out_of_order_messages_are_refused() {
        let id = identity();
        let pairings = Pairings::default();
        let mut setup = PairSetup::default();
        let m3 = Tlv::new()
            .with(tlv8::STATE, &[3])
            .with(tlv8::PUBLIC_KEY, &[1; 384])
            .with(tlv8::PROOF, &[0; 64])
            .encode();
        let r = setup.handle(&m3, "3939", &id, &pairings);
        let t = Tlv::decode(&r.body).unwrap();
        assert_eq!(t.byte(tlv8::STATE), Some(4));
        assert!(t.get(tlv8::ERROR).is_some());
        assert!(r.secret.is_none());
        let r = setup.handle(b"\x06", "3939", &id, &pairings);
        assert!(Tlv::decode(&r.body).unwrap().get(tlv8::ERROR).is_some());
    }
}
