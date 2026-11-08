//! A live stream of the playback for renderers that fetch audio themselves
//! (DLNA, Chromecast): uncompressed 44.1 kHz 16-bit stereo over HTTP, as
//! WAV or raw L16. The URL carries a random token; the audio travels
//! unencrypted through the local network (renderers cannot decrypt).

use crate::listen::{same_secret, DeadlineReader, Slots};
use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub(crate) const RATE: u32 = 44100;
/// Audio kept for connections that start or lag (frames).
const KEEP: u64 = RATE as u64 * 20;
/// Connections at the same time (a speaker opens one or two and may come
/// back); more are turned away.
const MAX_CONNECTIONS: usize = 8;
/// Time a client has to send its request.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Longest request head (request line and headers).
const MAX_HEAD: usize = 16 * 1024;

struct Ring {
    samples: VecDeque<i16>,
    /// Frame index of the first kept frame.
    start: u64,
    /// Where the latest connection started reading.
    served_from: Option<u64>,
}

impl Ring {
    fn end(&self) -> u64 {
        self.start + self.samples.len() as u64 / 2
    }
}

struct Shared {
    ring: Mutex<Ring>,
    cv: Condvar,
    stop: AtomicBool,
    token: String,
    /// New connections start this many frames before the newest audio, so
    /// the renderer fills its buffer at once.
    head_start: u64,
}

pub(crate) struct LiveStream {
    shared: Arc<Shared>,
    port: u16,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Format {
    Wav,
    /// Big-endian samples without a header ("audio/L16").
    L16,
}

impl Format {
    pub(crate) fn mime(self) -> &'static str {
        match self {
            Format::Wav => "audio/wav",
            Format::L16 => "audio/L16;rate=44100;channels=2",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::L16 => "l16",
        }
    }
}

/// The local address the renderer at `peer` reaches this device at.
pub(crate) fn local_ip_towards(peer: IpAddr) -> Option<IpAddr> {
    let s = UdpSocket::bind(if peer.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .ok()?;
    s.connect(SocketAddr::new(peer, 9)).ok()?;
    s.local_addr()
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified())
}

impl LiveStream {
    pub(crate) fn start(head_start: u64) -> std::io::Result<LiveStream> {
        let listener = TcpListener::bind("0.0.0.0:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring {
                samples: VecDeque::new(),
                start: 0,
                served_from: None,
            }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            token,
            head_start,
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("bl-stream".into())
            .spawn(move || accept_loop(listener, s, REQUEST_DEADLINE))?;
        Ok(LiveStream {
            shared,
            port,
            thread: Some(thread),
        })
    }

