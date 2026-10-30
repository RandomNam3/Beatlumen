//! The encrypted control connection after pairing: the byte stream is cut
//! into frames of a two-byte little-endian length (also the associated
//! data), up to 1024 bytes of ChaCha20-Poly1305 ciphertext and the tag.
//! Each direction has its own key and a 64-bit frame counter as nonce.

use crate::crypto::chacha::{open_in_place, seal_in_place};
use crate::crypto::sha512::hkdf_key;
use std::io::{self, Read};
use std::net::TcpStream;

/// Largest plaintext per frame.
const FRAME: usize = 1024;
/// Largest frame accepted from the peer.
const MAX_FRAME: usize = 16 * 1024;

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

/// Encrypts outgoing data.
pub struct Sealer {
    key: [u8; 32],
    counter: u64,
}

impl Sealer {
    pub fn new(key: [u8; 32]) -> Sealer {
        Sealer { key, counter: 0 }
    }

    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + plain.len() / FRAME * 18 + 18);
        for chunk in plain.chunks(FRAME) {
            let len = (chunk.len() as u16).to_le_bytes();
            let mut data = chunk.to_vec();
            let tag = seal_in_place(&self.key, &nonce(self.counter), &len, &mut data);
            self.counter += 1;
            out.extend_from_slice(&len);
            out.extend_from_slice(&data);
            out.extend_from_slice(&tag);
        }
        out
    }
}

/// Decrypts incoming data.
pub struct Opener {
    key: [u8; 32],
    counter: u64,
    /// Ciphertext of an incomplete frame.
    pending: Vec<u8>,
}

impl Opener {
    pub fn new(key: [u8; 32]) -> Opener {
        Opener {
            key,
            counter: 0,
            pending: Vec::new(),
        }
    }

    /// Adds received bytes; appends the plaintext of complete frames.
    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        self.pending.extend_from_slice(data);
        let mut pos = 0;
        while self.pending.len() - pos >= 2 {
            let len = u16::from_le_bytes([self.pending[pos], self.pending[pos + 1]]) as usize;
            if len > MAX_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame too large",
                ));
            }
            if self.pending.len() - pos < 2 + len + 16 {
                break;
            }
            let aad = [self.pending[pos], self.pending[pos + 1]];
            let mut data = self.pending[pos + 2..pos + 2 + len].to_vec();
            let tag = &self.pending[pos + 2 + len..pos + 2 + len + 16];
            if !open_in_place(&self.key, &nonce(self.counter), &aad, &mut data, tag) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "frame authentication failed",
                ));
            }
            self.counter += 1;
            out.extend_from_slice(&data);
            pos += 2 + len + 16;
        }
        self.pending.drain(..pos);
        Ok(())
    }
}

/// The two directions of the control connection derived from a pairing
/// secret, as seen by the receiver: (decrypt requests, encrypt responses).
pub fn control_keys(secret: &[u8]) -> (Opener, Sealer) {
    (
        Opener::new(hkdf_key(
            secret,
            "Control-Salt",
            "Control-Write-Encryption-Key",
        )),
        Sealer::new(hkdf_key(
            secret,
            "Control-Salt",
            "Control-Read-Encryption-Key",
        )),
    )
}

/// The control connection's reading side: plain until pairing completes,
/// decrypted afterwards.
pub struct Channel {
    stream: TcpStream,
    opener: Option<Opener>,
    plain: Vec<u8>,
    pos: usize,
}

impl Channel {
    pub fn new(stream: TcpStream) -> Channel {
        Channel {
            stream,
            opener: None,
            plain: Vec::new(),
            pos: 0,
        }
    }

    /// Switches to decryption; `buffered` is data already read ahead (it is
    /// ciphertext now).
    pub fn encrypt(&mut self, mut opener: Opener, buffered: &[u8]) -> io::Result<()> {
        self.plain.clear();
        self.pos = 0;
        opener.push(buffered, &mut self.plain)?;
        self.opener = Some(opener);
        Ok(())
    }

    pub fn is_encrypted(&self) -> bool {
        self.opener.is_some()
    }
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(opener) = &mut self.opener else {
            return self.stream.read(buf);
        };
        while self.pos >= self.plain.len() {
            self.plain.clear();
            self.pos = 0;
            let mut tmp = [0u8; 4096];
            let n = self.stream.read(&mut tmp)?;
            if n == 0 {
                return Ok(0);
            }
            opener.push(&tmp[..n], &mut self.plain)?;
        }
        let n = buf.len().min(self.plain.len() - self.pos);
        buf[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_in_pieces() {
        let key = [3u8; 32];
        let mut s = Sealer::new(key);
        let msg: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut wire = s.seal(&msg);
        wire.extend(s.seal(b"second"));
        // 3000 bytes: frames of 1024 + 1024 + 952, then one of 6.
        assert_eq!(wire.len(), 3000 + 3 * 18 + 6 + 18);
        let mut o = Opener::new(key);
        let mut out = Vec::new();
        for c in wire.chunks(7) {
            o.push(c, &mut out).unwrap();
        }
        assert_eq!(&out[..3000], &msg[..]);
        assert_eq!(&out[3000..], b"second");
    }

    #[test]
    fn tampering_and_replay_are_detected() {
        let key = [3u8; 32];
        let mut s = Sealer::new(key);
        let a = s.seal(b"hello");
        let mut bad = a.clone();
        bad[4] ^= 1;
        assert!(Opener::new(key).push(&bad, &mut Vec::new()).is_err());
        // The same frame twice: the second has the wrong counter.
        let mut o = Opener::new(key);
        o.push(&a, &mut Vec::new()).unwrap();
        assert!(o.push(&a, &mut Vec::new()).is_err());
        // Oversized length.
        assert!(Opener::new(key)
            .push(&[0xff, 0xff, 0, 0], &mut Vec::new())
            .is_err());
    }

    #[test]
    fn directions_use_different_keys() {
        let (mut open_req, mut seal_resp) = control_keys(b"secret");
        // A sender encrypts requests with the write key...
        let mut sender = Sealer::new(hkdf_key(
            b"secret",
            "Control-Salt",
            "Control-Write-Encryption-Key",
        ));
        let mut out = Vec::new();
        open_req.push(&sender.seal(b"GET /info"), &mut out).unwrap();
        assert_eq!(out, b"GET /info");
        // ...and cannot read responses with it.
        let resp = seal_resp.seal(b"RTSP/1.0 200 OK");
        let mut wrong = Opener::new(hkdf_key(
            b"secret",
            "Control-Salt",
            "Control-Write-Encryption-Key",
        ));
        assert!(wrong.push(&resp, &mut Vec::new()).is_err());
    }
}
