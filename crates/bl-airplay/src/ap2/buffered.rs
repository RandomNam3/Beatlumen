//! Buffered audio (AirPlay 2 stream type 103): the sender connects to a
//! TCP data port and sends audio well ahead of time, as packets of a
//! two-byte big-endian length (counting itself), a 12-byte RTP header,
//! the ChaCha20-Poly1305 encrypted payload with its tag (associated data:
//! header bytes 4..12) and the 8-byte nonce. The payload is one AAC-LC
//! (or ALAC) frame; decoded audio goes into the [`LiveStream`]. When the
//! stream holds enough, reading stops and TCP flow control holds the
//! sender back.

use crate::crypto::chacha::open_in_place;
use crate::stream::{Format, LiveStream};
use std::io::{self, Read};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Audio kept ahead in the stream before reading pauses.
const AHEAD_SECS: f64 = 4.0;
/// Largest packet accepted.
const MAX_PACKET: usize = 16 * 1024;

/// Audio encodings of buffered streams.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    AacLc { rate: u32, channels: u16 },
    Alac { rate: u32, bits: u8, channels: u16 },
}

impl Codec {
    /// The SETUP stream's `audioFormat` bit (the same bits as the
    /// `bufferStream` formats announced in /info).
    pub fn from_audio_format(bits: u64) -> Option<Codec> {
        Some(match bits {
            0x40000 => Codec::Alac {
                rate: 44100,
                bits: 16,
                channels: 2,
            },
            0x200000 => Codec::Alac {
                rate: 48000,
                bits: 24,
                channels: 2,
            },
            0x400000 => Codec::AacLc {
                rate: 44100,
                channels: 2,
            },
            0x800000 => Codec::AacLc {
                rate: 48000,
                channels: 2,
            },
            _ => return None,
        })
    }

    /// The packets' SSRC names their encoding.
    fn from_ssrc(ssrc: u32) -> Option<Codec> {
        Some(match ssrc {
            0x1500_0000 => Codec::Alac {
                rate: 48000,
                bits: 24,
                channels: 2,
            },
            0x1600_0000 => Codec::AacLc {
                rate: 44100,
                channels: 2,
            },
            0x1700_0000 => Codec::AacLc {
                rate: 48000,
                channels: 2,
            },
            _ => return None,
        })
    }

    pub fn format(&self) -> Format {
        match *self {
            Codec::AacLc { rate, channels } => Format {
                sample_rate: rate,
                channels,
                frames_per_packet: 1024,
            },
            Codec::Alac { rate, channels, .. } => Format {
                sample_rate: rate,
                channels,
                frames_per_packet: 352,
            },
        }
    }

    fn decoder(&self) -> Option<Decoder> {
        Some(match *self {
            Codec::AacLc { rate, channels } => {
                let sfi = bl_aac::SAMPLE_RATES.iter().position(|&r| r == rate)? as u16;
                // AudioSpecificConfig: object type 2 (LC), frequency index,
                // channel configuration.
                let asc = (2u16 << 11 | sfi << 7 | channels << 3).to_be_bytes();
                Decoder::Aac(Box::new(bl_aac::AacDecoder::new(&asc).ok()?))
            }
            Codec::Alac {
                rate,
                bits,
                channels,
            } => Decoder::Alac(
                bl_media::AlacDecoder::new(&alac_cookie(352, bits, channels, rate)).ok()?,
            ),
        })
    }
}

/// The `ALACSpecificConfig` of AirPlay streams (the encoder's usual
/// tuning parameters; the format comes from the stream setup).
pub fn alac_cookie(frames: u32, bits: u8, channels: u16, rate: u32) -> Vec<u8> {
    let mut cookie = Vec::with_capacity(24);
    cookie.extend_from_slice(&frames.to_be_bytes());
    cookie.extend_from_slice(&[0, bits, 40, 10, 14, channels as u8]);
    cookie.extend_from_slice(&255u16.to_be_bytes());
    cookie.extend_from_slice(&0u32.to_be_bytes());
    cookie.extend_from_slice(&0u32.to_be_bytes());
    cookie.extend_from_slice(&rate.to_be_bytes());
    cookie
}

enum Decoder {
    Aac(Box<bl_aac::AacDecoder>),
    Alac(bl_media::AlacDecoder),
}

impl Decoder {
    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> bool {
        match self {
            Decoder::Aac(d) => d.decode_frame(data, out).is_ok(),
            Decoder::Alac(d) => d.decode_packet(data, out).is_ok(),
        }
    }
}

