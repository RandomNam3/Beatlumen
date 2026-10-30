//! Key exchange (RFC 4253 section 7) with curve25519-sha256 (RFC 8731):
//! algorithm negotiation, the ephemeral X25519 exchange, the exchange hash
//! signed by the server's host key, the pin check and the derived keys.
//!
//! Strict key exchange (OpenSSH's `kex-strict-*-v00@openssh.com`, against
//! the Terrapin attack): sequence numbers restart at zero after each
//! NEWKEYS when the server supports it. Independently of that, the first
//! exchange accepts nothing but its own messages in their order, so a
//! man in the middle cannot slip in packets that shift sequence numbers.
//! A later exchange the server starts (rekeying) must show the same host
//! key.

use crate::cipher::{Cipher, PacketCipher};
use crate::transport::{RecvHalf, SendHalf};
use crate::wire::{Put, Reader};
use crate::{hostkey, msg, Error};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const KEX: &[&str] = &["curve25519-sha256", "curve25519-sha256@libssh.org"];
const STRICT_CLIENT: &str = "kex-strict-c-v00@openssh.com";
const STRICT_SERVER: &str = "kex-strict-s-v00@openssh.com";
/// Never used: both ciphers authenticate packets themselves. Listed for
/// servers that want a MAC in common regardless.
const MACS: &str =
    "hmac-sha2-256-etm@openssh.com,hmac-sha2-512-etm@openssh.com,hmac-sha2-256,hmac-sha2-512";

/// What key exchanges of a connection share.
pub(crate) struct KexContext {
    pub v_c: Vec<u8>,
    pub v_s: Vec<u8>,
    /// Expected host key fingerprint ("" = report the key).
    pub pin: String,
    /// Offered ciphers in order of preference.
    pub ciphers: Vec<Cipher>,
    /// The first exchange hash.
    pub session_id: Option<Vec<u8>>,
    pub host_key: Vec<u8>,
    pub fingerprint: String,
    pub strict: bool,
    /// Cipher client to server (the one reported).
    pub cipher: Option<Cipher>,
}

impl KexContext {
    pub fn new(v_s: Vec<u8>, pin: &str, ciphers: &[Cipher]) -> KexContext {
        KexContext {
            v_c: crate::transport::CLIENT_VERSION.as_bytes().to_vec(),
            v_s,
            pin: pin.to_string(),
            ciphers: ciphers.to_vec(),
            session_id: None,
            host_key: Vec::new(),
            fingerprint: String::new(),
            strict: false,
            cipher: None,
        }
    }
}

/// The algorithms both sides use.
#[derive(Debug, PartialEq)]
struct Negotiated {
    host_key: &'static str,
    c2s: Cipher,
    s2c: Cipher,
    strict: bool,
    /// The server sent a guessed key exchange packet that has to be skipped.
    skip_guess: bool,
}

fn random(buf: &mut [u8]) -> Result<(), Error> {
    getrandom::getrandom(buf).map_err(|_| Error::Protocol("no random numbers available".into()))
}

fn kexinit(ctx: &KexContext) -> Result<Vec<u8>, Error> {
    let mut p = vec![msg::KEXINIT];
    let mut cookie = [0u8; 16];
    random(&mut cookie)?;
    p.extend_from_slice(&cookie);
    let mut kex = KEX.join(",");
    // Only the first KEXINIT may announce strict mode.
    if ctx.session_id.is_none() {
        kex.push(',');
        kex.push_str(STRICT_CLIENT);
    }
    let ciphers: Vec<&str> = ctx.ciphers.iter().map(|c| c.name()).collect();
    let ciphers = ciphers.join(",");
    p.string(kex.as_bytes());
    p.string(hostkey::ALGORITHMS.join(",").as_bytes());
    p.string(ciphers.as_bytes());
    p.string(ciphers.as_bytes());
    p.string(MACS.as_bytes());
    p.string(MACS.as_bytes());
    p.string(b"none");
    p.string(b"none");
    p.string(b"");
    p.string(b"");
    p.bool(false);
    p.u32(0);
    Ok(p)
}

fn no_match(what: &str, theirs: &[&str]) -> Error {
    Error::Negotiation(format!(
        "no common {what} algorithm (the server offers {})",
        theirs.join(",")
    ))
}

