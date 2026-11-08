//! Small threaded HTTP/1.1 server with keep-alive, request bodies
//! (content-length and chunked) and file responses with byte ranges.

use crate::http::{
    percent_decode, read_headers, read_line, status_text, ChunkedReader, Headers, MAX_HEAD,
};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    /// The peer of the connection.
    pub remote: Option<SocketAddr>,
    /// The client: the peer, or the address a trusted reverse proxy
    /// forwarded for ([`ServerConfig::trusted_proxies`]).
    pub client: Option<IpAddr>,
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

/// Decides from a request's head (its `body` still empty) how large its
/// body may be, before any of it is read; `Err` answers the request with
/// that response instead (the body is never read).
pub type BodyLimit = dyn Fn(&Request) -> Result<usize, Response> + Send + Sync;

/// An address or a range of them (`10.0.0.0/8`, `fd00::/8`, `::1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    addr: IpAddr,
    prefix: u8,
}

impl IpNet {
    /// Parses `ADDRESS` or `ADDRESS/PREFIX` (IPv4 or IPv6).
    pub fn parse(s: &str) -> Result<IpNet, String> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let a = a.trim_start_matches('[').trim_end_matches(']');
        let addr = a
            .parse::<IpAddr>()
            .map_err(|_| format!("\"{s}\" is not an IP address or address range"))?
            .to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => p
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| format!("\"{s}\": the prefix length must be 0 to {max}"))?,
        };
        Ok(IpNet { addr, prefix })
    }

    /// The loopback addresses (`127.0.0.0/8`, `::1`).
    pub fn loopback() -> Vec<IpNet> {
        vec![
            IpNet {
                addr: IpAddr::from([127, 0, 0, 0]),
                prefix: 8,
            },
            IpNet {
                addr: IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
                prefix: 128,
            },
        ]
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        // Bits above the prefix are equal.
        fn same(diff: u128, bits: u32, prefix: u8) -> bool {
            prefix == 0 || diff >> (bits - prefix as u32) == 0
        }
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                same((u32::from(n) ^ u32::from(a)) as u128, 32, self.prefix)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => same(u128::from(n) ^ u128::from(a), 128, self.prefix),
            _ => false,
        }
    }
}

impl std::fmt::Display for IpNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let full = if self.addr.is_ipv4() { 32 } else { 128 };
        if self.prefix == full {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix)
        }
    }
}

/// The address limits count against: IPv6 grouped by /64 (one client
/// usually owns a whole /64), IPv4-mapped IPv6 as IPv4.
pub fn rate_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// Connections refused by the limits that may still ask for a health check
/// (`probe_paths`) from the loopback interface.
const MAX_PROBES: usize = 4;

pub struct ServerConfig {
    pub max_connections: usize,
    /// Simultaneous connections per client address (IPv6: per /64). A
    /// trusted proxy's connections are not counted; each of its requests
    /// counts against the client it forwards for instead.
    pub max_per_ip: usize,
    /// Largest request body in general.
    pub max_body: usize,
    /// Larger limits for specific requests: (method, exact path, limit).
    pub large_bodies: Vec<(&'static str, &'static str, usize)>,
    /// Decides the body limit from the head instead of `max_body` and
    /// `large_bodies` (e.g. large bodies only with a login).
    pub body_limit: Option<Arc<BodyLimit>>,
    /// Reverse proxies whose `X-Forwarded-For` (or `X-Real-IP`) names the
    /// client. Nobody else's is believed.
    pub trusted_proxies: Vec<IpNet>,
    /// `GET`/`HEAD` paths a loopback client may still ask for when the
    /// limits refuse its connection (the server's own health check).
    pub probe_paths: Vec<&'static str>,
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
            body_limit: None,
            trusted_proxies: Vec::new(),
            probe_paths: Vec::new(),
            keep_alive: Duration::from_secs(30),
            head_timeout: Duration::from_secs(10),
            min_body_rate: 16 * 1024,
        }
    }
}

impl ServerConfig {
    fn static_body_limit(&self, method: &str, path: &str) -> usize {
        self.large_bodies
            .iter()
            .find(|(m, p, _)| *m == method && *p == path)
            .map_or(self.max_body, |(_, _, n)| *n)
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|n| n.contains(ip))
    }
}

/// Connection counts.
#[derive(Default)]
struct Limits {
    active: AtomicUsize,
    probes: AtomicUsize,
    per_ip: Mutex<std::collections::HashMap<IpAddr, usize>>,
}

