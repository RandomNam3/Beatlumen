//! Small threaded HTTP/1.1 server with keep-alive, request bodies
//! (content-length and chunked) and file responses with byte ranges.

use crate::http::{
    percent_decode, read_headers, read_line, status_text, ChunkedReader, Headers, MAX_HEAD,
};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// Percent-decoded path without the query string.
    pub path: String,
    /// Raw query string (without '?').
    pub raw_query: String,
    pub query: Vec<(String, String)>,
    pub headers: Headers,
    pub body: Vec<u8>,
    pub remote: Option<SocketAddr>,
}

impl Request {
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }
    /// Path segments (empty segments removed).
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }
}

pub enum Body {
    Empty,
    Bytes(Vec<u8>),
    /// A slice of a file (`start`, `len`).
    File {
        file: std::fs::File,
        start: u64,
        len: u64,
    },
    /// Produced while it is sent (length unknown): sent chunked until the
    /// reader ends.
    Stream(Box<dyn Read + Send>),
    /// Read while it is sent, of a known length (a remote file).
    Sized {
        reader: Box<dyn Read + Send>,
        len: u64,
    },
}

pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Body,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Headers::default(),
            body: Body::Empty,
        }
    }
    pub fn bytes(status: u16, content_type: &str, data: Vec<u8>) -> Response {
        let mut r = Response::new(status);
        r.headers.set("Content-Type", content_type);
        r.body = Body::Bytes(data);
        r
    }
    pub fn text(status: u16, text: &str) -> Response {
        Response::bytes(
            status,
            "text/plain; charset=utf-8",
            text.as_bytes().to_vec(),
        )
    }
    pub fn json(status: u16, v: &bl_json::Value) -> Response {
        Response::bytes(status, "application/json", v.to_json().into_bytes())
    }
    /// JSON error body `{"error": msg}`.
    pub fn error(status: u16, msg: &str) -> Response {
        Response::json(status, &bl_json::Value::object().with("error", msg))
    }
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.set(name, value);
        self
    }

    /// Serves a file honouring a `Range` header.
    pub fn file(
        path: &std::path::Path,
        content_type: &str,
        range_header: Option<&str>,
    ) -> Response {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Response::error(404, "not found"),
        };
        let len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => return Response::error(500, "cannot stat file"),
        };
        let mut r = Response::new(200);
        r.headers.set("Content-Type", content_type);
        r.headers.set("Accept-Ranges", "bytes");
        match crate::http::parse_range(range_header, len) {
            Ok(Some((a, b))) => {
                r.status = 206;
                r.headers
                    .set("Content-Range", format!("bytes {a}-{b}/{len}"));
                r.body = Body::File {
                    file,
                    start: a,
                    len: b - a + 1,
                };
            }
            Ok(None) => {
                r.body = Body::File {
                    file,
                    start: 0,
                    len,
                }
            }
            Err(()) => {
                let mut e = Response::new(416);
                e.headers.set("Content-Range", format!("bytes */{len}"));
                return e;
            }
        }
        r
    }
    /// Serves a seekable stream (a remote file) honouring a `Range` header.
    /// One whose length cannot be learned is sent whole and chunked.
    pub fn seekable<R: Read + Seek + Send + 'static>(
        mut src: R,
        content_type: &str,
        range_header: Option<&str>,
    ) -> Response {
        let mut r = Response::new(200);
        r.headers.set("Content-Type", content_type);
        let len = match src.seek(SeekFrom::End(0)) {
            Ok(len) => len,
            Err(_) => {
                r.body = Body::Stream(Box::new(src));
                return r;
            }
        };
        r.headers.set("Accept-Ranges", "bytes");
        let (start, n) = match crate::http::parse_range(range_header, len) {
            Ok(Some((a, b))) => {
                r.status = 206;
                r.headers
                    .set("Content-Range", format!("bytes {a}-{b}/{len}"));
                (a, b - a + 1)
            }
            Ok(None) => (0, len),
            Err(()) => {
                let mut e = Response::new(416);
                e.headers.set("Content-Range", format!("bytes */{len}"));
                return e;
            }
        };
        if src.seek(SeekFrom::Start(start)).is_err() {
            return Response::error(502, "cannot read the file");
        }
        r.body = Body::Sized {
            reader: Box::new(src.take(n)),
            len: n,
        };
        r
    }
}

pub type Handler = dyn Fn(Request) -> Response + Send + Sync;

