//! The UDP side of a stream: audio packets (decrypted and decoded into the
//! [`LiveStream`]), sync packets on the control port, requests for lost
//! packets, and the timing port. AirPlay 2 realtime streams use the same
//! packets with ChaCha20-Poly1305 instead of AES.

use crate::crypto::aes::Aes128;
use crate::crypto::chacha::open_in_place;
use crate::meta::{Codec, StreamFormat};
use crate::stream::{Format, LiveStream};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Longest gap of lost packets that is asked for again.
const MAX_RESEND: u16 = 128;

pub struct Session {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Generation of the stream in the [`LiveStream`].
    pub generation: u64,
    pub server_port: u16,
    pub control_port: u16,
    pub timing_port: u16,
}

/// Turns RTP payloads into PCM.
struct PacketDecoder {
    aes: Option<(Aes128, [u8; 16])>,
    /// AirPlay 2: the packet's payload is sealed with this key (nonce: the
    /// last 8 bytes, associated data: header bytes 4..12).
    chacha: Option<[u8; 32]>,
    alac: Option<bl_media::AlacDecoder>,
    channels: usize,
    buf: Vec<u8>,
}

impl PacketDecoder {
    /// Decodes an RTP packet (12-byte header and payload).
    fn decode(&mut self, packet: &[u8]) -> Option<Vec<f32>> {
        self.buf.clear();
        if let Some(key) = &self.chacha {
            let n = packet.len();
            if n < 12 + 16 + 8 {
                return None;
            }
            let mut nonce = [0u8; 12];
            nonce[4..].copy_from_slice(&packet[n - 8..]);
            self.buf.extend_from_slice(&packet[12..n - 8 - 16]);
            if !open_in_place(
                key,
                &nonce,
                &packet[4..12],
                &mut self.buf,
                &packet[n - 24..n - 8],
            ) {
                return None;
            }
        } else {
            self.buf.extend_from_slice(&packet[12..]);
        }
        if let Some((aes, iv)) = &self.aes {
            aes.decrypt_cbc(iv, &mut self.buf);
        }
        let mut pcm = Vec::new();
        match &mut self.alac {
            Some(dec) => {
                dec.decode_packet(&self.buf, &mut pcm).ok()?;
            }
            None => {
                // L16: big-endian 16-bit samples.
                let usable = self.buf.len() / (2 * self.channels) * 2 * self.channels;
                pcm.extend(
                    self.buf[..usable]
                        .chunks_exact(2)
                        .map(|s| i16::from_be_bytes([s[0], s[1]]) as f32 / 32768.0),
                );
            }
        }
        Some(pcm)
    }
}

/// State shared by the audio and control threads.
struct Shared {
    decoder: Mutex<PacketDecoder>,
    stream: LiveStream,
    generation: u64,
    /// Sequence number expected next.
    expected: Mutex<Option<u16>>,
    control: UdpSocket,
    /// Where to send requests for lost packets (the sender's control port).
    peer_control: Option<SocketAddr>,
}

impl Shared {
    /// Handles an RTP audio packet (12-byte header + payload).
    fn audio(&self, packet: &[u8], resent: bool) {
        if packet.len() < 12 + 4 {
            return;
        }
        let seq = u16::from_be_bytes([packet[2], packet[3]]);
        let ts = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        if !resent {
            self.track_sequence(seq);
        }
        let pcm = match self.decoder.lock() {
            Ok(mut d) => d.decode(packet),
            Err(_) => None,
        };
        if let Some(pcm) = pcm {
            self.stream.push(self.generation, ts, pcm);
        }
    }

    /// Notices gaps in the sequence numbers and asks for the lost packets.
    fn track_sequence(&self, seq: u16) {
        let Ok(mut expected) = self.expected.lock() else {
            return;
        };
        if let Some(exp) = *expected {
            let gap = seq.wrapping_sub(exp);
            if gap == 0 {
                *expected = Some(seq.wrapping_add(1));
            } else if gap < 0x8000 {
                if gap <= MAX_RESEND {
                    self.request_resend(exp, gap);
                }
                *expected = Some(seq.wrapping_add(1));
            }
            // else: an old packet arriving late; keep waiting for `exp`.
        } else {
            *expected = Some(seq.wrapping_add(1));
        }
    }