    pub(crate) fn url(&self, ip: IpAddr, format: Format) -> String {
        let host = match ip {
            IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        format!(
            "http://{host}:{}/{}.{}",
            self.port,
            self.shared.token,
            format.extension()
        )
    }

    /// Appends interleaved stereo frames.
    pub(crate) fn push(&self, pcm: &[i16]) {
        let mut r = self.shared.ring.lock().unwrap_or_else(|e| e.into_inner());
        r.samples.extend(pcm.iter().copied());
        let frames = r.samples.len() as u64 / 2;
        if frames > KEEP {
            let drop = frames - KEEP;
            r.samples.drain(..(drop * 2) as usize);
            r.start += drop;
        }
        drop(r);
        self.shared.cv.notify_all();
    }

    /// Frames pushed so far, and where the latest connection started.
    pub(crate) fn progress(&self) -> (u64, Option<u64>) {
        let r = self.shared.ring.lock().unwrap_or_else(|e| e.into_inner());
        (r.end(), r.served_from)
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.cv.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 128 random bits as hex for the stream's URL.
fn random_token() -> String {
    bl_airplay::crypto::hash::hex(&bl_airplay::crypto::random_bytes(16))
}

/// Accepts connections (non-blocking listener) until the stream stops.
fn accept_loop(listener: TcpListener, s: Arc<Shared>, deadline: Duration) {
    let slots = Slots::new(MAX_CONNECTIONS);
    while !s.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((conn, _)) => {
                let Some(slot) = slots.take() else {
                    drop(conn);
                    continue;
                };
                let s = s.clone();
                let _ = std::thread::Builder::new()
                    .name("bl-stream-conn".into())
                    .spawn(move || {
                        let _slot = slot;
                        crate::panics::catch("live stream", || serve(conn, &s, deadline));
                    });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn serve(conn: TcpStream, s: &Shared, deadline: Duration) {
    let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(reader) = DeadlineReader::new(&conn, deadline) else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let mut limit = MAX_HEAD;
    let Ok(Some(line)) = bl_net::http::read_line(&mut reader, &mut limit) else {
        return;
    };
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut conn = conn;
    // The token first: without it nothing more is read.
    let format = if same_secret(path, &format!("/{}.wav", s.token)) {
        Format::Wav
    } else if same_secret(path, &format!("/{}.l16", s.token)) {
        Format::L16
    } else {
        let _ = conn
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    if bl_net::http::read_headers(&mut reader, &mut limit).is_err() {
        return;
    }
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nConnection: close\r\nCache-Control: no-cache\r\nAccept-Ranges: none\r\ntransferMode.dlna.org: Streaming\r\ncontentFeatures.dlna.org: DLNA.ORG_OP=00;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\r\n\r\n",
        format.mime()
    );
    if conn.write_all(head.as_bytes()).is_err() || method.eq_ignore_ascii_case("HEAD") {
        return;
    }
    if format == Format::Wav {
        // A WAV header for a (practically) endless stream.
        let data_len: u32 = 0x7fff_f000;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(data_len + 36).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&RATE.to_le_bytes());
        h.extend_from_slice(&(RATE * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data_len.to_le_bytes());
        if conn.write_all(&h).is_err() {
            return;
        }
    }
    let mut cursor = {
        let mut r = s.ring.lock().unwrap_or_else(|e| e.into_inner());
        let from = r.end().saturating_sub(s.head_start).max(r.start);
        r.served_from = Some(from);
        from
    };
    let mut out = Vec::new();
    while !s.stop.load(Ordering::Relaxed) {
        {
            let mut r = s.ring.lock().unwrap_or_else(|e| e.into_inner());
            while r.end() <= cursor && !s.stop.load(Ordering::Relaxed) {
                r = match s.cv.wait_timeout(r, Duration::from_millis(500)) {
                    Ok((g, _)) => g,
                    Err(e) => e.into_inner().0,
                };
            }
            // Fell behind what is kept: skip ahead.
            cursor = cursor.max(r.start);
            let from = ((cursor - r.start) * 2) as usize;
            out.clear();
            for &v in r.samples.range(from..) {
                match format {
                    Format::Wav => out.extend_from_slice(&v.to_le_bytes()),
                    Format::L16 => out.extend_from_slice(&v.to_be_bytes()),
                }
            }
            cursor = r.end();
        }
        if !out.is_empty() && conn.write_all(&out).is_err() {
            return;
        }
    }
}

/// A device that plays the live stream (DLNA renderer, Chromecast).
pub(super) trait LiveTarget {
    fn set_volume(&mut self, volume: f32);
    fn set_paused(&mut self, paused: bool);
    /// Called often: talk to the device; an error ends the output.
    fn tick(&mut self) -> Result<(), String>;
    /// Seconds from producing audio until it is heard.
    fn delay(&self) -> f64;
}

/// Audio produced ahead of real time (frames); a new connection also gets
/// this much at once, so the device starts quickly.
pub(super) const LEAD: u64 = RATE as u64 / 2;

/// Feeds the stream from the player in real time until the output stops.
pub(super) fn live_loop(
    job: &super::Job,
    stream: &LiveStream,
    target: &mut dyn LiveTarget,
) -> Result<(), String> {
    use std::time::Instant;
    let mut pump = super::Pump::new(job);
    let mut produced: u64 = 0;
    // Real time at `anchor.0` corresponds to frame `anchor.1`.
    let mut anchor = (Instant::now(), 0u64);
    let mut silent = false;
    while !job.stopped() {
        let pending = job.take_control();
        if let Some(v) = pending.volume {
            target.set_volume(v);
        }
        target.tick()?;
        pump.reset_seen(&job.output);
        if job.output.is_silent() {
            if !silent {
                silent = true;
                target.set_paused(true);
            }
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        if silent {
            silent = false;
            target.set_paused(false);
            anchor = (Instant::now(), produced.saturating_sub(LEAD));
        }
        let now = anchor.1 + (anchor.0.elapsed().as_secs_f64() * RATE as f64) as u64;
        let due = (now + LEAD).saturating_sub(produced);
        if due >= 441 {
            // The newest frame is heard after the device's delay.
            let pcm = pump.take(&job.output, due as usize, target.delay());
            produced += pcm.len() as u64 / 2;
            stream.push(pcm);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Instant;

    fn started(deadline: Duration) -> (Arc<Shared>, SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring {
                samples: VecDeque::new(),
                start: 0,
                served_from: None,
            }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            token: "0123456789abcdef".into(),
            head_start: 0,
        });
        let s = shared.clone();
        let t = std::thread::spawn(move || accept_loop(listener, s, deadline));
        (shared, addr, t)
    }

    fn stop(s: &Shared, t: std::thread::JoinHandle<()>) {
        s.stop.store(true, Ordering::Relaxed);
        s.cv.notify_all();
        t.join().unwrap();
    }

    /// Sends `request` and reads what comes back within a second.
    fn ask(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let _ = c.write_all(request);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = c.read(&mut buf) {
            if n == 0 || out.len() > 1 << 16 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    #[test]
    fn only_the_token_opens_the_stream() {
        let (s, addr, t) = started(Duration::from_secs(5));
        let wrong = ask(addr, b"GET /0123456789abcdee.wav HTTP/1.1\r\n\r\n");
        assert!(wrong.starts_with(b"HTTP/1.1 404"));
        // Without the token the headers are not even read.
        let wrong = ask(addr, b"GET /x.wav HTTP/1.1\r\nX: y\r\n");
        assert!(wrong.starts_with(b"HTTP/1.1 404"));
        let ok = ask(addr, b"HEAD /0123456789abcdef.wav HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(ok.starts_with(b"HTTP/1.1 200 OK"), "{}", String::from_utf8_lossy(&ok));
        stop(&s, t);
    }

    #[test]
    fn an_endless_line_or_a_slow_client_is_cut_off() {
        let (s, addr, t) = started(Duration::from_millis(500));
        // A request line without end: dropped after the head limit.
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let junk = vec![b'a'; 64 * 1024];
        let mut sent = 0;
        while sent < 4 << 20 && c.write_all(&junk).is_ok() {
            sent += junk.len();
        }
        assert!(sent < 4 << 20, "the server kept reading an endless line");
        // A client that sends nothing is closed at the deadline.
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let start = Instant::now();
        let mut buf = [0u8; 8];
        assert!(matches!(c.read(&mut buf), Ok(0) | Err(_)));
        assert!(start.elapsed() < Duration::from_secs(2));
        stop(&s, t);
    }

    #[test]
    fn connections_are_capped() {
        let (s, addr, t) = started(Duration::from_secs(30));
        let idle: Vec<TcpStream> = (0..MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(addr).unwrap())
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        // The next one is closed without an answer.
        let extra = ask(addr, b"HEAD /0123456789abcdef.wav HTTP/1.1\r\n\r\n");
        assert!(extra.is_empty());
        drop(idle);
        std::thread::sleep(Duration::from_millis(300));
        let ok = ask(addr, b"HEAD /0123456789abcdef.wav HTTP/1.1\r\n\r\n");
        assert!(ok.starts_with(b"HTTP/1.1 200 OK"));
        stop(&s, t);
    }
}