impl Limits {
    /// Counts one more for `key` unless it has `max` already.
    fn take(&self, key: IpAddr, max: usize) -> bool {
        let mut m = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        let n = m.entry(key).or_insert(0);
        if *n >= max {
            if *n == 0 {
                m.remove(&key);
            }
            return false;
        }
        *n += 1;
        true
    }

    fn give(&self, key: IpAddr) {
        let mut m = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&key);
            }
        }
    }
}

/// What a connection or request holds of the limits, given back when it is
/// dropped: also when a handler or a streamed body panics.
struct Slot {
    limits: Arc<Limits>,
    active: bool,
    probe: bool,
    key: Option<IpAddr>,
}

impl Slot {
    fn new(limits: &Arc<Limits>) -> Slot {
        Slot {
            limits: limits.clone(),
            active: false,
            probe: false,
            key: None,
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if self.active {
            self.limits.active.fetch_sub(1, Ordering::Relaxed);
        }
        if self.probe {
            self.limits.probes.fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(key) = self.key {
            self.limits.give(key);
        }
    }
}

/// Counts `n` up unless it reached `max`.
fn take_counter(n: &AtomicUsize, max: usize) -> bool {
    let mut v = n.load(Ordering::Relaxed);
    while v < max {
        match n.compare_exchange_weak(v, v + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => v = now,
        }
    }
    false
}

/// A slot for a new connection from `peer`, or None when the limits refuse
/// it. A trusted proxy's connections count only towards the total.
fn admit(limits: &Arc<Limits>, config: &ServerConfig, peer: Option<IpAddr>) -> Option<Slot> {
    let mut slot = Slot::new(limits);
    if !take_counter(&limits.active, config.max_connections) {
        return None;
    }
    slot.active = true;
    if let Some(ip) = peer.filter(|ip| !config.trusts(*ip)) {
        let key = rate_key(ip);
        if !limits.take(key, config.max_per_ip) {
            return None;
        }
        slot.key = Some(key);
    }
    Some(slot)
}

/// Accepts connections forever, handling each on its own thread.
pub fn serve(listener: TcpListener, handler: Arc<Handler>, config: ServerConfig) -> io::Result<()> {
    let limits = Arc::new(Limits::default());
    let config = Arc::new(config);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => {
                // E.g. out of file descriptors: no busy loop meanwhile.
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        let peer = stream.peer_addr().ok().map(|a| a.ip().to_canonical());
        let slot = admit(&limits, &config, peer).or_else(|| {
            // A health check gets through on a few reserved connections.
            let local = peer.is_some_and(|ip| ip.is_loopback());
            if !local || config.probe_paths.is_empty() || !take_counter(&limits.probes, MAX_PROBES)
            {
                return None;
            }
            let mut s = Slot::new(&limits);
            s.probe = true;
            Some(s)
        });
        let Some(slot) = slot else {
            let mut s = stream;
            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        };
        let handler = handler.clone();
        let limits = limits.clone();
        let config = config.clone();
        // When the thread cannot start, the closure and with it the slot
        // and the connection are dropped.
        let _ = std::thread::Builder::new()
            .name("http-conn".into())
            .spawn(move || {
                let probe = slot.probe;
                let _slot = slot;
                // A panic (in parsing, a handler or a streamed body) ends
                // only this connection; the slot is given back either way.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    handle_connection(stream, &*handler, &config, &limits, probe)
                }));
            });
    }
    Ok(())
}

/// The client a trusted proxy forwards for: the last address in
/// `X-Forwarded-For` that is not one of the trusted proxies themselves
/// (addresses further left were written by the client and prove nothing).
fn forwarded_client(headers: &Headers, peer: IpAddr, trusted: &[IpNet]) -> IpAddr {
    fn parse(v: &str) -> Option<IpAddr> {
        let v = v.trim().trim_matches('"');
        v.parse::<IpAddr>()
            .ok()
            .or_else(|| v.parse::<SocketAddr>().ok().map(|a| a.ip()))
            .map(|ip| ip.to_canonical())
    }
    let hops: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("x-forwarded-for"))
        .flat_map(|(_, v)| v.split(','))
        .collect();
    if hops.is_empty() {
        return headers.get("x-real-ip").and_then(parse).unwrap_or(peer);
    }
    let mut client = peer;
    for hop in hops.iter().rev() {
        if !trusted.iter().any(|n| n.contains(client)) {
            break;
        }
        match parse(hop) {
            Some(ip) => client = ip,
            None => break,
        }
    }
    client
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

