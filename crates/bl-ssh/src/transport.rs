//! The binary packet protocol (RFC 4253 section 6) over TCP: the version
//! exchange and packets sent and received with the current
//! [`PacketCipher`] of each direction. Sending and receiving are separate
//! halves (on clones of the socket) so one thread can wait for packets
//! while others send.

use crate::cipher::PacketCipher;
use crate::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The identification this client sends.
pub(crate) const CLIENT_VERSION: &str = "SSH-2.0-Beatlumen_1.0";
/// Largest packet accepted (length field), as OpenSSH.
pub(crate) const MAX_PACKET: usize = 256 * 1024;
/// Packets with one set of keys before the sequence number (the
/// ChaCha20 nonce) would repeat.
const MAX_PACKETS_PER_KEY: u64 = u32::MAX as u64;

pub(crate) fn io_error(e: std::io::Error) -> Error {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            Error::Io("the server did not answer in time".into())
        }
        std::io::ErrorKind::UnexpectedEof => Error::Io("the server closed the connection".into()),
        _ => Error::Io(e.to_string()),
    }
}

/// Connects to the first reachable address of `host`.
pub(crate) fn connect(
    host: &str,
    port: u16,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<TcpStream, Error> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::Io(format!("cannot resolve {host}: {e}")))?;
    let mut last = Error::Io(format!("{host} has no address"));
    // A few addresses at most (IPv6 and IPv4 of a name), each with the timeout.
    for addr in addrs.take(4) {
        match TcpStream::connect_timeout(&addr, connect_timeout) {
            Ok(s) => {
                s.set_read_timeout(Some(io_timeout)).map_err(io_error)?;
                s.set_write_timeout(Some(io_timeout)).map_err(io_error)?;
                s.set_nodelay(true).map_err(io_error)?;
                return Ok(s);
            }
            Err(e) => last = Error::Io(format!("cannot connect to {host}:{port}: {e}")),
        }
    }
    Err(last)
}

pub(crate) struct SendHalf {
    sock: TcpStream,
    cipher: PacketCipher,
    pub seq: u32,
    with_keys: u64,
}

pub(crate) struct RecvHalf {
    sock: BufReader<TcpStream>,
    cipher: PacketCipher,
    pub seq: u32,
    with_keys: u64,
}

/// Splits a connected socket into its two halves.
pub(crate) fn split(sock: TcpStream) -> Result<(SendHalf, RecvHalf), Error> {
    let tx = SendHalf {
        sock: sock.try_clone().map_err(io_error)?,
        cipher: PacketCipher::None,
        seq: 0,
        with_keys: 0,
    };
    let rx = RecvHalf {
        sock: BufReader::with_capacity(64 * 1024, sock),
        cipher: PacketCipher::None,
        seq: 0,
        with_keys: 0,
    };
    Ok((tx, rx))
}

