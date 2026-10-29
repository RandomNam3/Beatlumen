//! Account keys: the password and the recovery code each derive a key that
//! wraps the random master key, and an authentication key whose hash the
//! server checks. The master key wraps the identity.
//!
//! ```text
//! password ──Argon2id(salt)──► root ─┬─ "auth" ──► auth key (sent at login)
//!                                    └─ "kek"  ──► wraps the master key
//! recovery code ─┬─ "recovery kek"  ──► wraps the master key
//!                └─ "recovery auth" ──► sent when recovering
//! master key ──► wraps the identity (Ed25519 + X25519)
//! ```

use crate::identity::{Identity, PublicIdentity};
use crate::recovery::RecoveryCode;
use crate::{aead, derive, random, sha256, Error, Secret};
use zeroize::Zeroize;

pub const SALT_LEN: usize = 16;
/// Size of a wrapped master key.
pub const WRAPPED_MASTER_LEN: usize = 32 + aead::OVERHEAD;
/// Size of wrapped identity private keys.
pub const WRAPPED_IDENTITY_LEN: usize = 64 + aead::OVERHEAD;

/// The canonical form of a user name (lower case `a-z 0-9 . _ -`, 1 to 32
/// characters, starting with a letter or digit). Names are part of the
/// associated data of wrapped keys, so client and server must agree on it.
pub fn normalize_user(name: &str) -> Result<String, Error> {
    let n = name.trim().to_ascii_lowercase();
    let ok = (1..=32).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && n.as_bytes()[0].is_ascii_alphanumeric();
    if ok {
        Ok(n)
    } else {
        Err(Error::Format(
            "user name: 1 to 32 characters a-z, 0-9, '.', '_', '-'",
        ))
    }
}

/// Argon2id parameters, chosen by the client at registration and stored on
/// the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_kib: u32,
    /// Passes.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes, 1 lane.
    pub const DEFAULT: KdfParams = KdfParams {
        m_kib: 64 * 1024,
        t: 3,
        p: 1,
    };

    /// Rejects parameters below the default (a malicious server could hand
    /// out weak ones to make the password easy to guess) and absurd ones (a
    /// server could exhaust the device's memory).
    pub fn check(&self) -> Result<(), Error> {
        let d = KdfParams::DEFAULT;
        if self.m_kib < d.m_kib || self.t < d.t || self.p < d.p {
            return Err(Error::WeakParameters);
        }
        if self.m_kib > 1024 * 1024 || self.t > 16 || self.p > 8 {
            return Err(Error::Format("key derivation parameters too large"));
        }
        Ok(())
    }
}

/// Argon2id of a password (checks the parameters first).
pub(crate) fn argon2id(
    password: &str,
    salt: &[u8; SALT_LEN],
    params: KdfParams,
) -> Result<[u8; 32], Error> {
    params.check()?;
    let argon = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(params.m_kib, params.t, params.p, Some(32))
            .map_err(|_| Error::Format("key derivation parameters"))?,
    );
    let mut root = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut root)
        .map_err(|_| Error::Format("key derivation"))?;
    Ok(root)
}

/// What the password derives.
pub struct PasswordKeys {
    /// Sent to the server at login (the server stores only a hash of it).
    pub auth: Secret,
    kek: Secret,
}

impl PasswordKeys {
    /// Runs Argon2id (slow by design: about a second on a phone).
    pub fn derive(
        password: &str,
        salt: &[u8; SALT_LEN],
        params: KdfParams,
    ) -> Result<PasswordKeys, Error> {
        let mut root = argon2id(password, salt, params)?;
        let keys = PasswordKeys {
            auth: derive(&root, "beatlumen auth v1"),
            kek: derive(&root, "beatlumen kek v1"),
        };
        root.zeroize();
        Ok(keys)
    }
}

/// What the recovery code derives.
pub struct RecoveryKeys {
    /// Sent to the server when recovering.
    pub auth: Secret,
    kek: Secret,
}