pub struct ServerConfig {
    pub max_connections: usize,
    /// Simultaneous connections per client address (IPv6: per /64);
    /// loopback (a local reverse proxy) is exempt.
    pub max_per_ip: usize,
    /// Largest request body in general.
    pub max_body: usize,
    /// Larger limits for specific requests: (method, exact path, limit).
    pub large_bodies: Vec<(&'static str, &'static str, usize)>,
    /// Idle time allowed between requests on a kept-alive connection.
    pub keep_alive: Duration,
    /// Time allowed for the request line and headers.
    pub head_timeout: Duration,
    /// Minimum upload rate in bytes per second (plus a fixed grace period)
    /// before a slow body is abandoned.
    pub min_body_rate: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            max_connections: 256,
            max_per_ip: 32,
            max_body: 1024 * 1024,
            large_bodies: Vec::new(),
            keep_alive: Duration::from_secs(30),
            head_timeout: Duration::from_secs(10),
            min_body_rate: 16 * 1024,
        }
    }
}

impl ServerConfig {
    fn body_limit(&self, method: &str, path: &str) -> usize {
        self.large_bodies
            .iter()
            .find(|(m, p, _)| *m == method && *p == path)
            .map_or(self.max_body, |(_, _, n)| *n)
    }
}

/// Connection counts per client address.
struct Limits {
    active: AtomicUsize,
    per_ip: std::sync::Mutex<std::collections::HashMap<IpAddr, usize>>,
}

/// Address a connection counts against (IPv6 grouped by /64, since one
/// client usually owns a whole /64); None for loopback.
fn ip_bucket(addr: Option<SocketAddr>) -> Option<IpAddr> {
    let ip = addr?.ip();
    if ip.is_loopback() {
        return None;
    }
    Some(match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
        v4 => v4,
    })
}

/// Accepts connections forever, handling each on its own thread.
pub fn serve(listener: TcpListener, handler: Arc<Handler>, config: ServerConfig) -> io::Result<()> {
    let limits = Arc::new(Limits {
        active: AtomicUsize::new(0),
        per_ip: Default::default(),
    });
    let config = Arc::new(config);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let bucket = ip_bucket(stream.peer_addr().ok());
        let refused = limits.active.load(Ordering::Relaxed) >= config.max_connections
            || bucket.is_some_and(|ip| {
                let mut m = limits.per_ip.lock().unwrap_or_else(|e| e.into_inner());
                let n = m.entry(ip).or_insert(0);
                if *n >= config.max_per_ip {
                    true
                } else {
                    *n += 1;
                    false
                }
            });
        if refused {
            let mut s = stream;
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        limits.active.fetch_add(1, Ordering::Relaxed);
        let handler = handler.clone();
        let limits2 = limits.clone();
        let config = config.clone();
        let release = move |limits: &Limits| {
            limits.active.fetch_sub(1, Ordering::Relaxed);
            if let Some(ip) = bucket {
                let mut m = limits.per_ip.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(n) = m.get_mut(&ip) {
                    *n -= 1;
                    if *n == 0 {
                        m.remove(&ip);
                    }
                }
            }
        };
        let spawned = std::thread::Builder::new()
            .name("http-conn".into())
            .spawn(move || {
                let _ = handle_connection(stream, &*handler, &config, &limits2.active);
                release(&limits2);
            });
        if spawned.is_err() {
            release(&limits);
        }
    }
    Ok(())
}