/// Picks the first algorithm of ours the server also offers.
fn negotiate(ciphers: &[Cipher], initial: bool, i_s: &[u8]) -> Result<Negotiated, Error> {
    let mut r = Reader::new(
        i_s.get(17..)
            .ok_or_else(|| Error::Protocol("short KEXINIT".into()))?,
    );
    let kex = r.name_list()?;
    let host_keys = r.name_list()?;
    let enc_cs = r.name_list()?;
    let enc_sc = r.name_list()?;
    let _mac_cs = r.name_list()?;
    let _mac_sc = r.name_list()?;
    let comp_cs = r.name_list()?;
    let comp_sc = r.name_list()?;
    r.name_list()?;
    r.name_list()?;
    let follows = r.bool()?;
    r.u32()?;
    let kex_alg = KEX
        .iter()
        .find(|a| kex.contains(a))
        .ok_or_else(|| no_match("key exchange", &kex))?;
    let host_key = *hostkey::ALGORITHMS
        .iter()
        .find(|a| host_keys.contains(a))
        .ok_or_else(|| no_match("host key", &host_keys))?;
    let pick = |theirs: &[&str]| {
        ciphers
            .iter()
            .copied()
            .find(|c| theirs.contains(&c.name()))
            .ok_or_else(|| no_match("cipher", theirs))
    };
    let c2s = pick(&enc_cs)?;
    let s2c = pick(&enc_sc)?;
    if !comp_cs.contains(&"none") || !comp_sc.contains(&"none") {
        return Err(no_match("compression", &comp_cs));
    }
    Ok(Negotiated {
        host_key,
        c2s,
        s2c,
        strict: initial && kex.contains(&STRICT_SERVER),
        skip_guess: follows
            && (kex.first() != Some(kex_alg) || host_keys.first() != Some(&host_key)),
    })
}

/// The reason of a DISCONNECT message.
pub(crate) fn disconnected(m: &[u8]) -> Error {
    let mut r = Reader::new(&m[1..]);
    let _code = r.u32();
    let text = r
        .string()
        .map(|s| String::from_utf8_lossy(&s[..s.len().min(200)]).into_owned())
        .unwrap_or_default();
    Error::Io(format!("the server closed the connection: {text}"))
}

/// Receives the next message of the exchange; during a rekeying the server
/// may interleave IGNORE, DEBUG and UNIMPLEMENTED.
fn expect(rx: &mut RecvHalf, initial: bool, want: u8) -> Result<Vec<u8>, Error> {
    loop {
        let m = rx.recv()?;
        match m[0] {
            t if t == want => return Ok(m),
            msg::DISCONNECT => return Err(disconnected(&m)),
            msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED if !initial => continue,
            t => {
                return Err(Error::Protocol(format!(
                    "unexpected message {t} during the key exchange"
                )))
            }
        }
    }
}