impl RecoveryKeys {
    pub fn derive(code: &RecoveryCode) -> RecoveryKeys {
        RecoveryKeys {
            auth: derive(code.secret().as_bytes(), "beatlumen recovery auth v1"),
            kek: derive(code.secret().as_bytes(), "beatlumen recovery kek v1"),
        }
    }
}

/// The value the server stores to check an auth key. The pepper is a secret
/// of the server kept outside the database, so a stolen database alone does
/// not even allow checking guesses offline.
pub fn verifier(auth: &[u8], pepper: &[u8]) -> [u8; 32] {
    sha256(&[b"beatlumen verifier v1\0", auth, pepper])
}

/// The salt the server hands out for a user that does not exist, so
/// prelogin does not reveal which names are registered. Stable per name.
pub fn fake_salt(pepper: &[u8], user: &str) -> [u8; SALT_LEN] {
    sha256(&[b"beatlumen fake salt v1\0", pepper, user.as_bytes()])[..SALT_LEN]
        .try_into()
        .expect("16")
}

fn master_aad(user: &str, kind: &str) -> Vec<u8> {
    let mut a = format!("beatlumen master v1\0{kind}\0").into_bytes();
    a.extend_from_slice(user.as_bytes());
    a
}

fn wrap_master(kek: &Secret, user: &str, kind: &str, master: &Secret) -> Result<Vec<u8>, Error> {
    aead::seal(kek, &master_aad(user, kind), master.as_bytes())
}

fn unwrap_master(kek: &Secret, user: &str, kind: &str, data: &[u8]) -> Result<Secret, Error> {
    let mut raw = aead::open(kek, &master_aad(user, kind), data)?;
    let out = raw.as_slice().try_into().map(Secret::from_bytes);
    raw.zeroize();
    out.map_err(|_| Error::Format("master key length"))
}

/// The fixed PRF input of passkeys (the output differs per passkey and
/// server anyway; a fixed input allows sign-in without a user name).
pub fn passkey_prf_input() -> [u8; 32] {
    sha256(&[b"beatlumen passkey prf v1"])
}

/// The key a passkey's PRF output wraps the master key with.
pub fn passkey_kek(prf: &[u8]) -> Result<Secret, Error> {
    if prf.len() < 32 {
        return Err(Error::Format("passkey PRF output too short"));
    }
    Ok(derive(prf, "beatlumen passkey kek v1"))
}

fn passkey_kind(credential_id: &[u8]) -> String {
    let h = sha256(&[credential_id]);
    let hex: String = h[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("passkey:{hex}")
}

/// Wraps the master key for a passkey (`credential_id`, PRF output).
pub fn wrap_master_for_passkey(
    user: &str,
    master: &Secret,
    credential_id: &[u8],
    prf: &[u8],
) -> Result<Vec<u8>, Error> {
    let user = normalize_user(user)?;
    wrap_master(
        &passkey_kek(prf)?,
        &user,
        &passkey_kind(credential_id),
        master,
    )
}

/// Unwraps the master key with a passkey's PRF output.
pub fn unwrap_master_with_passkey(
    user: &str,
    credential_id: &[u8],
    prf: &[u8],
    wrapped: &[u8],
) -> Result<Secret, Error> {
    let user = normalize_user(user)?;
    unwrap_master(
        &passkey_kek(prf)?,
        &user,
        &passkey_kind(credential_id),
        wrapped,
    )
}

/// Unwraps the master key with the password keys (for the sign-in page,
/// which hands the master key on instead of keeping an account open).
pub fn unwrap_master_with_password(
    user: &str,
    keys: &PasswordKeys,
    wrapped: &[u8],
) -> Result<Secret, Error> {
    let user = normalize_user(user)?;
    unwrap_master(&keys.kek, &user, "password", wrapped)
}

/// A new password for the master key (also used by the sign-in page,
/// which works with the master key directly).
pub fn password_change(
    user: &str,
    master: &Secret,
    new_password: &str,
) -> Result<PasswordChange, Error> {
    let user = normalize_user(user)?;
    let mut salt = [0u8; SALT_LEN];
    random(&mut salt)?;
    let params = KdfParams::DEFAULT;
    let pk = PasswordKeys::derive(new_password, &salt, params)?;
    Ok(PasswordChange {
        salt,
        params,
        master_by_password: wrap_master(&pk.kek, &user, "password", master)?,
        auth: pk.auth.clone(),
    })
}

/// Everything the server stores for an account, apart from the verifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBundle {
    pub salt: [u8; SALT_LEN],
    pub params: KdfParams,
    /// Master key wrapped by the password key.
    pub master_by_password: Vec<u8>,
    /// Master key wrapped by the recovery key.
    pub master_by_recovery: Vec<u8>,
    /// Identity private keys wrapped by the master key.
    pub identity: Vec<u8>,
    pub public: PublicIdentity,
}

