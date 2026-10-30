//! Packet protection: the two AEAD ciphers OpenSSH offers, which carry
//! their own authentication (no separate MAC):
//!
//! * `chacha20-poly1305@openssh.com` (OpenSSH's PROTOCOL.chacha20poly1305):
//!   a 64-byte key, the second half encrypts the 4-byte length, the first
//!   half the rest from block 1 on; block 0 yields the Poly1305 key; the
//!   nonce is the packet sequence number; the tag covers the encrypted
//!   length and body.
//! * `aes256-gcm@openssh.com` (RFC 5647 as OpenSSH uses it): the length
//!   stays plain and is the associated data; the 12-byte nonce is a fixed
//!   part and a 64-bit counter incremented per packet.

use crate::Error;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20::ChaCha20Legacy;
use poly1305::Poly1305;
use zeroize::Zeroize;

/// The packet ciphers this client offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cipher {
    ChaCha20Poly1305,
    Aes256Gcm,
}

impl Cipher {
    pub const ALL: [Cipher; 2] = [Cipher::ChaCha20Poly1305, Cipher::Aes256Gcm];

    pub fn name(self) -> &'static str {
        match self {
            Cipher::ChaCha20Poly1305 => "chacha20-poly1305@openssh.com",
            Cipher::Aes256Gcm => "aes256-gcm@openssh.com",
        }
    }

    pub(crate) fn key_len(self) -> usize {
        match self {
            Cipher::ChaCha20Poly1305 => 64,
            Cipher::Aes256Gcm => 32,
        }
    }

    pub(crate) fn iv_len(self) -> usize {
        match self {
            Cipher::ChaCha20Poly1305 => 0,
            Cipher::Aes256Gcm => 12,
        }
    }
}

pub(crate) const TAG_LEN: usize = 16;

/// The protection of one direction: none before the first key exchange.
pub(crate) enum PacketCipher {
    None,
    ChaCha { main: [u8; 32], header: [u8; 32] },
    Gcm { aead: Box<Aes256Gcm>, iv: [u8; 12] },
}

impl Drop for PacketCipher {
    fn drop(&mut self) {
        if let PacketCipher::ChaCha { main, header } = self {
            main.zeroize();
            header.zeroize();
        }
    }
}

fn chacha(key: &[u8; 32], seq: u32) -> ChaCha20Legacy {
    let nonce = (seq as u64).to_be_bytes();
    ChaCha20Legacy::new(key.into(), &nonce.into())
}

/// Poly1305 over `data` with the key from block 0 of the main stream.
fn chacha_tag(main: &[u8; 32], seq: u32, data: &[u8]) -> [u8; TAG_LEN] {
    let mut key = [0u8; 32];
    chacha(main, seq).apply_keystream(&mut key);
    let tag = Poly1305::new((&key).into()).compute_unpadded(data);
    key.zeroize();
    tag.into()
}

/// Constant-time comparison of two tags.
fn tags_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn bad_mac() -> Error {
    Error::Protocol("message authentication failed".into())
}

impl PacketCipher {
    /// `key` and `iv` as derived by the key exchange (lengths from
    /// [`Cipher::key_len`] and [`Cipher::iv_len`]).
    pub fn new(kind: Cipher, key: &[u8], iv: &[u8]) -> PacketCipher {
        match kind {
            Cipher::ChaCha20Poly1305 => {
                let mut main = [0u8; 32];
                let mut header = [0u8; 32];
                main.copy_from_slice(&key[..32]);
                header.copy_from_slice(&key[32..64]);
                PacketCipher::ChaCha { main, header }
            }
            Cipher::Aes256Gcm => {
                let mut nonce = [0u8; 12];
                nonce.copy_from_slice(&iv[..12]);
                PacketCipher::Gcm {
                    aead: Box::new(Aes256Gcm::new(key[..32].into())),
                    iv: nonce,
                }
            }
        }
    }

    /// Padding granularity.
    pub fn block_size(&self) -> usize {
        match self {
            PacketCipher::None | PacketCipher::ChaCha { .. } => 8,
            PacketCipher::Gcm { .. } => 16,
        }
    }

    /// Whether the length field is outside the padded part (AEAD).
    pub fn aead(&self) -> bool {
        !matches!(self, PacketCipher::None)
    }

    pub fn tag_len(&self) -> usize {
        if self.aead() {
            TAG_LEN
        } else {
            0
        }
    }

    /// Protects a packet (`buf`: length field, padding length, payload,
    /// padding) in place and appends the tag.
    pub fn seal(&mut self, seq: u32, buf: &mut Vec<u8>) -> Result<(), Error> {
        match self {
            PacketCipher::None => {}
            PacketCipher::ChaCha { main, header } => {
                chacha(header, seq).apply_keystream(&mut buf[..4]);
                let mut c = chacha(main, seq);
                c.seek(64u64);
                c.apply_keystream(&mut buf[4..]);
                let tag = chacha_tag(main, seq, buf);
                buf.extend_from_slice(&tag);
            }
            PacketCipher::Gcm { aead, iv } => {
                let (len, body) = buf.split_at_mut(4);
                let tag = aead
                    .encrypt_in_place_detached(Nonce::from_slice(iv), len, body)
                    .map_err(|_| Error::Protocol("encryption failed".into()))?;
                buf.extend_from_slice(&tag);
                next_iv(iv);
            }
        }
        Ok(())
    }