/// Runs a key exchange. `server_init` is the server's KEXINIT when it
/// started a rekeying; for the first exchange it is read here. Nothing
/// else may be sent on `tx` until this returns.
pub(crate) fn run(
    tx: &mut SendHalf,
    rx: &mut RecvHalf,
    ctx: &mut KexContext,
    server_init: Option<Vec<u8>>,
) -> Result<(), Error> {
    let initial = ctx.session_id.is_none();
    let i_c = kexinit(ctx)?;
    tx.send(&i_c)?;
    let i_s = match server_init {
        Some(m) => m,
        None => expect(rx, initial, msg::KEXINIT)?,
    };
    let neg = negotiate(&ctx.ciphers, initial, &i_s)?;
    if initial {
        ctx.strict = neg.strict;
    }
    if neg.skip_guess {
        rx.recv()?;
    }

    let mut seed = Zeroizing::new([0u8; 32]);
    random(&mut seed[..])?;
    let secret = x25519_dalek::StaticSecret::from(*seed);
    let q_c = x25519_dalek::PublicKey::from(&secret);
    let mut m = vec![msg::KEX_ECDH_INIT];
    m.string(q_c.as_bytes());
    tx.send(&m)?;

    let reply = expect(rx, initial, msg::KEX_ECDH_REPLY)?;
    let mut r = Reader::new(&reply[1..]);
    let k_s = r.string()?;
    let q_s: [u8; 32] = r
        .string()?
        .try_into()
        .map_err(|_| Error::Protocol("invalid key exchange value".into()))?;
    let sig = r.string()?;
    r.end()?;
    let shared = secret.diffie_hellman(&x25519_dalek::PublicKey::from(q_s));
    if !shared.was_contributory() {
        return Err(Error::Protocol("invalid key exchange value".into()));
    }
    let mut k = Zeroizing::new(Vec::new());
    k.mpint(shared.as_bytes());

    let mut hash = Vec::new();
    for s in [
        &ctx.v_c[..],
        &ctx.v_s,
        &i_c,
        &i_s,
        k_s,
        q_c.as_bytes(),
        &q_s,
    ] {
        hash.string(s);
    }
    let mut h = Sha256::new();
    h.update(&hash);
    h.update(&k[..]);
    let h = h.finalize().to_vec();

    // The server proves it holds the host key, then the key must be the
    // pinned one (the fingerprint reported is thus of a proven key).
    hostkey::verify(neg.host_key, k_s, sig, &h)?;
    let fp = hostkey::fingerprint(k_s);
    if initial {
        if ctx.pin.trim().is_empty() {
            return Err(Error::UnknownHostKey(fp));
        }
        let expected = hostkey::normalize_pin(&ctx.pin);
        if expected != fp {
            return Err(Error::HostKeyChanged {
                expected,
                found: fp,
            });
        }
        ctx.host_key = k_s.to_vec();
        ctx.fingerprint = fp;
    } else if k_s != ctx.host_key {
        return Err(Error::HostKeyChanged {
            expected: ctx.fingerprint.clone(),
            found: fp,
        });
    }

    let session_id = ctx.session_id.get_or_insert_with(|| h.clone()).clone();
    // RFC 4253 section 7.2: HASH(K || H || letter || session_id), extended
    // with HASH(K || H || what came before) as needed.
    let derive = |letter: u8, len: usize| {
        let mut out = Zeroizing::new(Vec::with_capacity(len + 32));
        let mut d = Sha256::new();
        d.update(&k[..]);
        d.update(&h);
        d.update([letter]);
        d.update(&session_id);
        out.extend_from_slice(&d.finalize());
        while out.len() < len {
            let mut d = Sha256::new();
            d.update(&k[..]);
            d.update(&h);
            d.update(&out[..]);
            let more = d.finalize();
            out.extend_from_slice(&more);
        }
        out.truncate(len);
        out
    };
    let tx_cipher = PacketCipher::new(
        neg.c2s,
        &derive(b'C', neg.c2s.key_len()),
        &derive(b'A', neg.c2s.iv_len()),
    );
    let rx_cipher = PacketCipher::new(
        neg.s2c,
        &derive(b'D', neg.s2c.key_len()),
        &derive(b'B', neg.s2c.iv_len()),
    );

    tx.send(&[msg::NEWKEYS])?;
    tx.set_cipher(tx_cipher);
    if ctx.strict {
        tx.seq = 0;
    }
    expect(rx, initial, msg::NEWKEYS)?;
    rx.set_cipher(rx_cipher);
    if ctx.strict {
        rx.seq = 0;
    }
    ctx.cipher = Some(neg.c2s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_init(lists: [&str; 10], follows: bool) -> Vec<u8> {
        let mut p = vec![msg::KEXINIT];
        p.extend_from_slice(&[0; 16]);
        for l in lists {
            p.string(l.as_bytes());
        }
        p.bool(follows);
        p.u32(0);
        p
    }

    const OPENSSH: [&str; 10] = [
        "sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org,ext-info-s,kex-strict-s-v00@openssh.com",
        "rsa-sha2-512,rsa-sha2-256,ecdsa-sha2-nistp256,ssh-ed25519",
        "chacha20-poly1305@openssh.com,aes128-ctr,aes256-gcm@openssh.com",
        "aes256-gcm@openssh.com",
        "umac-64-etm@openssh.com",
        "umac-64-etm@openssh.com",
        "none,zlib@openssh.com",
        "none",
        "",
        "",
    ];

    #[test]
    fn negotiation_follows_our_preferences() {
        let all = Cipher::ALL;
        let n = negotiate(&all, true, &server_init(OPENSSH, false)).unwrap();
        assert_eq!(
            n,
            Negotiated {
                host_key: "ssh-ed25519",
                c2s: Cipher::ChaCha20Poly1305,
                s2c: Cipher::Aes256Gcm,
                strict: true,
                skip_guess: false,
            }
        );
        // Strict mode only counts in the first exchange.
        assert!(
            !negotiate(&all, false, &server_init(OPENSSH, false))
                .unwrap()
                .strict
        );
        // The server guessed another key exchange algorithm.
        assert!(
            negotiate(&all, true, &server_init(OPENSSH, true))
                .unwrap()
                .skip_guess
        );
        let mut only_cbc = OPENSSH;
        only_cbc[2] = "aes128-cbc,3des-cbc";
        let e = negotiate(&all, true, &server_init(only_cbc, false)).unwrap_err();
        assert!(e.to_string().contains("aes128-cbc"), "{e}");
        let mut dss = OPENSSH;
        dss[1] = "ssh-dss,ssh-rsa";
        assert!(negotiate(&all, true, &server_init(dss, false)).is_err());
        let mut zlib = OPENSSH;
        zlib[7] = "zlib@openssh.com";
        assert!(negotiate(&all, true, &server_init(zlib, false)).is_err());
        assert!(negotiate(&all, true, &[msg::KEXINIT, 1, 2]).is_err());
    }
}