/// What registration uploads: the bundle and both auth keys (the server
/// hashes them with its pepper and forgets them).
pub struct Registration {
    pub bundle: KeyBundle,
    pub auth: Secret,
    pub recovery_auth: Secret,
}

/// A new password: replaces salt, parameters, verifier and the password
/// wrapping of the master key. Files are untouched.
pub struct PasswordChange {
    pub salt: [u8; SALT_LEN],
    pub params: KdfParams,
    pub auth: Secret,
    pub master_by_password: Vec<u8>,
}

/// A new recovery code: replaces its verifier and wrapping.
pub struct RecoveryChange {
    pub recovery_auth: Secret,
    pub master_by_recovery: Vec<u8>,
}

/// An unlocked account on the user's device.
pub struct Account {
    pub user: String,
    master: Secret,
    pub identity: Identity,
}

impl Account {
    /// Creates a new account. The recovery code must be shown to the user
    /// once; it is not stored anywhere.
    pub fn register(
        user: &str,
        password: &str,
    ) -> Result<(Account, Registration, RecoveryCode), Error> {
        let user = &normalize_user(user)?;
        let master = Secret::random()?;
        let identity = Identity::generate()?;
        let code = RecoveryCode::generate()?;
        let mut salt = [0u8; SALT_LEN];
        random(&mut salt)?;
        let params = KdfParams::DEFAULT;
        let pk = PasswordKeys::derive(password, &salt, params)?;
        let rk = RecoveryKeys::derive(&code);
        let bundle = KeyBundle {
            salt,
            params,
            master_by_password: wrap_master(&pk.kek, user, "password", &master)?,
            master_by_recovery: wrap_master(&rk.kek, user, "recovery", &master)?,
            identity: identity.wrap(&master)?,
            public: identity.public(),
        };
        let reg = Registration {
            bundle,
            auth: pk.auth.clone(),
            recovery_auth: rk.auth.clone(),
        };
        let account = Account {
            user: user.to_string(),
            master,
            identity,
        };
        Ok((account, reg, code))
    }

    /// Unlocks with the password keys (derived before login, since the
    /// auth key is needed to get the bundle).
    pub fn unlock(user: &str, keys: &PasswordKeys, bundle: &KeyBundle) -> Result<Account, Error> {
        let user = &normalize_user(user)?;
        let master = unwrap_master(&keys.kek, user, "password", &bundle.master_by_password)?;
        Account::open(user, master, bundle)
    }

    /// Unlocks with the recovery code. Set a new password and a new
    /// recovery code right after.
    pub fn recover(user: &str, code: &RecoveryCode, bundle: &KeyBundle) -> Result<Account, Error> {
        let user = &normalize_user(user)?;
        let rk = RecoveryKeys::derive(code);
        let master = unwrap_master(&rk.kek, user, "recovery", &bundle.master_by_recovery)?;
        Account::open(user, master, bundle)
    }