    /// The packet length from the first four bytes as received.
    pub fn length(&self, seq: u32, first: [u8; 4]) -> u32 {
        let mut b = first;
        if let PacketCipher::ChaCha { header, .. } = self {
            chacha(header, seq).apply_keystream(&mut b);
        }
        u32::from_be_bytes(b)
    }

    /// Checks and decrypts a received packet: `buf` holds the length field
    /// as received, the body and the tag. On success `buf[4..]` (without
    /// the tag) is plain.
    pub fn open(&mut self, seq: u32, buf: &mut [u8]) -> Result<(), Error> {
        let tag_len = self.tag_len();
        if buf.len() < 4 + tag_len {
            return Err(bad_mac());
        }
        let (data, tag) = buf.split_at_mut(buf.len() - tag_len);
        match self {
            PacketCipher::None => {}
            PacketCipher::ChaCha { main, .. } => {
                let want = chacha_tag(main, seq, data);
                if !tags_equal(&want, tag) {
                    return Err(bad_mac());
                }
                let mut c = chacha(main, seq);
                c.seek(64u64);
                c.apply_keystream(&mut data[4..]);
            }
            PacketCipher::Gcm { aead, iv } => {
                let (len, body) = data.split_at_mut(4);
                aead.decrypt_in_place_detached(
                    Nonce::from_slice(iv),
                    len,
                    body,
                    Tag::from_slice(tag),
                )
                .map_err(|_| bad_mac())?;
                next_iv(iv);
            }
        }
        Ok(())
    }
}

/// Increments the 64-bit invocation counter of a GCM nonce.
fn next_iv(iv: &mut [u8; 12]) {
    let mut c = [0u8; 8];
    c.copy_from_slice(&iv[4..]);
    let c = u64::from_be_bytes(c).wrapping_add(1);
    iv[4..].copy_from_slice(&c.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A 16-byte aligned packet: length 28, padding length 4, payload.
    fn packet() -> Vec<u8> {
        let mut p = vec![0, 0, 0, 28, 7];
        p.extend_from_slice(b"Beatlumen SSH test pkt");
        p.extend_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(p.len(), 32);
        p
    }

    /// Known answers computed independently with Python's `cryptography`
    /// (ChaCha20 with a 64-bit counter and nonce, Poly1305) following
    /// OpenSSH's PROTOCOL.chacha20poly1305.
    #[test]
    fn chacha20_poly1305_openssh_vector() {
        let key: Vec<u8> = (0u8..64).collect();
        let mut c = PacketCipher::new(Cipher::ChaCha20Poly1305, &key, &[]);
        let mut buf = packet();
        c.seal(7, &mut buf).unwrap();
        assert_eq!(
            buf,
            hex(concat!(
                "a39afcb6",
                "2f0470223aef5f3309039ba384c7f35851c144160e88498242691fda",
                "2cc26d895c620565305cd7ea57ccb55c"
            ))
        );
        let mut d = PacketCipher::new(Cipher::ChaCha20Poly1305, &key, &[]);
        assert_eq!(d.length(7, [buf[0], buf[1], buf[2], buf[3]]), 28);
        d.open(7, &mut buf).unwrap();
        assert_eq!(&buf[4..32], &packet()[4..]);
    }

    #[test]
    fn aes256_gcm_openssh_vector() {
        let key: Vec<u8> = (0u8..32).collect();
        let iv: Vec<u8> = (100u8..112).collect();
        let mut c = PacketCipher::new(Cipher::Aes256Gcm, &key, &iv);
        let mut first = packet();
        c.seal(0, &mut first).unwrap();
        // The second packet uses the incremented counter.
        let mut second = packet();
        c.seal(1, &mut second).unwrap();
        assert_eq!(&first[..4], &[0, 0, 0, 28]);
        assert_eq!(
            first,
            hex(concat!(
                "0000001c",
                "4f59bb070d8523f35b0c7fbb892d4a8927b1722afb078773a5d2a84d",
                "405fd3c90a1e58f2a86bffe10e57dae8"
            ))
        );
        assert_ne!(first, second);
        let mut d = PacketCipher::new(Cipher::Aes256Gcm, &key, &iv);
        d.open(0, &mut first).unwrap();
        d.open(1, &mut second).unwrap();
        assert_eq!(&first[4..32], &packet()[4..]);
        assert_eq!(&second[4..32], &packet()[4..]);
    }

    #[test]
    fn tampering_is_detected() {
        for kind in Cipher::ALL {
            let key = vec![9u8; kind.key_len()];
            let iv = vec![3u8; kind.iv_len()];
            let mut c = PacketCipher::new(kind, &key, &iv);
            let mut buf = packet();
            c.seal(5, &mut buf).unwrap();
            for i in [0, 4, 20, buf.len() - 1] {
                let mut bad = buf.clone();
                bad[i] ^= 1;
                let mut d = PacketCipher::new(kind, &key, &iv);
                assert!(d.open(5, &mut bad).is_err(), "{kind:?} byte {i}");
            }
            // A wrong sequence number (a dropped or replayed packet).
            if kind == Cipher::ChaCha20Poly1305 {
                let mut d = PacketCipher::new(kind, &key, &iv);
                assert!(d.open(6, &mut buf.clone()).is_err());
            }
            let mut d = PacketCipher::new(kind, &key, &iv);
            d.open(5, &mut buf).unwrap();
        }
    }
}
