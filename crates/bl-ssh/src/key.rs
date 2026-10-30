//! Login credentials: a password or an Ed25519 private key in OpenSSH's
//! format (`-----BEGIN OPENSSH PRIVATE KEY-----`, PROTOCOL.key), which
//! `ssh-keygen -t ed25519` writes. Keys protected by a passphrase and other
//! key types are refused with an explanation.

use crate::wire::{Put, Reader};
use crate::Error;
use ed25519_dalek::{Signer, SigningKey};
use zeroize::{Zeroize, Zeroizing};

const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
const END: &str = "-----END OPENSSH PRIVATE KEY-----";
const MAGIC: &[u8] = b"openssh-key-v1\0";

/// How to log in.
pub enum Auth {
    Password(Zeroizing<String>),
    Key(Box<PrivateKey>),
}

impl Auth {
    pub fn password(p: &str) -> Auth {
        Auth::Password(Zeroizing::new(p.to_string()))
    }

    pub fn key(k: PrivateKey) -> Auth {
        Auth::Key(Box::new(k))
    }

    /// A private key when `secret` is one (it starts with a `-----BEGIN`
    /// line), a password otherwise.
    pub fn from_secret(secret: &str) -> Result<Auth, Error> {
        let t = secret.trim_start();
        if t.starts_with("-----BEGIN ") || t.starts_with("PuTTY-User-Key-File") {
            PrivateKey::from_openssh(t).map(Auth::key)
        } else {
            Ok(Auth::password(secret))
        }
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::Password(_) => write!(f, "Auth::Password(…)"),
            Auth::Key(k) => write!(f, "Auth::Key({})", k.fingerprint()),
        }
    }
}

/// An Ed25519 private key (zeroed when dropped).
pub struct PrivateKey {
    key: SigningKey,
    comment: String,
}

fn key_error(m: &str) -> Error {
    Error::Key(m.into())
}