/// Packets to drop (FLUSHBUFFERED), by 23-bit sequence number.
#[derive(Default)]
struct Flush {
    /// Everything before this sequence number.
    until: Option<u32>,
    /// Ranges `[from, until)` announced ahead of time.
    ranges: Vec<(u32, u32)>,
}

/// `a - b` in the 23-bit sequence space, as a signed distance.
fn seq_diff(a: u32, b: u32) -> i32 {
    let d = a.wrapping_sub(b) & 0x7f_ffff;
    if d >= 0x40_0000 {
        d as i32 - 0x80_0000
    } else {
        d as i32
    }
}

impl Flush {
    /// Whether the packet `seq` is dropped.
    fn drops(&mut self, seq: u32) -> bool {
        if let Some(u) = self.until {
            if seq_diff(seq, u) < 0 {
                return true;
            }
            self.until = None;
        }
        self.ranges.retain(|&(_, until)| seq_diff(seq, until) < 0);
        self.ranges
            .iter()
            .any(|&(from, until)| seq_diff(seq, from) >= 0 && seq_diff(seq, until) < 0)
    }
}

/// A running data connection.
pub struct Buffered {
    pub port: u16,
    stop: Arc<AtomicBool>,
    flush: Arc<Mutex<Flush>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Buffered {
    /// Listens for the sender `peer`; packets are decrypted with `key`
    /// and decoded as `codec` (unless their SSRC names another encoding
    /// of the same format).
    pub fn start(
        key: [u8; 32],
        codec: Codec,
        stream: LiveStream,
        generation: u64,
        peer: IpAddr,
    ) -> io::Result<Buffered> {
        let listener = TcpListener::bind(if peer.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        })?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flush = Arc::new(Mutex::new(Flush::default()));
        let worker = Worker {
            key,
            codec,
            stream,
            generation,
            stop: stop.clone(),
            flush: flush.clone(),
            decoder: None,
        };
        let thread = std::thread::Builder::new()
            .name("bl-airplay-buffered".into())
            .spawn(move || worker.run(listener, peer))?;
        Ok(Buffered {
            port,
            stop,
            flush,
            thread: Some(thread),
        })
    }

    /// Drops packets before `until` (now), or in `[from, until)` when
    /// `from` is given (later).
    pub fn flush(&self, from: Option<u32>, until: u32) {
        let mut f = self.flush.lock().unwrap_or_else(|e| e.into_inner());
        match from {
            None => {
                f.until = Some(until & 0x7f_ffff);
                f.ranges.clear();
            }
            Some(from) => {
                if f.ranges.len() < 16 {
                    f.ranges.push((from & 0x7f_ffff, until & 0x7f_ffff));
                }
            }
        }
    }
}

