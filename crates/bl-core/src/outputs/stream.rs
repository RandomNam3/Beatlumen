//! A live stream of the playback for renderers that fetch audio themselves
//! (DLNA, Chromecast): uncompressed 44.1 kHz 16-bit stereo over HTTP, as
//! WAV or raw L16. The URL carries a random token; the audio travels
//! unencrypted through the local network (renderers cannot decrypt).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub(crate) const RATE: u32 = 44100;
/// Audio kept for connections that start or lag (frames).
const KEEP: u64 = RATE as u64 * 20;

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
            .spawn(move || {
                while !s.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let s = s.clone();
                            let _ = std::thread::Builder::new()
                                .name("bl-stream-conn".into())
                                .spawn(move || serve(conn, &s));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })?;
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

fn serve(conn: TcpStream, s: &Shared) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
    let mut reader = BufReader::new(match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    // Skip the headers.
    loop {
        let mut h = String::new();
        match reader.read_line(&mut h) {
            Ok(0) | Err(_) => return,
            Ok(_) if h == "\r\n" || h == "\n" => break,
            Ok(_) => {}
        }
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut conn = conn;
    let format = if path == format!("/{}.wav", s.token) {
        Format::Wav
    } else if path == format!("/{}.l16", s.token) {
        Format::L16
    } else {
        let _ = conn
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
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
