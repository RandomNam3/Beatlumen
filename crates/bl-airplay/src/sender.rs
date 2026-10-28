//! AirPlay 1 (RAOP) sender: streams 44.1 kHz stereo audio to a receiver
//! (AirPort Express, AV receivers, shairport-sync, Apple TV, …).
//!
//! The RTSP connection sets up the stream (ANNOUNCE, SETUP, RECORD) and
//! carries volume, track information and flushes. Audio goes over UDP as
//! RTP packets of 352 frames in uncompressed ALAC framing, AES-encrypted
//! when the receiver supports it (the stream key is wrapped with the
//! protocol's RSA key). Sync packets tell the receiver which frame to play
//! when: frame `rtp_now - latency` now, so everything sent is heard
//! [`LATENCY`] frames later. The receiver asks for lost packets on the
//! control port and for the time on the timing port.

use crate::crypto::{self, aes::Aes128};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SAMPLE_RATE: u32 = 44100;
/// Frames per RTP packet.
pub const FRAMES_PER_PACKET: usize = 352;
/// Frames between sending and hearing them (2 s, as iTunes).
pub const LATENCY: u32 = 88200;
/// Packets kept for resending.
const HISTORY: usize = 1000;
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The receiver's password (when its TXT record says pw=true).
    pub password: Option<String>,
    /// Encrypt the audio when the receiver supports it ("et" contains 1).
    pub encrypt: bool,
    /// Shown by some receivers as the sender's name.
    pub name: String,
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The receiver answered with this status.
    Status(&'static str, u16),
    Protocol(&'static str),
    /// A password is needed or was wrong.
    Auth,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Status(m, s) => write!(f, "{m}: the receiver answered {s}"),
            Error::Protocol(m) => write!(f, "{m}"),
            Error::Auth => write!(f, "password required or wrong"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The RTSP control connection.
struct Rtsp {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    cseq: u32,
    url: String,
    session: Option<String>,
    password: Option<String>,
    /// realm, nonce once the receiver asked for a password.
    digest: Option<(String, String)>,
    client_id: String,
    active_remote: u32,
    name: String,
}

impl Rtsp {
    fn request(
        &mut self,
        method: &str,
        extra: &[(&str, String)],
        body: &[u8],
    ) -> Result<Response, Error> {
        let mut retried = false;
        loop {
            self.cseq += 1;
            let uri = if method == "OPTIONS" {
                "*".to_string()
            } else {
                self.url.clone()
            };
            let mut head = format!(
                "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: Beatlumen/1.0\r\nClient-Instance: {}\r\nDACP-ID: {}\r\nActive-Remote: {}\r\n",
                self.cseq, self.client_id, self.client_id, self.active_remote
            );
            if !self.name.is_empty() {
                head.push_str(&format!(
                    "X-Apple-Client-Name: {}\r\n",
                    self.name.replace(['\r', '\n'], " ")
                ));
            }
            if let Some(s) = &self.session {
                head.push_str(&format!("Session: {s}\r\n"));
            }
            if let (Some((realm, nonce)), Some(pw)) = (&self.digest, &self.password) {
                let md5 = |s: String| crypto::hash::hex(&crypto::hash::md5(s.as_bytes()));
                let ha1 = md5(format!("iTunes:{realm}:{pw}"));
                let ha2 = md5(format!("{method}:{uri}"));
                let resp = md5(format!("{ha1}:{nonce}:{ha2}"));
                head.push_str(&format!(
                    "Authorization: Digest username=\"iTunes\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{resp}\"\r\n"
                ));
            }
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            if !body.is_empty() {
                head.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            head.push_str("\r\n");
            let mut msg = head.into_bytes();
            msg.extend_from_slice(body);
            self.writer.write_all(&msg)?;
            let r = self.read_response()?;
            if r.status == 401 {
                let www = r.header("WWW-Authenticate").unwrap_or("").to_string();
                if retried || self.password.is_none() {
                    return Err(Error::Auth);
                }
                let field = |k| crate::rtsp::digest_field(&www, k).unwrap_or_default();
                self.digest = Some((field("realm"), field("nonce")));
                retried = true;
                continue;
            }
            return Ok(r);
        }
    }

    fn read_response(&mut self) -> Result<Response, Error> {
        let mut status = 0;
        let mut headers = Vec::new();
        let mut first = true;
        let mut total = 0usize;
        loop {
            let mut line = String::new();
            let n = (&mut self.reader).take(8192).read_line(&mut line)?;
            if n == 0 {
                return Err(Error::Protocol("the receiver closed the connection"));
            }
            total += n;
            if total > 64 * 1024 {
                return Err(Error::Protocol("response too large"));
            }
            let line = line.trim_end();
            if first {
                if line.is_empty() {
                    continue;
                }
                first = false;
                status = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .ok_or(Error::Protocol("bad status line"))?;
                continue;
            }
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let r = Response { status, headers };
        let len: usize = r
            .header("Content-Length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if len > crate::rtsp::MAX_BODY {
            return Err(Error::Protocol("response too large"));
        }
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body)?;
        Ok(r)
    }
}

/// Seconds since 1900 as NTP 32.32 fixed point.
fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() + 2_208_988_800;
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    secs << 32 | frac
}

/// RTP time of the sender's clock: `anchor_rtp` at `anchor`.
#[derive(Clone, Copy)]
struct Clock {
    anchor: Instant,
    anchor_rtp: u32,
}

impl Clock {
    fn rtp_at(&self, t: Instant) -> u32 {
        let frames = t.saturating_duration_since(self.anchor).as_secs_f64() * SAMPLE_RATE as f64;
        self.anchor_rtp.wrapping_add(frames as u64 as u32)
    }
}

/// State shared with the control and timing threads.
struct Shared {
    audio: UdpSocket,
    control: UdpSocket,
    peer_control: SocketAddr,
    aes: Option<(Aes128, [u8; 16])>,
    history: Mutex<VecDeque<(u16, Vec<u8>)>>,
    clock: Mutex<Clock>,
    /// Audio is flowing (sync packets are sent).
    running: AtomicBool,
    /// The next sync packet is the first after a (re)start.
    first_sync: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn send_sync(&self) {
        let rtp_now = self
            .clock
            .lock()
            .map(|c| c.rtp_at(Instant::now()))
            .unwrap_or(0);
        let first = self.first_sync.swap(false, Ordering::Relaxed);
        let mut p = [0u8; 20];
        p[0] = if first { 0x90 } else { 0x80 };
        p[1] = 0xd4;
        p[2..4].copy_from_slice(&7u16.to_be_bytes());
        p[4..8].copy_from_slice(&rtp_now.wrapping_sub(LATENCY).to_be_bytes());
        p[8..16].copy_from_slice(&ntp_now().to_be_bytes());
        p[16..20].copy_from_slice(&rtp_now.to_be_bytes());
        let _ = self.control.send_to(&p, self.peer_control);
    }

    fn resend(&self, first: u16, count: u16) {
        let Ok(h) = self.history.lock() else { return };
        for i in 0..count.min(256) {
            let seq = first.wrapping_add(i);
            if let Some((_, packet)) = h.iter().find(|(s, _)| *s == seq) {
                let mut p = vec![0x80, 0xd6, 0x00, 0x01];
                p.extend_from_slice(packet);
                let _ = self.control.send_to(&p, self.peer_control);
            }
        }
    }
}

/// A stream to one receiver. Dropping it ends the stream (TEARDOWN).
pub struct Sender {
    rtsp: Mutex<Rtsp>,
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
    seq: u16,
    next_rtp: u32,
    pending: Vec<i16>,
    ssrc: u32,
    first_packet: bool,
    /// Extra latency the receiver reported (Audio-Latency), in frames.
    pub receiver_latency: u32,
    last_keepalive: Instant,
}

fn random_u32() -> u32 {
    let b = crypto::random_bytes(4);
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl Sender {
    /// Connects to the receiver at `addr` and sets up a stream. `et` is the
    /// TXT record's list of supported encryptions ("0,1,…").
    pub fn connect(addr: SocketAddr, et: &str, opts: &Options) -> Result<Sender, Error> {
        let stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?.ip();
        let sid = random_u32();
        let ip_kind = if local.is_ipv6() { "IP6" } else { "IP4" };
        let mut rtsp = Rtsp {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            cseq: 0,
            url: format!("rtsp://{local}/{sid}"),
            session: None,
            password: opts.password.clone().filter(|p| !p.is_empty()),
            digest: None,
            client_id: crypto::hash::hex(&crypto::random_bytes(8)).to_uppercase(),
            active_remote: random_u32(),
            name: opts.name.clone(),
        };
        let r = rtsp.request("OPTIONS", &[], &[])?;
        if r.status != 200 {
            return Err(Error::Status("OPTIONS", r.status));
        }

        let supported: Vec<&str> = et.split(',').map(str::trim).collect();
        let encrypt = opts.encrypt && supported.contains(&"1")
            || !supported.is_empty() && !supported.contains(&"0") && supported.contains(&"1");
        let key: [u8; 16] = crypto::random_bytes(16).try_into().expect("16 bytes");
        let iv: [u8; 16] = crypto::random_bytes(16).try_into().expect("16 bytes");
        let mut sdp = format!(
            "v=0\r\no=iTunes {sid} 0 IN {ip_kind} {local}\r\ns=iTunes\r\nc=IN {ip_kind} {}\r\nt=0 0\r\nm=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\na=fmtp:96 {FRAMES_PER_PACKET} 0 16 40 10 14 2 255 0 0 {SAMPLE_RATE}\r\n",
            addr.ip()
        );
        if encrypt {
            let wrapped =
                crypto::rsa_encrypt_oaep(&key).ok_or(Error::Protocol("key wrapping failed"))?;
            sdp.push_str(&format!(
                "a=rsaaeskey:{}\r\na=aesiv:{}\r\n",
                crypto::base64_encode(&wrapped),
                crypto::base64_encode(&iv)
            ));
        }
        let r = rtsp.request(
            "ANNOUNCE",
            &[("Content-Type", "application/sdp".into())],
            sdp.as_bytes(),
        )?;
        if r.status != 200 {
            return Err(Error::Status("ANNOUNCE", r.status));
        }

        let bind = if addr.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let control = UdpSocket::bind(bind)?;
        let timing = UdpSocket::bind(bind)?;
        control.set_read_timeout(Some(Duration::from_millis(200)))?;
        timing.set_read_timeout(Some(Duration::from_millis(200)))?;
        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
            control.local_addr()?.port(),
            timing.local_addr()?.port()
        );
        let r = rtsp.request("SETUP", &[("Transport", transport)], &[])?;
        if r.status != 200 {
            return Err(Error::Status("SETUP", r.status));
        }
        rtsp.session = r
            .header("Session")
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
        let tr = r.header("Transport").unwrap_or("").to_string();
        let port = |name: &str| -> Option<u16> {
            tr.split(';')
                .find_map(|p| p.trim().strip_prefix(name)?.strip_prefix('=')?.parse().ok())
        };
        let server_port = port("server_port").ok_or(Error::Protocol("no server port"))?;
        let control_port = port("control_port").unwrap_or(server_port + 1);
        let ip: IpAddr = addr.ip();

        let seq = random_u32() as u16;
        let rtp = random_u32();
        let r = rtsp.request(
            "RECORD",
            &[
                ("Range", "npt=0-".into()),
                ("RTP-Info", format!("seq={seq};rtptime={rtp}")),
            ],
            &[],
        )?;
        if r.status != 200 {
            return Err(Error::Status("RECORD", r.status));
        }
        let receiver_latency = r
            .header("Audio-Latency")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let audio = UdpSocket::bind(bind)?;
        audio.connect(SocketAddr::new(ip, server_port))?;
        let shared = Arc::new(Shared {
            audio,
            control,
            peer_control: SocketAddr::new(ip, control_port),
            aes: encrypt.then(|| (Aes128::new(&key), iv)),
            history: Mutex::new(VecDeque::with_capacity(HISTORY)),
            clock: Mutex::new(Clock {
                anchor: Instant::now(),
                anchor_rtp: rtp,
            }),
            running: AtomicBool::new(false),
            first_sync: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let mut threads = Vec::new();
        {
            let s = shared.clone();
            threads.push(spawn("bl-airplay-tx-control", move || {
                let mut buf = [0u8; 64];
                let mut last_sync = Instant::now() - Duration::from_secs(2);
                while !s.stop.load(Ordering::Relaxed) {
                    if s.running.load(Ordering::Relaxed)
                        && last_sync.elapsed() >= Duration::from_secs(1)
                    {
                        last_sync = Instant::now();
                        s.send_sync();
                    }
                    if let Ok((n, from)) = s.control.recv_from(&mut buf) {
                        // Lost packets: 0x80 0xd5, seq, first, count.
                        if from.ip() == s.peer_control.ip() && n >= 8 && buf[1] & 0x7f == 0x55 {
                            let first = u16::from_be_bytes([buf[4], buf[5]]);
                            let count = u16::from_be_bytes([buf[6], buf[7]]);
                            s.resend(first, count);
                        }
                    }
                }
            }));
        }
        {
            let s = shared.clone();
            threads.push(spawn("bl-airplay-tx-timing", move || {
                let mut buf = [0u8; 64];
                while !s.stop.load(Ordering::Relaxed) {
                    let Ok((n, from)) = timing.recv_from(&mut buf) else {
                        continue;
                    };
                    let received = ntp_now();
                    // Time request: 0x80 0xd2, …, transmit time at 24..32.
                    if from.ip() != ip || n < 32 || buf[1] & 0x7f != 0x52 {
                        continue;
                    }
                    let mut p = [0u8; 32];
                    p[0] = 0x80;
                    p[1] = 0xd3;
                    p[2..4].copy_from_slice(&7u16.to_be_bytes());
                    p[8..16].copy_from_slice(&buf[24..32]);
                    p[16..24].copy_from_slice(&received.to_be_bytes());
                    p[24..32].copy_from_slice(&ntp_now().to_be_bytes());
                    let _ = timing.send_to(&p, from);
                }
            }));
        }
        Ok(Sender {
            rtsp: Mutex::new(rtsp),
            shared,
            threads,
            seq,
            next_rtp: rtp,
            pending: Vec::with_capacity(FRAMES_PER_PACKET * 4),
            ssrc: random_u32(),
            first_packet: true,
            receiver_latency,
            last_keepalive: Instant::now(),
        })
    }

    /// Frames to send now so that the stream stays `lead` frames ahead of
    /// the clock (whole packets).
    pub fn frames_due(&self, lead: u32) -> usize {
        if !self.shared.running.load(Ordering::Relaxed) {
            return FRAMES_PER_PACKET * 2;
        }
        let now = self
            .shared
            .clock
            .lock()
            .map(|c| c.rtp_at(Instant::now()))
            .unwrap_or(0);
        let ahead = self.next_rtp.wrapping_sub(now) as i32;
        let due = lead as i64 - ahead as i64 - self.pending.len() as i64 / 2;
        if due <= 0 {
            0
        } else {
            (due as usize).div_ceil(FRAMES_PER_PACKET) * FRAMES_PER_PACKET
        }
    }

    /// Sends interleaved 16-bit stereo frames at 44.1 kHz.
    pub fn send(&mut self, pcm: &[i16]) -> Result<(), Error> {
        if !self.shared.running.swap(true, Ordering::Relaxed) {
            // (Re)start: the clock continues where the stream continues.
            if let Ok(mut c) = self.shared.clock.lock() {
                *c = Clock {
                    anchor: Instant::now(),
                    anchor_rtp: self.next_rtp,
                };
            }
            self.shared.first_sync.store(true, Ordering::Relaxed);
            self.shared.send_sync();
        }
        self.pending.extend_from_slice(pcm);
        let per_packet = FRAMES_PER_PACKET * 2;
        let mut sent = 0;
        while self.pending.len() - sent >= per_packet {
            let frame = alac_frame(&self.pending[sent..sent + per_packet]);
            sent += per_packet;
            self.send_packet(frame)?;
        }
        self.pending.drain(..sent);
        Ok(())
    }

    fn send_packet(&mut self, mut payload: Vec<u8>) -> Result<(), Error> {
        if let Some((aes, iv)) = &self.shared.aes {
            aes.encrypt_cbc(iv, &mut payload);
        }
        let mut p = Vec::with_capacity(12 + payload.len());
        p.push(0x80);
        p.push(if self.first_packet { 0xe0 } else { 0x60 });
        p.extend_from_slice(&self.seq.to_be_bytes());
        p.extend_from_slice(&self.next_rtp.to_be_bytes());
        p.extend_from_slice(&self.ssrc.to_be_bytes());
        p.extend_from_slice(&payload);
        self.first_packet = false;
        // A full socket buffer drops the packet; the receiver asks again.
        let _ = self.shared.audio.send(&p);
        if let Ok(mut h) = self.shared.history.lock() {
            if h.len() >= HISTORY {
                h.pop_front();
            }
            h.push_back((self.seq, p));
        }
        self.seq = self.seq.wrapping_add(1);
        self.next_rtp = self.next_rtp.wrapping_add(FRAMES_PER_PACKET as u32);
        Ok(())
    }

    /// Drops what the receiver has buffered (pause, seek); sending again
    /// restarts the clock.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.shared.running.store(false, Ordering::Relaxed);
        self.pending.clear();
        let info = format!("seq={};rtptime={}", self.seq, self.next_rtp);
        let r = self.rtsp_request("FLUSH", &[("RTP-Info", info)], &[])?;
        if r != 200 {
            return Err(Error::Status("FLUSH", r));
        }
        Ok(())
    }

    /// Volume 0..1 (0 mutes); the receiver's scale is -30..0 dB.
    pub fn set_volume(&mut self, v: f32) -> Result<(), Error> {
        let db = if v <= 0.001 {
            -144.0
        } else {
            -30.0 * (1.0 - v.clamp(0.0, 1.0))
        };
        let body = format!("volume: {db:.6}\r\n");
        self.rtsp_request(
            "SET_PARAMETER",
            &[("Content-Type", "text/parameters".into())],
            body.as_bytes(),
        )?;
        Ok(())
    }

    /// Title, artist and album of what plays now.
    pub fn set_metadata(&mut self, title: &str, artist: &str, album: &str) -> Result<(), Error> {
        fn item(tag: &[u8; 4], v: &[u8]) -> Vec<u8> {
            let mut out = tag.to_vec();
            out.extend_from_slice(&(v.len() as u32).to_be_bytes());
            out.extend_from_slice(v);
            out
        }
        let mut inner = item(b"minm", title.as_bytes());
        inner.extend(item(b"asar", artist.as_bytes()));
        inner.extend(item(b"asal", album.as_bytes()));
        let body = item(b"mlit", &inner);
        let info = format!("rtptime={}", self.next_rtp);
        self.rtsp_request(
            "SET_PARAMETER",
            &[
                ("Content-Type", "application/x-dmap-tagged".into()),
                ("RTP-Info", info),
            ],
            &body,
        )?;
        Ok(())
    }

    /// Cover art (JPEG or PNG).
    pub fn set_artwork(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() || data.len() > crate::rtsp::MAX_BODY {
            return Ok(());
        }
        let ct = if data.starts_with(b"\x89PNG") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let info = format!("rtptime={}", self.next_rtp);
        self.rtsp_request(
            "SET_PARAMETER",
            &[("Content-Type", ct.into()), ("RTP-Info", info)],
            data,
        )?;
        Ok(())
    }

    /// Progress of the current track (seconds), shown by some receivers.
    pub fn set_progress(&mut self, position: f64, duration: f64) -> Result<(), Error> {
        let now = self.next_rtp;
        let start = now.wrapping_sub((position.max(0.0) * SAMPLE_RATE as f64) as u32);
        let end = start.wrapping_add((duration.max(0.0) * SAMPLE_RATE as f64) as u32);
        let body = format!("progress: {start}/{now}/{end}\r\n");
        self.rtsp_request(
            "SET_PARAMETER",
            &[("Content-Type", "text/parameters".into())],
            body.as_bytes(),
        )?;
        Ok(())
    }

    /// Keeps the control connection alive (call now and then).
    pub fn keepalive(&mut self) -> Result<(), Error> {
        if self.last_keepalive.elapsed() < Duration::from_secs(25) {
            return Ok(());
        }
        self.last_keepalive = Instant::now();
        self.rtsp_request("OPTIONS", &[], &[])?;
        Ok(())
    }

    fn rtsp_request(
        &mut self,
        method: &str,
        extra: &[(&str, String)],
        body: &[u8],
    ) -> Result<u16, Error> {
        self.last_keepalive = Instant::now();
        let mut r = self.rtsp.lock().map_err(|_| Error::Protocol("poisoned"))?;
        Ok(r.request(method, extra, body)?.status)
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if let Ok(mut r) = self.rtsp.lock() {
            let _ = r.writer.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = r.request("TEARDOWN", &[], &[]);
        }
        self.shared.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn thread")
}

/// An uncompressed ("escape") ALAC frame of interleaved 16-bit stereo,
/// which every ALAC decoder accepts.
pub fn alac_frame(samples: &[i16]) -> Vec<u8> {
    let frames = samples.len() / 2;
    let mut w = BitWriter::default();
    w.put(1, 3); // stereo channel pair
    w.put(0, 4); // element instance tag
    w.put(0, 12);
    let partial = frames != FRAMES_PER_PACKET;
    w.put(partial as u32, 1); // explicit sample count
    w.put(0, 2); // bytes shifted
    w.put(1, 1); // escape: uncompressed
    if partial {
        w.put(frames as u32, 32);
    }
    for &s in &samples[..frames * 2] {
        w.put(s as u16 as u32, 16);
    }
    w.put(7, 3); // end
    w.finish()
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, n: u32) {
        self.acc = self.acc << n | (value as u64 & ((1u64 << n) - 1));
        self.bits += n;
        while self.bits >= 8 {
            self.bits -= 8;
            self.out.push((self.acc >> self.bits) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push((self.acc << (8 - self.bits)) as u8);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alac_frames_decode() {
        let samples: Vec<i16> = (0..FRAMES_PER_PACKET * 2)
            .map(|i| (i as i16).wrapping_mul(37))
            .collect();
        // The cookie of the SDP's fmtp line.
        let cookie = [
            0, 0, 1, 0x60, 0, 16, 40, 10, 14, 2, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xac, 0x44,
        ];
        let mut dec = bl_media::AlacDecoder::new(&cookie).unwrap();
        let mut pcm = Vec::new();
        dec.decode_packet(&alac_frame(&samples), &mut pcm).unwrap();
        assert_eq!(pcm.len(), samples.len());
        for (a, b) in pcm.iter().zip(&samples) {
            assert!((a * 32768.0 - *b as f32).abs() < 0.5);
        }
    }
}