impl Drop for Buffered {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    key: [u8; 32],
    codec: Codec,
    stream: LiveStream,
    generation: u64,
    stop: Arc<AtomicBool>,
    flush: Arc<Mutex<Flush>>,
    decoder: Option<(Codec, Decoder)>,
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn run(mut self, listener: TcpListener, peer: IpAddr) {
        let conn = loop {
            if self.stopped() {
                return;
            }
            match listener.accept() {
                Ok((s, from)) if from.ip() == peer => break s,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        drop(listener);
        let _ = conn.set_nonblocking(false);
        let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
        let mut conn = conn;
        let mut packet = vec![0u8; MAX_PACKET];
        loop {
            // Enough audio ahead: let the sender wait.
            loop {
                if self.stopped() {
                    return;
                }
                match self.stream.buffered_secs(self.generation) {
                    None => return,
                    Some(s) if s > AHEAD_SECS => std::thread::sleep(Duration::from_millis(20)),
                    Some(_) => break,
                }
            }
            let mut len = [0u8; 2];
            if !self.read_full(&mut conn, &mut len) {
                return;
            }
            let len = (u16::from_be_bytes(len) as usize).saturating_sub(2);
            if len > MAX_PACKET {
                return;
            }
            if !self.read_full(&mut conn, &mut packet[..len]) {
                return;
            }
            self.packet(&mut packet[..len]);
        }
    }

    /// Reads exactly `buf.len()` bytes; false on end of stream, error or
    /// stop.
    fn read_full(&self, conn: &mut TcpStream, buf: &mut [u8]) -> bool {
        let mut pos = 0;
        while pos < buf.len() {
            if self.stopped() {
                return false;
            }
            match conn.read(&mut buf[pos..]) {
                Ok(0) => return false,
                Ok(n) => pos += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }

    fn packet(&mut self, p: &mut [u8]) {
        if p.len() < 12 + 16 + 8 {
            return;
        }
        let seq = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7f_ffff;
        let ts = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
        let ssrc = u32::from_be_bytes([p[8], p[9], p[10], p[11]]);
        if ssrc == 0 {
            return; // filler without audio
        }
        if self
            .flush
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drops(seq)
        {
            return;
        }
        let n = p.len();
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&p[n - 8..]);
        let aad: [u8; 8] = p[4..12].try_into().expect("8 bytes");
        let (body, rest) = p[12..n - 8].split_at_mut(n - 8 - 12 - 16);
        if !open_in_place(&self.key, &nonce, &aad, body, rest) {
            return;
        }
        let codec = Codec::from_ssrc(ssrc)
            .filter(|c| c.format().sample_rate == self.codec.format().sample_rate)
            .unwrap_or(self.codec);
        if self.decoder.as_ref().map(|(c, _)| *c) != Some(codec) {
            self.decoder = codec.decoder().map(|d| (codec, d));
        }
        let Some((_, dec)) = &mut self.decoder else {
            return;
        };
        let mut pcm = Vec::with_capacity(2048 * 2);
        if dec.decode(body, &mut pcm) {
            self.stream.push(self.generation, ts, pcm);
        }
    }
}

/// Builds packets as senders do (for the tests' sender).
#[cfg(test)]
pub fn encode_packet(
    key: &[u8; 32],
    seq: u32,
    ts: u32,
    ssrc: u32,
    frame: &[u8],
    nonce: u64,
) -> Vec<u8> {
    use crate::crypto::chacha::seal_in_place;
    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(&(0x8060_0000 | seq & 0x7f_ffff).to_be_bytes());
    header[4..8].copy_from_slice(&ts.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce.to_le_bytes());
    let mut body = frame.to_vec();
    let tag = seal_in_place(key, &n, &header[4..12], &mut body);
    let len = 2 + 12 + body.len() + 16 + 8;
    let mut out = (len as u16).to_be_bytes().to_vec();
    out.extend_from_slice(&header);
    out.extend_from_slice(&body);
    out.extend_from_slice(&tag);
    out.extend_from_slice(&n[4..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_flushes() {
        let mut f = Flush {
            until: Some(100),
            ..Default::default()
        };
        assert!(f.drops(99));
        assert!(!f.drops(100));
        assert!(!f.drops(50)); // the flush is over
        let mut f = Flush {
            ranges: vec![(200, 210)],
            ..Default::default()
        };
        assert!(!f.drops(199));
        assert!(f.drops(200) && f.drops(209));
        assert!(!f.drops(210));
        assert!(f.ranges.is_empty());
        // Wrap-around of the 23-bit space.
        let mut f = Flush {
            until: Some(5),
            ..Default::default()
        };
        assert!(f.drops(0x7f_fff0));
        assert!(!f.drops(6));
    }

    #[test]
    fn codecs() {
        assert_eq!(
            Codec::from_audio_format(0x400000),
            Some(Codec::AacLc {
                rate: 44100,
                channels: 2
            })
        );
        assert!(Codec::from_audio_format(1).is_none());
        for bits in [0x40000, 0x200000, 0x400000, 0x800000] {
            assert!(Codec::from_audio_format(bits).unwrap().decoder().is_some());
        }
        assert_eq!(
            Codec::from_ssrc(0x1700_0000).unwrap().format().sample_rate,
            48000
        );
    }

    #[test]
    fn packets_decrypt_and_reject_tampering() {
        let key = [1u8; 32];
        let stream = LiveStream::new();
        let codec = Codec::AacLc {
            rate: 44100,
            channels: 2,
        };
        let generation = stream.begin(codec.format());
        let mut w = Worker {
            key,
            codec,
            stream: stream.clone(),
            generation,
            stop: Arc::new(AtomicBool::new(false)),
            flush: Arc::new(Mutex::new(Flush::default())),
            decoder: None,
        };
        // A silent AAC-LC stereo frame (as ffmpeg encodes silence).
        let silent = [0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c];
        let p = encode_packet(&key, 1, 1000, 0x1600_0000, &silent, 7);
        let mut bad = p.clone();
        bad[20] ^= 1;
        w.packet(&mut bad[2..]);
        assert_eq!(stream.buffered_secs(generation), Some(0.0));
        let mut good = p.clone();
        w.packet(&mut good[2..]);
        let secs = stream.buffered_secs(generation).unwrap();
        assert!((secs - 1024.0 / 44100.0).abs() < 1e-6, "{secs}");
    }
}