impl PrivateKey {
    /// Reads an unencrypted Ed25519 key in OpenSSH's format.
    pub fn from_openssh(text: &str) -> Result<PrivateKey, Error> {
        let text = text.trim();
        if text.len() > 64 * 1024 {
            return Err(key_error("the private key is too long"));
        }
        let Some(body) = text.strip_prefix(BEGIN) else {
            return Err(key_error(
                "unsupported private key format; use an Ed25519 key in OpenSSH format (ssh-keygen -t ed25519)",
            ));
        };
        let body = body
            .strip_suffix(END)
            .ok_or_else(|| key_error("the private key is incomplete"))?;
        let data = Zeroizing::new(
            crate::base64::decode(body).ok_or_else(|| key_error("the private key is damaged"))?,
        );
        fn damaged(_: Error) -> Error {
            key_error("the private key is damaged")
        }
        let body = data
            .strip_prefix(MAGIC)
            .ok_or_else(|| key_error("the private key is damaged"))?;
        let mut r = Reader::new(body);
        let cipher = r.str().map_err(damaged)?;
        let kdf = r.str().map_err(damaged)?;
        r.string().map_err(damaged)?; // KDF options
        if cipher != "none" || kdf != "none" {
            return Err(key_error(
                "the private key is protected by a passphrase, which is not supported; remove it with ssh-keygen -p",
            ));
        }
        if r.u32().map_err(damaged)? != 1 {
            return Err(key_error("the key file must hold exactly one key"));
        }
        let public = r.string().map_err(damaged)?;
        let mut p = Reader::new(r.string().map_err(damaged)?);
        if p.u32().map_err(damaged)? != p.u32().map_err(damaged)? {
            return Err(key_error("the private key is damaged"));
        }
        let kind = p.str().map_err(damaged)?;
        if kind != "ssh-ed25519" {
            return Err(Error::Key(format!(
                "{kind} keys are not supported; use an Ed25519 key (ssh-keygen -t ed25519)"
            )));
        }
        let pk = p.string().map_err(damaged)?;
        let sk = p.string().map_err(damaged)?;
        let comment = String::from_utf8_lossy(p.string().map_err(damaged)?).into_owned();
        // The secret part is the 32-byte seed followed by the public key.
        if pk.len() != 32 || sk.len() != 64 || &sk[32..] != pk {
            return Err(key_error("the private key is damaged"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&sk[..32]);
        let key = SigningKey::from_bytes(&seed);
        seed.zeroize();
        let k = PrivateKey { key, comment };
        if k.key.verifying_key().as_bytes() != pk || k.public_blob() != public {
            return Err(key_error("the private key does not match its public key"));
        }
        Ok(k)
    }

    /// The public key in SSH encoding.
    pub fn public_blob(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.string(b"ssh-ed25519");
        b.string(self.key.verifying_key().as_bytes());
        b
    }

    /// The public key as a line for `authorized_keys`.
    pub fn public_key(&self) -> String {
        let line = format!(
            "ssh-ed25519 {}",
            crate::base64::encode(&self.public_blob(), true)
        );
        if self.comment.is_empty() {
            line
        } else {
            format!("{line} {}", self.comment)
        }
    }

    pub fn fingerprint(&self) -> String {
        crate::hostkey::fingerprint(&self.public_blob())
    }

    pub(crate) fn algorithm(&self) -> &'static str {
        "ssh-ed25519"
    }

    /// A signature of `data` in SSH encoding.
    pub(crate) fn sign(&self, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.string(b"ssh-ed25519");
        b.string(&self.key.sign(data).to_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made for these tests with `ssh-keygen -t ed25519 -N ''`; not used
    /// anywhere else.
    const TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDnEqEhZDEet9gPvs40LibQyivQLh2q6sOfkcUlPV/eOAAAAJhDvmORQ75j
kQAAAAtzc2gtZWQyNTUxOQAAACDnEqEhZDEet9gPvs40LibQyivQLh2q6sOfkcUlPV/eOA
AAAEALRnc++LERCa5/T58MXG1IdherF6NN2hI6tUbB0ch2becSoSFkMR632A++zjQuJtDK
K9AuHarqw5+RxSU9X944AAAADmJlYXRsdW1lbi10ZXN0AQIDBAUGBw==
-----END OPENSSH PRIVATE KEY-----
";
    const TEST_PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOcSoSFkMR632A++zjQuJtDKK9AuHarqw5+RxSU9X944 beatlumen-test";

    /// The same kind of key with the passphrase "secret".
    const ENCRYPTED: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABAOKryrJm
a/eNUHOyrFm3A7AAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIGYdD3kqjudnqQUV
ng807i8VpnR7H9gVQCeBVm10ZqveAAAAkMd0gkM/nvpjIHRbDi+lH+j5tKHnYSfe4KQf34
Ljcl0YWA8u1SGr3wPREcjAgL3fGD3fJ9zfd1DMK9ejjZxusDV44QQmRLx2baQYYQX3309H
HA165b1V6sp7p72VF57efPmyPsnrQlJ2Dw6KW/Ze42IhzOmt5F+XRR0YSnL2qWmqs9qL1m
ZhiMAO3VVv//o4+w==
-----END OPENSSH PRIVATE KEY-----";

    /// An ECDSA key (`ssh-keygen -t ecdsa`).
    const ECDSA: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAaAAAABNlY2RzYS
1zaGEyLW5pc3RwMjU2AAAACG5pc3RwMjU2AAAAQQSRr5dQ1zTn5z6JXk6hwOKP/GzXWDrq
MyJE+waPJU0tSkSpQVMhEhOd/+Ya86BYVy7wJPBNluu4PLDlMQ0xgZHGAAAAoMgiLtXIIi
7VAAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBJGvl1DXNOfnPole
TqHA4o/8bNdYOuozIkT7Bo8lTS1KRKlBUyESE53/5hrzoFhXLvAk8E2W67g8sOUxDTGBkc
YAAAAhAP/e7aVx9b1GVDgFOIKpsT8ac1WaL2NQnOv0FlMOQn9SAAAAB3Jvb3RAdm0=
-----END OPENSSH PRIVATE KEY-----";

    #[test]
    fn reads_openssh_ed25519_keys() {
        let k = PrivateKey::from_openssh(TEST_KEY).unwrap();
        assert_eq!(k.public_key(), TEST_PUB);
        // `ssh-keygen -lf` of the public key.
        assert_eq!(
            k.fingerprint(),
            "SHA256:wI4KuqST49UnQBEb6o8oKBn0P1fXQ2em3W/kWQvZHGE"
        );
        // Windows line ends and surrounding blanks.
        let crlf = format!("\n  {}  \n", TEST_KEY.replace('\n', "\r\n"));
        assert!(matches!(Auth::from_secret(&crlf), Ok(Auth::Key(_))));
        assert!(matches!(
            Auth::from_secret("hunter2"),
            Ok(Auth::Password(p)) if p.as_str() == "hunter2"
        ));
    }

    #[test]
    fn refuses_what_it_cannot_use() {
        let msg = |r: Result<PrivateKey, Error>| r.err().unwrap().to_string();
        assert!(msg(PrivateKey::from_openssh(ENCRYPTED)).contains("passphrase"));
        assert!(msg(PrivateKey::from_openssh(ECDSA)).contains("ecdsa-sha2-nistp256"));
        assert!(msg(PrivateKey::from_openssh(
            "-----BEGIN RSA PRIVATE KEY-----\nMII\n-----END RSA PRIVATE KEY-----"
        ))
        .contains("format"));
        let damaged = TEST_KEY.replace("K9Au", "K9Bu");
        assert!(PrivateKey::from_openssh(&damaged).is_err());
        let cut = &TEST_KEY[..TEST_KEY.len() - 40];
        assert!(PrivateKey::from_openssh(cut).is_err());
    }

    #[test]
    fn signatures_verify() {
        let k = PrivateKey::from_openssh(TEST_KEY).unwrap();
        let sig = k.sign(b"data");
        crate::hostkey::verify("ssh-ed25519", &k.public_blob(), &sig, b"data").unwrap();
    }
}