/// A socket whose reads fail once a deadline has passed (per-read timeouts
/// alone let a client trickle one byte at a time forever).
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "request too slow"));
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn handle_connection(
    stream: TcpStream,
    handler: &Handler,
    config: &ServerConfig,
    active: &AtomicUsize,
) -> io::Result<()> {
    let remote = stream.peer_addr().ok();
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let _ = stream.set_nodelay(true);
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::with_capacity(
        16 * 1024,
        DeadlineStream {
            stream,
            deadline: Instant::now() + config.head_timeout,
        },
    );
    let mut first = true;
    loop {
        if !first {
            // Idle keep-alive connections are dropped quickly when the
            // server is nearly full.
            let idle = if active.load(Ordering::Relaxed) * 4 >= config.max_connections * 3 {
                Duration::from_secs(2)
            } else {
                config.keep_alive
            };
            reader.get_mut().deadline = Instant::now() + idle;
        }
        first = false;
        let mut limit = MAX_HEAD;
        let line = match read_line(&mut reader, &mut limit) {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(_) => return Ok(()),
        };
        if line.is_empty() {
            continue;
        }
        reader.get_mut().deadline = Instant::now() + config.head_timeout;
        let mut parts = line.split(' ');
        let (method, target, proto) = match (parts.next(), parts.next(), parts.next()) {
            (Some(m), Some(t), Some(p)) => (m.to_string(), t.to_string(), p.to_string()),
            _ => {
                write_response(&mut writer, Response::text(400, "bad request"), false, true)?;
                return Ok(());
            }
        };
        let headers = match read_headers(&mut reader, &mut limit) {
            Ok(h) => h,
            Err(_) => {
                write_response(&mut writer, Response::text(400, "bad headers"), false, true)?;
                return Ok(());
            }
        };
        let http10 = proto == "HTTP/1.0";
        let conn_hdr = headers
            .get("connection")
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_default();
        let keep_alive = if http10 {
            conn_hdr.contains("keep-alive")
        } else {
            !conn_hdr.contains("close")
        };
        let (path, raw_query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let path = percent_decode(&path, false);
        // Body: limited per request, read as it arrives (never allocated
        // up front from the announced length) and at a minimum rate.
        let max_body = config.body_limit(&method, &path);
        let announced = headers.content_length();
        // Ambiguous framing is rejected (request smuggling through proxies
        // that interpret it differently).
        let te = headers.get("transfer-encoding").is_some();
        let cl = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .count();
        if (te && (cl > 0 || !headers.is_chunked())) || cl > 1 || (cl == 1 && announced.is_none()) {
            write_response(
                &mut writer,
                Response::text(400, "bad request framing"),
                false,
                true,
            )?;
            return Ok(());
        }
        if announced.is_some_and(|n| n > max_body as u64) {
            write_response(
                &mut writer,
                Response::text(413, "body too large"),
                false,
                true,
            )?;
            return Ok(());
        }
        let expected = announced.unwrap_or(max_body as u64);
        reader.get_mut().deadline = Instant::now()
            + Duration::from_secs(30)
            + Duration::from_secs(expected / config.min_body_rate.max(1));
        let mut body = Vec::new();
        if headers.is_chunked() {
            let mut r = ChunkedReader::new(&mut reader);
            if (&mut r)
                .take(max_body as u64 + 1)
                .read_to_end(&mut body)
                .is_err()
                || body.len() > max_body
            {
                write_response(
                    &mut writer,
                    Response::text(413, "body too large"),
                    false,
                    true,
                )?;
                return Ok(());
            }
        } else if let Some(n) = announced {
            if (&mut reader).take(n).read_to_end(&mut body).is_err() || body.len() as u64 != n {
                return Ok(());
            }
        }
        let req = Request {
            method: method.clone(),
            path,
            query: crate::http::parse_query(&raw_query),
            raw_query,
            headers,
            body,
            remote,
        };
        let resp = handler(req);
        write_response(&mut writer, resp, method == "HEAD", !keep_alive)?;
        if !keep_alive {
            return Ok(());
        }
    }
}

fn write_response(
    w: &mut TcpStream,
    mut resp: Response,
    head_only: bool,
    close: bool,
) -> io::Result<()> {
    let len = match &resp.body {
        Body::Empty => Some(0),
        Body::Bytes(b) => Some(b.len() as u64),
        Body::File { len, .. } | Body::Sized { len, .. } => Some(*len),
        Body::Stream(_) => None,
    };
    match len {
        Some(len) => resp.headers.set("Content-Length", len.to_string()),
        None => resp.headers.set("Transfer-Encoding", "chunked"),
    }
    if close {
        resp.headers.set("Connection", "close");
    }
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nServer: Beatlumen\r\n",
        resp.status,
        status_text(resp.status)
    );
    for (k, v) in resp.headers.iter() {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    if head_only {
        return w.flush();
    }
    match resp.body {
        Body::Empty => {}
        Body::Bytes(b) => w.write_all(&b)?,
        Body::File {
            mut file,
            start,
            len,
        } => {
            file.seek(SeekFrom::Start(start))?;
            let mut remaining = len;
            let mut buf = vec![0u8; 256 * 1024];
            while remaining > 0 {
                let want = remaining.min(buf.len() as u64) as usize;
                let n = file.read(&mut buf[..want])?;
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n])?;
                remaining -= n as u64;
            }
        }
        Body::Sized { mut reader, len } => {
            let mut remaining = len;
            let mut buf = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let want = remaining.min(buf.len() as u64) as usize;
                let n = match reader.read(&mut buf[..want]) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    // Shorter than announced: the connection must close.
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                w.write_all(&buf[..n])?;
                remaining -= n as u64;
            }
        }
        Body::Stream(mut r) => {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match r.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    // Ends the response early: the client sees no last chunk.
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    break;
                }
                w.write_all(format!("{n:x}\r\n").as_bytes())?;
                w.write_all(&buf[..n])?;
                w.write_all(b"\r\n")?;
                w.flush()?;
            }
            w.write_all(b"0\r\n\r\n")?;
        }
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client;

    /// Sends raw bytes and returns the response text (until close/timeout).
    fn raw(addr: SocketAddr, data: &[u8], wait: Duration) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(wait)).unwrap();
        s.write_all(data).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn hostile_requests_are_limited() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handler: Arc<Handler> =
            Arc::new(|req: Request| Response::text(200, &req.body.len().to_string()));
        let config = ServerConfig {
            max_body: 1000,
            large_bodies: vec![("POST", "/upload", 100_000)],
            head_timeout: Duration::from_millis(300),
            ..Default::default()
        };
        std::thread::spawn(move || serve(listener, handler, config));
        let w = Duration::from_secs(5);
        // Announced bodies above the route's limit are refused before reading.
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        let r = raw(
            addr,
            b"POST /upload HTTP/1.1\r\nContent-Length: 5000\r\nConnection: close\r\n\r\n",
            Duration::from_millis(200),
        );
        assert!(r.is_empty(), "upload route accepts larger bodies: {r}");
        // Ambiguous framing.
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\nabc",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 400"), "{r}");
        let r = raw(addr, b"POST /x HTTP/1.1\r\nContent-Length: abc\r\n\r\n", w);
        assert!(r.starts_with("HTTP/1.1 400"), "{r}");
        // A head that trickles in is cut off after the head timeout.
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let t = std::time::Instant::now();
        let mut closed = false;
        for _ in 0..40 {
            if s.write_all(b"X").is_err() {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            closed && t.elapsed() < Duration::from_secs(2),
            "slow head not dropped"
        );
        // A normal request still works.
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("3"), "{r}");
    }

    #[test]
    fn ipv6_clients_are_grouped_by_prefix() {
        let a: SocketAddr = "[2001:db8:1:2:aaaa::1]:5".parse().unwrap();
        let b: SocketAddr = "[2001:db8:1:2:bbbb::7]:9".parse().unwrap();
        assert_eq!(ip_bucket(Some(a)), ip_bucket(Some(b)));
        assert_eq!(ip_bucket(Some("127.0.0.1:1".parse().unwrap())), None);
    }

    #[test]
    fn end_to_end_with_client() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!("ocnet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let file2 = file.clone();
        let handler: Arc<Handler> = Arc::new(move |req: Request| match req.path.as_str() {
            "/echo" => Response::bytes(200, "application/octet-stream", req.body),
            "/file" => Response::file(&file2, "application/octet-stream", req.header("range")),
            // The file passed through a remote stream, as for remote sources.
            "/relay" => {
                let url = format!("http://{addr}/file");
                let src = client::HttpStream::open(&url, &[]).unwrap();
                Response::seekable(src, "application/octet-stream", req.header("range"))
            }
            "/q" => Response::text(200, req.query("x").unwrap_or("")),
            _ => Response::error(404, "nope"),
        });
        std::thread::spawn(move || serve(listener, handler, ServerConfig::default()));
        let base = format!("http://{addr}");
        let opts = client::Options::default();
        let r = client::request("POST", &format!("{base}/echo"), &[], b"payload", &opts).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"payload"[..]));
        let r = client::request("GET", &format!("{base}/q?x=a%20b"), &[], &[], &opts).unwrap();
        assert_eq!(r.body, b"a b");
        let r = client::request("GET", &format!("{base}/missing"), &[], &[], &opts).unwrap();
        assert_eq!(r.status, 404);
        // Seekable stream.
        let mut s = client::HttpStream::open(&format!("{base}/file"), &[]).unwrap();
        assert_eq!(s.len(), data.len() as u64);
        let mut buf = vec![0u8; 1000];
        s.seek(SeekFrom::Start(123_456)).unwrap();
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &data[123_456..124_456]);
        s.seek(SeekFrom::Start(10)).unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        assert_eq!(&all[..], &data[10..]);
        let mut s = client::HttpStream::open(&format!("{base}/relay"), &[]).unwrap();
        assert_eq!(s.len(), data.len() as u64);
        s.seek(SeekFrom::Start(200_000)).unwrap();
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &data[200_000..201_000]);
        let r = client::request("GET", &format!("{base}/relay"), &[], &[], &opts).unwrap();
        assert_eq!(r.body, data);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