    fn request_resend(&self, first: u16, count: u16) {
        let Some(peer) = self.peer_control else {
            return;
        };
        let mut req = [0u8; 8];
        req[0] = 0x80;
        req[1] = 0x55 | 0x80;
        req[2..4].copy_from_slice(&1u16.to_be_bytes());
        req[4..6].copy_from_slice(&first.to_be_bytes());
        req[6..8].copy_from_slice(&count.to_be_bytes());
        let _ = self.control.send_to(&req, peer);
    }

    fn control_packet(&self, packet: &[u8], received: Instant) {
        if packet.len() < 4 {
            return;
        }
        match packet[1] {
            // Sync (AirPlay 2: with the sender's PTP time): the frame "now
            // minus latency" is heard now.
            0xd4 | 0xd7 if packet.len() >= 20 => {
                let frame = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
                self.stream.sync(self.generation, frame, received);
            }
            // Resent audio packet (wrapped in a 4-byte header).
            0xd6 => self.audio(&packet[4..], true),
            _ => {}
        }
    }
}

impl Session {
    /// Opens the UDP ports for a stream from `peer`; `peer_control_port`
    /// is where lost packets are requested; `chacha` is the key of AirPlay 2
    /// realtime streams.
    pub fn start(
        format: &StreamFormat,
        stream: LiveStream,
        peer: IpAddr,
        peer_control_port: Option<u16>,
        chacha: Option<[u8; 32]>,
    ) -> std::io::Result<Session> {
        let bind = |_: ()| -> std::io::Result<UdpSocket> {
            let s = UdpSocket::bind(if peer.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            })?;
            s.set_read_timeout(Some(Duration::from_millis(200)))?;
            Ok(s)
        };
        let audio = bind(())?;
        let control = bind(())?;
        let timing = bind(())?;
        let alac = match &format.codec {
            Codec::Alac(cookie) => Some(
                bl_media::AlacDecoder::new(cookie)
                    .map_err(|e| std::io::Error::other(e.to_string()))?,
            ),
            Codec::L16 => None,
        };
        let generation = stream.begin(Format {
            sample_rate: format.sample_rate,
            channels: format.channels,
            frames_per_packet: format.frames_per_packet,
        });
        let shared = Arc::new(Shared {
            decoder: Mutex::new(PacketDecoder {
                aes: format.aes.map(|(k, iv)| (Aes128::new(&k), iv)),
                chacha,
                alac,
                channels: format.channels.max(1) as usize,
                buf: Vec::with_capacity(2048),
            }),
            stream,
            generation,
            expected: Mutex::new(None),
            control: control.try_clone()?,
            peer_control: peer_control_port.map(|p| SocketAddr::new(peer, p)),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let ports = (
            audio.local_addr()?.port(),
            control.local_addr()?.port(),
            timing.local_addr()?.port(),
        );
        let mut threads = Vec::new();
        {
            let (shared, stop) = (shared.clone(), stop.clone());
            threads.push(spawn("bl-airplay-audio", move || {
                let mut buf = [0u8; 2048];
                while !stop.load(Ordering::Relaxed) {
                    let Ok((n, from)) = audio.recv_from(&mut buf) else {
                        continue;
                    };
                    if from.ip() != peer || n < 12 {
                        continue;
                    }
                    match buf[1] & 0x7f {
                        0x60 => shared.audio(&buf[..n], false),
                        0x56 if n > 16 => shared.audio(&buf[4..n], true),
                        _ => {}
                    }
                }
            }));
        }
        {
            let (shared, stop) = (shared.clone(), stop.clone());
            threads.push(spawn("bl-airplay-control", move || {
                let mut buf = [0u8; 2048];
                while !stop.load(Ordering::Relaxed) {
                    if let Ok((n, from)) = control.recv_from(&mut buf) {
                        if from.ip() == peer {
                            shared.control_packet(&buf[..n], Instant::now());
                        }
                    }
                }
            }));
        }
        {
            let stop = stop.clone();
            threads.push(spawn("bl-airplay-timing", move || {
                // Senders may expect the port to exist; nothing to answer.
                let mut buf = [0u8; 256];
                while !stop.load(Ordering::Relaxed) {
                    let _ = timing.recv_from(&mut buf);
                }
            }));
        }
        Ok(Session {
            stop,
            threads,
            generation,
            server_port: ports.0,
            control_port: ports.1,
            timing_port: ports.2,
        })
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn thread")
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