/// Answers a request whose body was not read, then closes the connection.
/// What the client still sends is read and dropped for a moment, so that it
/// gets to see the answer (closing on unread data resets the connection).
fn refuse(
    writer: &mut TcpStream,
    reader: &mut BufReader<DeadlineStream>,
    resp: Response,
) -> io::Result<()> {
    write_response(writer, resp, false, true)?;
    let _ = writer.shutdown(std::net::Shutdown::Write);
    reader.get_mut().deadline = Instant::now() + Duration::from_secs(2);
    let _ = io::copy(&mut reader.take(256 * 1024), &mut io::sink());
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    handler: &Handler,
    config: &ServerConfig,
    limits: &Arc<Limits>,
    probe: bool,
) -> io::Result<()> {
    let remote = stream.peer_addr().ok();
    let peer = remote.map(|a| a.ip().to_canonical());
    let proxy = peer.is_some_and(|ip| config.trusts(ip));
    let active = &limits.active;
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
    // After an answer the next request may take the keep-alive time (blank
    // lines between requests do not extend it).
    let mut answered = false;
    loop {
        if answered {
            // Idle keep-alive connections are dropped quickly when the
            // server is nearly full.
            let idle = if active.load(Ordering::Relaxed) * 4 >= config.max_connections * 3 {
                Duration::from_secs(2)
            } else {
                config.keep_alive
            };
            reader.get_mut().deadline = Instant::now() + idle;
            answered = false;
        }
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
        let has_body = headers.is_chunked() || announced.is_some_and(|n| n > 0);
        // Over the limits, a local health check still gets through: one
        // request, without a body.
        if probe
            && (has_body
                || !matches!(method.as_str(), "GET" | "HEAD")
                || !config.probe_paths.contains(&path.as_str()))
        {
            return refuse(&mut writer, &mut reader, Response::text(503, "busy"));
        }
        let client = peer.map(|ip| {
            if proxy {
                forwarded_client(&headers, ip, &config.trusted_proxies)
            } else {
                ip
            }
        });
        // A trusted proxy's requests count against the clients they come
        // from, while they run.
        let mut client_slot = Slot::new(limits);
        if proxy && !probe {
            if let Some(key) = client.map(rate_key) {
                if !limits.take(key, config.max_per_ip) {
                    return refuse(&mut writer, &mut reader, Response::text(503, "busy"));
                }
                client_slot.key = Some(key);
            }
        }
        let mut req = Request {
            method: method.clone(),
            path,
            query: crate::http::parse_query(&raw_query),
            raw_query,
            headers,
            body: Vec::new(),
            remote,
            client,
        };
        // Body: its limit is decided from the head (before any of it is
        // read), it is read as it arrives (never allocated up front from the
        // announced length) and at a minimum rate.
        let max_body = if !has_body {
            0
        } else if let Some(decide) = &config.body_limit {
            match catch_unwind(AssertUnwindSafe(|| decide(&req))) {
                Ok(Ok(n)) => n,
                Ok(Err(resp)) => return refuse(&mut writer, &mut reader, resp),
                Err(_) => {
                    let resp = Response::text(500, "internal server error");
                    return refuse(&mut writer, &mut reader, resp);
                }
            }
        } else {
            config.static_body_limit(&req.method, &req.path)
        };
        if announced.is_some_and(|n| n > max_body as u64) {
            let resp = Response::text(413, "body too large");
            return refuse(&mut writer, &mut reader, resp);
        }
        if has_body
            && req
                .headers
                .get("expect")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("100-continue"))
        {
            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        let expected = announced.unwrap_or(max_body as u64);
        reader.get_mut().deadline = Instant::now()
            + Duration::from_secs(30)
            + Duration::from_secs(expected / config.min_body_rate.max(1));
        let mut body = Vec::new();
        if req.headers.is_chunked() {
            let mut r = ChunkedReader::new(&mut reader);
            if (&mut r)
                .take(max_body as u64 + 1)
                .read_to_end(&mut body)
                .is_err()
                || body.len() > max_body
            {
                let resp = Response::text(413, "body too large");
                return refuse(&mut writer, &mut reader, resp);
            }
        } else if let Some(n) = announced.filter(|n| *n > 0) {
            if (&mut reader).take(n).read_to_end(&mut body).is_err() || body.len() as u64 != n {
                return Ok(());
            }
        }
        req.body = body;
        let resp = catch_unwind(AssertUnwindSafe(|| handler(req)))
            .unwrap_or_else(|_| Response::text(500, "internal server error"));
        let close = !keep_alive || probe;
        write_response(&mut writer, resp, method == "HEAD", close)?;
        drop(client_slot);
        if close {
            return Ok(());
        }
        answered = true;
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
        // Blank lines do not keep a connection open either.
        let mut s = TcpStream::connect(addr).unwrap();
        let t = std::time::Instant::now();
        let mut closed = false;
        for _ in 0..40 {
            if s.write_all(b"\r\n").is_err() {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            closed && t.elapsed() < Duration::from_secs(2),
            "blank lines kept the connection"
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
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(
            rate_key(ip("2001:db8:1:2:aaaa::1")),
            rate_key(ip("2001:db8:1:2:bbbb::7"))
        );
        assert_ne!(
            rate_key(ip("2001:db8:1:2::1")),
            rate_key(ip("2001:db8:1:3::1"))
        );
        // Loopback counts like everybody else; mapped IPv4 as IPv4.
        assert_eq!(rate_key(ip("127.0.0.1")), ip("127.0.0.1"));
        assert_eq!(rate_key(ip("::ffff:10.1.2.3")), ip("10.1.2.3"));
    }

    #[test]
    fn address_ranges() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let n = IpNet::parse("10.0.0.0/8").unwrap();
        assert!(n.contains(ip("10.200.3.4")) && n.contains(ip("::ffff:10.0.0.1")));
        assert!(!n.contains(ip("11.0.0.1")) && !n.contains(ip("::1")));
        let one = IpNet::parse("192.168.1.5").unwrap();
        assert!(one.contains(ip("192.168.1.5")) && !one.contains(ip("192.168.1.6")));
        let v6 = IpNet::parse("fd00::/8").unwrap();
        assert!(v6.contains(ip("fd12::1")) && !v6.contains(ip("fe80::1")));
        assert!(IpNet::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(IpNet::parse("[::1]").unwrap().contains(ip("::1")));
        assert_eq!(
            IpNet::parse("10.0.0.0/8").unwrap().to_string(),
            "10.0.0.0/8"
        );
        assert_eq!(IpNet::parse("::1").unwrap().to_string(), "::1");
        for bad in ["", "10.0.0.0/33", "fd00::/129", "example.org", "10.0.0.0/x"] {
            assert!(IpNet::parse(bad).is_err(), "{bad}");
        }
        assert!(IpNet::loopback()
            .iter()
            .any(|n| n.contains(ip("127.0.0.53"))));
    }

    #[test]
    fn forwarded_addresses_from_trusted_proxies_only() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let trusted = vec![
            IpNet::parse("127.0.0.1").unwrap(),
            IpNet::parse("10.0.0.0/8").unwrap(),
        ];
        let h = |pairs: &[(&str, &str)]| {
            Headers(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        let peer = ip("127.0.0.1");
        // The client's own entries (further left) prove nothing.
        let fwd = h(&[("X-Forwarded-For", "6.6.6.6, 203.0.113.9, 10.1.1.1")]);
        assert_eq!(forwarded_client(&fwd, peer, &trusted), ip("203.0.113.9"));
        // Several headers count as one list.
        let two = h(&[
            ("X-Forwarded-For", "1.2.3.4"),
            ("x-forwarded-for", "[2001:db8::1]:443"),
        ]);
        assert_eq!(forwarded_client(&two, peer, &trusted), ip("2001:db8::1"));
        // Only proxies all the way: the leftmost.
        let all = h(&[("X-Forwarded-For", "10.0.0.7")]);
        assert_eq!(forwarded_client(&all, peer, &trusted), ip("10.0.0.7"));
        // Garbage stops at the last trusted hop.
        let junk = h(&[("X-Forwarded-For", "nonsense")]);
        assert_eq!(forwarded_client(&junk, peer, &trusted), peer);
        let real = h(&[("X-Real-IP", "198.51.100.4")]);
        assert_eq!(forwarded_client(&real, peer, &trusted), ip("198.51.100.4"));
        assert_eq!(forwarded_client(&h(&[]), peer, &trusted), peer);
    }

    /// A server on a fresh port with `config`; returns its address.
    fn start(handler: Arc<Handler>, config: ServerConfig) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, handler, config));
        addr
    }

    /// Like `raw`, retried for a moment while the server still counts a
    /// connection that just ended (its thread gives the slot back after
    /// closing).
    fn raw_ok(addr: SocketAddr, data: &[u8], wait: Duration) -> String {
        let t0 = Instant::now();
        loop {
            let r = raw(addr, data, wait);
            if !r.starts_with("HTTP/1.1 503") || t0.elapsed() > Duration::from_secs(5) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A kept-alive connection that got its first answer (200).
    fn held(addr: SocketAddr, data: &[u8]) -> TcpStream {
        let t0 = Instant::now();
        loop {
            let mut s = TcpStream::connect(addr).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(data).unwrap();
            let mut buf = [0u8; 12];
            s.read_exact(&mut buf).unwrap();
            if &buf == b"HTTP/1.1 200" {
                return s;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "refused");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn panics_give_the_connection_back() {
        // Panics are expected here: keep the test output readable.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        struct Exploding(usize);
        impl Read for Exploding {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    panic!("stream broke");
                }
                self.0 -= 1;
                buf[0] = b'x';
                Ok(1)
            }
        }
        let handler: Arc<Handler> = Arc::new(|req: Request| match req.path.as_str() {
            "/panic" => panic!("handler broke"),
            "/stream" => {
                let mut r = Response::new(200);
                r.body = Body::Stream(Box::new(Exploding(3)));
                r
            }
            _ => Response::text(200, "ok"),
        });
        // Two connections per address: a leak would lock the client out.
        let addr = start(
            handler,
            ServerConfig {
                max_per_ip: 2,
                ..Default::default()
            },
        );
        let w = Duration::from_secs(5);
        for _ in 0..10 {
            let r = raw(addr, b"GET /panic HTTP/1.1\r\nConnection: close\r\n\r\n", w);
            assert!(r.starts_with("HTTP/1.1 500"), "{r}");
            let r = raw(addr, b"GET /stream HTTP/1.1\r\n\r\n", w);
            assert!(
                r.starts_with("HTTP/1.1 200") && !r.ends_with("0\r\n\r\n"),
                "{r}"
            );
        }
        std::panic::set_hook(hook);
        let r = raw_ok(addr, b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n", w);
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        // Both slots are free again: two connections at once still work.
        let a = held(addr, b"GET / HTTP/1.1\r\n\r\n");
        let b = held(addr, b"GET / HTTP/1.1\r\n\r\n");
        drop((a, b));
    }

    #[test]
    fn slot_counts_are_released() {
        let limits = Arc::new(Limits::default());
        let config = ServerConfig {
            max_connections: 3,
            max_per_ip: 2,
            trusted_proxies: vec![IpNet::parse("10.0.0.1").unwrap()],
            ..Default::default()
        };
        let a = Some("192.0.2.1".parse().unwrap());
        let s1 = admit(&limits, &config, a).unwrap();
        let s2 = admit(&limits, &config, a).unwrap();
        assert!(admit(&limits, &config, a).is_none(), "per address");
        let proxy = Some("10.0.0.1".parse().unwrap());
        let p = admit(&limits, &config, proxy).expect("proxies are not counted per address");
        assert!(admit(&limits, &config, proxy).is_none(), "in total");
        // Dropping during a panic gives the slot back as well.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(move || {
            let _held = s1;
            panic!("boom");
        }));
        drop((s2, p));
        assert_eq!(limits.active.load(Ordering::Relaxed), 0);
        assert!(limits.per_ip.lock().unwrap().is_empty());
    }

    #[test]
    fn per_address_limit_counts_forwarded_clients() {
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let g = gate.clone();
        // `/hold` waits until the gate opens, keeping its request running.
        let handler: Arc<Handler> = Arc::new(move |req: Request| {
            if req.path == "/hold" {
                let (m, cv) = &*g;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
            }
            Response::text(200, &req.client.map(|c| c.to_string()).unwrap_or_default())
        });
        let addr = start(
            handler,
            ServerConfig {
                max_per_ip: 1,
                trusted_proxies: IpNet::loopback(),
                ..Default::default()
            },
        );
        let w = Duration::from_secs(5);
        // The forwarded client is the one the handler sees.
        let r = raw(
            addr,
            b"GET / HTTP/1.1\r\nX-Forwarded-For: 6.6.6.6, 203.0.113.5\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.ends_with("\r\n\r\n203.0.113.5"), "{r}");
        // One running request per client, though both come through the
        // same proxy.
        let mut held = TcpStream::connect(addr).unwrap();
        held.write_all(
            b"GET /hold HTTP/1.1\r\nX-Forwarded-For: 203.0.113.5\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let r = raw(
            addr,
            b"GET / HTTP/1.1\r\nX-Forwarded-For: 203.0.113.5\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 503"), "same client: {r}");
        let r = raw(
            addr,
            b"GET / HTTP/1.1\r\nX-Forwarded-For: 198.51.100.7\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 200"), "another client: {r}");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let mut out = String::new();
        held.set_read_timeout(Some(w)).unwrap();
        let _ = held.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        let r = raw_ok(
            addr,
            b"GET / HTTP/1.1\r\nX-Forwarded-For: 203.0.113.5\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 200"), "released: {r}");
    }

    #[test]
    fn untrusted_peers_cannot_forward() {
        let handler: Arc<Handler> = Arc::new(|req: Request| {
            Response::text(200, &req.client.map(|c| c.to_string()).unwrap_or_default())
        });
        let addr = start(
            handler,
            ServerConfig {
                max_per_ip: 1,
                trusted_proxies: vec![IpNet::parse("192.0.2.1").unwrap()],
                probe_paths: vec!["/health"],
                ..Default::default()
            },
        );
        let w = Duration::from_secs(5);
        let r = raw(
            addr,
            b"GET / HTTP/1.1\r\nX-Forwarded-For: 203.0.113.5\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.ends_with("\r\n\r\n127.0.0.1"), "{r}");
        // Loopback is limited like any address now ...
        let held = held(addr, b"GET / HTTP/1.1\r\n\r\n");
        let r = raw(addr, b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n", w);
        assert!(r.starts_with("HTTP/1.1 503"), "{r}");
        // ... but its health check still gets through.
        let r = raw(addr, b"GET /health HTTP/1.1\r\n\r\n", w);
        assert!(
            r.starts_with("HTTP/1.1 200") && r.contains("Connection: close"),
            "{r}"
        );
        drop(held);
    }

    #[test]
    fn body_limit_is_decided_before_the_body() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let handler: Arc<Handler> = Arc::new(move |req: Request| {
            c.fetch_add(1, Ordering::Relaxed);
            Response::text(200, &req.body.len().to_string())
        });
        let decide: Arc<BodyLimit> = Arc::new(|req: &Request| {
            if req.header("authorization") == Some("Bearer good") {
                Ok(100_000)
            } else if req.path == "/upload" {
                Err(Response::text(401, "log in first"))
            } else {
                Ok(10)
            }
        });
        let addr = start(
            handler,
            ServerConfig {
                body_limit: Some(decide),
                ..Default::default()
            },
        );
        let w = Duration::from_secs(5);
        // Only the head is sent: the answer comes without waiting for a body.
        let r = raw(
            addr,
            b"POST /upload HTTP/1.1\r\nContent-Length: 50000\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 401"), "{r}");
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nContent-Length: 50000\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 413"), "{r}");
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n14\r\n01234567890123456789\r\n0\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 413"), "chunked: {r}");
        assert_eq!(calls.load(Ordering::Relaxed), 0, "no handler ran");
        // Small bodies and logins are fine; Expect: 100-continue is answered.
        let r = raw(
            addr,
            b"POST /x HTTP/1.1\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("3"), "{r}");
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(w)).unwrap();
        s.write_all(b"POST /upload HTTP/1.1\r\nAuthorization: Bearer good\r\nContent-Length: 5000\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut cont = [0u8; 25];
        s.read_exact(&mut cont).unwrap();
        assert_eq!(&cont, b"HTTP/1.1 100 Continue\r\n\r\n");
        s.write_all(&[b'y'; 5000]).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(
            out.starts_with("HTTP/1.1 200") && out.ends_with("5000"),
            "{out}"
        );
        // GET without a body never asks.
        let r = raw(
            addr,
            b"GET /upload HTTP/1.1\r\nConnection: close\r\n\r\n",
            w,
        );
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
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
        // A body over the caller's limit is refused (from its announced
        // length, before it is read).
        let small = client::Options {
            max_body: 1000,
            ..Default::default()
        };
        let e = client::request("GET", &format!("{base}/file"), &[], &[], &small).unwrap_err();
        assert!(e.contains("too large"), "{e}");
        assert_eq!(client::Options::default().max_body, 64 << 20);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