/// Sends our identification and reads the server's (without CR LF). The
/// server may send other lines first (RFC 4253 section 4.2).
pub(crate) fn exchange_versions(tx: &mut SendHalf, rx: &mut RecvHalf) -> Result<Vec<u8>, Error> {
    tx.sock
        .write_all(format!("{CLIENT_VERSION}\r\n").as_bytes())
        .map_err(io_error)?;
    for _ in 0..64 {
        let mut line = Vec::new();
        (&mut rx.sock)
            .take(256)
            .read_until(b'\n', &mut line)
            .map_err(io_error)?;
        if line.last() != Some(&b'\n') {
            return Err(if line.is_empty() {
                Error::Io("the server closed the connection".into())
            } else {
                Error::Protocol("not an SSH server".into())
            });
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.starts_with(b"SSH-") {
            if !(line.starts_with(b"SSH-2.0-") || line.starts_with(b"SSH-1.99-")) {
                return Err(Error::Protocol("the server does not speak SSH 2".into()));
            }
            if line.iter().any(|&b| !(32..127).contains(&b)) {
                return Err(Error::Protocol("invalid server identification".into()));
            }
            return Ok(line);
        }
    }
    Err(Error::Protocol("not an SSH server".into()))
}

impl SendHalf {
    /// Sends one packet with random padding.
    pub fn send(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() > MAX_PACKET - 64 {
            return Err(Error::Protocol("message too long".into()));
        }
        if self.with_keys >= MAX_PACKETS_PER_KEY {
            return Err(Error::Protocol("too many packets without new keys".into()));
        }
        let block = self.cipher.block_size();
        // AEAD ciphers leave the length field out of the alignment.
        let aligned = if self.cipher.aead() { 1 } else { 5 } + payload.len();
        let mut pad = block - aligned % block;
        if pad < 4 {
            pad += block;
        }
        let mut buf = Vec::with_capacity(5 + payload.len() + pad + self.cipher.tag_len());
        buf.extend_from_slice(&((1 + payload.len() + pad) as u32).to_be_bytes());
        buf.push(pad as u8);
        buf.extend_from_slice(payload);
        let mut padding = [0u8; 32];
        getrandom::getrandom(&mut padding[..pad])
            .map_err(|_| Error::Protocol("no random numbers available".into()))?;
        buf.extend_from_slice(&padding[..pad]);
        self.cipher.seal(self.seq, &mut buf)?;
        self.sock.write_all(&buf).map_err(io_error)?;
        self.seq = self.seq.wrapping_add(1);
        self.with_keys += 1;
        Ok(())
    }

    pub fn set_cipher(&mut self, cipher: PacketCipher) {
        self.cipher = cipher;
        self.with_keys = 0;
    }

    /// Closes the connection in both directions (wakes a blocked reader).
    pub fn shutdown(&self) {
        let _ = self.sock.shutdown(Shutdown::Both);
    }

    pub fn socket(&self) -> Result<TcpStream, Error> {
        self.sock.try_clone().map_err(io_error)
    }
}

impl RecvHalf {
    /// Receives one packet and returns its payload (never empty).
    pub fn recv(&mut self) -> Result<Vec<u8>, Error> {
        if self.with_keys >= MAX_PACKETS_PER_KEY {
            return Err(Error::Protocol("too many packets without new keys".into()));
        }
        let mut first = [0u8; 4];
        self.sock.read_exact(&mut first).map_err(io_error)?;
        let len = self.cipher.length(self.seq, first) as usize;
        let block = self.cipher.block_size();
        // AEAD: the length itself is aligned; otherwise length field + rest.
        let aligned = if self.cipher.aead() { len } else { len + 4 };
        if !(5..=MAX_PACKET).contains(&len) || aligned % block != 0 {
            return Err(Error::Protocol("invalid packet length".into()));
        }
        let mut buf = vec![0u8; 4 + len + self.cipher.tag_len()];
        buf[..4].copy_from_slice(&first);
        self.sock.read_exact(&mut buf[4..]).map_err(io_error)?;
        self.cipher.open(self.seq, &mut buf)?;
        let pad = buf[4] as usize;
        if pad < 4 || pad + 1 >= len {
            return Err(Error::Protocol("invalid padding".into()));
        }
        self.seq = self.seq.wrapping_add(1);
        self.with_keys += 1;
        buf.truncate(4 + len - pad);
        buf.drain(..5);
        Ok(buf)
    }

    pub fn set_cipher(&mut self, cipher: PacketCipher) {
        self.cipher = cipher;
        self.with_keys = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cipher::Cipher;
    use std::net::TcpListener;

    fn pair() -> ((SendHalf, RecvHalf), (SendHalf, RecvHalf)) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (b, _) = l.accept().unwrap();
        (split(a).unwrap(), split(b).unwrap())
    }

    /// Packets of all sizes through each cipher, and the version lines.
    #[test]
    fn packets_round_trip() {
        let ((mut tx, _), (_, mut rx)) = pair();
        for kind in [
            None,
            Some(Cipher::ChaCha20Poly1305),
            Some(Cipher::Aes256Gcm),
        ] {
            if let Some(k) = kind {
                let key = vec![7u8; k.key_len()];
                let iv = vec![1u8; k.iv_len()];
                tx.set_cipher(PacketCipher::new(k, &key, &iv));
                rx.set_cipher(PacketCipher::new(k, &key, &iv));
            }
            for n in [1usize, 2, 3, 7, 8, 15, 16, 17, 100, 4096, 65_000] {
                let payload: Vec<u8> = (0..n).map(|i| (i * 31 + n) as u8).collect();
                tx.send(&payload).unwrap();
                assert_eq!(rx.recv().unwrap(), payload, "{kind:?} {n}");
            }
        }
        assert_eq!(tx.seq, rx.seq);
    }

    #[test]
    fn version_lines() {
        let ((mut tx, mut rx), (mut stx, mut srx)) = pair();
        stx.sock
            .write_all(b"Welcome\r\nSSH-2.0-OpenSSH_9.6 test\r\n")
            .unwrap();
        assert_eq!(
            exchange_versions(&mut tx, &mut rx).unwrap(),
            b"SSH-2.0-OpenSSH_9.6 test"
        );
        let mut line = String::new();
        srx.sock.read_line(&mut line).unwrap();
        assert_eq!(line, "SSH-2.0-Beatlumen_1.0\r\n");
    }

    #[test]
    fn garbage_is_rejected() {
        let ((_, mut rx), (mut stx, _)) = pair();
        // A length of 1 GiB must not be allocated.
        stx.sock.write_all(&[0x40, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(rx.recv().is_err());
        let ((_, mut rx), (mut stx, _)) = pair();
        stx.sock.write_all(&[0, 0, 0, 12, 20, 1, 2, 3]).unwrap();
        stx.sock.write_all(&[0; 8]).unwrap();
        // Padding length 20 exceeds the packet.
        assert!(rx.recv().is_err());
    }
}