    /// Opens the account with its master key (from a passkey or the
    /// browser sign-in).
    pub fn from_master(user: &str, master: Secret, bundle: &KeyBundle) -> Result<Account, Error> {
        let user = &normalize_user(user)?;
        Account::open(user, master, bundle)
    }

    /// The master key (for wrapping it for a new passkey or handing it to
    /// an app).
    pub fn master(&self) -> &Secret {
        &self.master
    }

    fn open(user: &str, master: Secret, bundle: &KeyBundle) -> Result<Account, Error> {
        let identity = Identity::unwrap(&master, &bundle.identity)?;
        // The published key must be ours: a server that swapped it could
        // read files others share with us.
        if identity.public() != bundle.public {
            return Err(Error::Signature);
        }
        Ok(Account {
            user: user.to_string(),
            master,
            identity,
        })
    }

    pub fn change_password(&self, new_password: &str) -> Result<PasswordChange, Error> {
        password_change(&self.user, &self.master, new_password)
    }

    /// Issues a new recovery code (the old one stops working once the
    /// server stores the change).
    pub fn new_recovery_code(&self) -> Result<(RecoveryChange, RecoveryCode), Error> {
        let code = RecoveryCode::generate()?;
        let rk = RecoveryKeys::derive(&code);
        let change = RecoveryChange {
            master_by_recovery: wrap_master(&rk.kek, &self.user, "recovery", &self.master)?,
            recovery_auth: rk.auth.clone(),
        };
        Ok((change, code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_unlock_change_recover() {
        let (acc, reg, code) = Account::register("alice", "correct horse").unwrap();
        let b = &reg.bundle;
        let pk = PasswordKeys::derive("correct horse", &b.salt, b.params).unwrap();
        assert_eq!(pk.auth, reg.auth);
        let un = Account::unlock("alice", &pk, b).unwrap();
        assert_eq!(un.identity.public(), acc.identity.public());

        // Wrong password, wrong user name, swapped public key.
        let bad = PasswordKeys::derive("wrong", &b.salt, b.params).unwrap();
        assert_ne!(bad.auth, reg.auth);
        assert!(Account::unlock("alice", &bad, b).is_err());
        assert!(Account::unlock("bob", &pk, b).is_err());
        let mut swapped = b.clone();
        swapped.public = Identity::generate().unwrap().public();
        assert_eq!(
            Account::unlock("alice", &pk, &swapped).err(),
            Some(Error::Signature)
        );
        // The password wrapping cannot be used as the recovery wrapping.
        let mut mixed = b.clone();
        mixed.master_by_recovery = b.master_by_password.clone();
        assert!(Account::recover("alice", &code, &mixed).is_err());

        // Recovery, then a new password and a new code.
        let rec =
            Account::recover("alice", &RecoveryCode::parse(&code.display()).unwrap(), b).unwrap();
        let ch = rec.change_password("new secret").unwrap();
        let (rc, code2) = rec.new_recovery_code().unwrap();
        let mut b2 = b.clone();
        b2.salt = ch.salt;
        b2.master_by_password = ch.master_by_password;
        b2.master_by_recovery = rc.master_by_recovery;
        let pk2 = PasswordKeys::derive("new secret", &b2.salt, b2.params).unwrap();
        assert_eq!(pk2.auth, ch.auth);
        assert!(Account::unlock("alice", &pk2, &b2).is_ok());
        assert!(Account::unlock("alice", &pk, &b2).is_err());
        assert!(Account::recover("alice", &code2, &b2).is_ok());
        assert!(Account::recover("alice", &code, &b2).is_err());
        assert_ne!(rc.recovery_auth, reg.recovery_auth);
    }

    #[test]
    fn weak_parameters_are_refused() {
        let salt = [1; SALT_LEN];
        let weak = KdfParams {
            m_kib: 1024,
            t: 1,
            p: 1,
        };
        assert_eq!(
            PasswordKeys::derive("pw", &salt, weak).err(),
            Some(Error::WeakParameters)
        );
        let huge = KdfParams {
            m_kib: u32::MAX,
            t: 3,
            p: 1,
        };
        assert!(PasswordKeys::derive("pw", &salt, huge).is_err());
    }

    #[test]
    fn passkey_wrapping() {
        let (acc, reg, _) = Account::register("alice", "correct horse").unwrap();
        let prf = [5u8; 32];
        let w = wrap_master_for_passkey("alice", acc.master(), b"cred-1", &prf).unwrap();
        let m = unwrap_master_with_passkey("Alice", b"cred-1", &prf, &w).unwrap();
        let back = Account::from_master("alice", m, &reg.bundle).unwrap();
        assert_eq!(back.identity.public(), acc.identity.public());
        // Another passkey, another credential id, another user: no.
        assert!(unwrap_master_with_passkey("alice", b"cred-1", &[6; 32], &w).is_err());
        assert!(unwrap_master_with_passkey("alice", b"cred-2", &prf, &w).is_err());
        assert!(unwrap_master_with_passkey("bob", b"cred-1", &prf, &w).is_err());
        assert!(passkey_kek(&[1; 16]).is_err());
        let pk =
            PasswordKeys::derive("correct horse", &reg.bundle.salt, reg.bundle.params).unwrap();
        let m2 = unwrap_master_with_password("alice", &pk, &reg.bundle.master_by_password).unwrap();
        assert_eq!(m2, *acc.master());
    }

    #[test]
    fn user_names() {
        assert_eq!(normalize_user(" Alice.B-1 ").unwrap(), "alice.b-1");
        for bad in ["", ".x", "a b", "ä", "a/b", &"x".repeat(33)] {
            assert!(normalize_user(bad).is_err(), "{bad}");
        }
        let (_, reg, _) = Account::register("alice", "pw").unwrap();
        assert_eq!(reg.bundle.master_by_password.len(), WRAPPED_MASTER_LEN);
        assert_eq!(reg.bundle.identity.len(), WRAPPED_IDENTITY_LEN);
    }

    #[test]
    fn verifier_and_fake_salt() {
        let v = verifier(b"auth", b"pepper");
        assert_ne!(v, verifier(b"auth", b"other pepper"));
        assert_ne!(v, verifier(b"auth2", b"pepper"));
        assert_eq!(fake_salt(b"p", "bob"), fake_salt(b"p", "bob"));
        assert_ne!(fake_salt(b"p", "bob"), fake_salt(b"p", "eve"));
    }
}

#[cfg(test)]
mod stability {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Changing the derivation would lock every user out: these values must
    /// never change for this format version. They were checked against an
    /// independent implementation (argon2-cffi and Python's hmac).
    #[test]
    fn derivation_is_stable() {
        let pk = PasswordKeys::derive(
            "Beatlumen \u{266b} passwort",
            &[7; SALT_LEN],
            KdfParams::DEFAULT,
        )
        .unwrap();
        assert_eq!(
            hex(pk.auth.as_bytes()),
            "d31d1f2bb78ffe675c60daae7eddfae464d1fa6acaedb8e0ae81e2a5bc809ce8"
        );
        assert_eq!(
            hex(pk.kek.as_bytes()),
            "3d5beb2af51bc2da3878ecae9c55a15f3eaa1c8f11bfde1c06b481e37cbffbca"
        );
        let rk = RecoveryKeys::derive(&RecoveryCode::from_secret(Secret::from_bytes([9; 32])));
        assert_eq!(
            hex(rk.auth.as_bytes()),
            "05681e697f4310d7836464ecd80698b2df12017e86436bab7b9e4a2697dbbcb4"
        );
        assert_eq!(
            hex(rk.kek.as_bytes()),
            "d14b9f0dc666dd2853a4dca3d0b4d9f227ea63f6a921a2e9fd434f74351532b2"
        );
    }
}
