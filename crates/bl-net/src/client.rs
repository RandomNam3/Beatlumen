//! Minimal blocking HTTP/1.1 client (plain HTTP over TCP).
//!
//! Used by native clients to talk to the configured Beatlumen server. HTTPS
//! is not implemented in this crate; native platforms may provide TLS via
//! system facilities (see the application's `Host` implementations) or the
//! server can be reached through a VPN/LAN.

use crate::http::{read_headers, read_line, ChunkedReader, Headers, MAX_HEAD};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Path including the query string, always starting with '/'.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let s = s.trim();
        let (scheme, rest) = s.split_once("://").ok_or_else(|| format!("invalid URL (missing scheme): {s}"))?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return Err(format!("unsupported URL scheme: {scheme}")),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let authority = authority.rsplit_once('@').map(|(_, a)| a).unwrap_or(authority);
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            // IPv6 literal.
            let (h, rest) = stripped.split_once(']').ok_or("invalid IPv6 host")?;
            let port = rest.strip_prefix(':').map(|p| p.parse::<u16>().map_err(|_| "invalid port")).transpose()?.unwrap_or(default_port);
            (h.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| format!("invalid port in {s}"))?),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(format!("invalid URL (empty host): {s}"));
        }
        Ok(Url { scheme, host, port, path: if path.is_empty() { "/".into() } else { path.to_string() } })
    }

    pub fn host_header(&self) -> String {
        let default = if self.scheme == "https" { 443 } else { 80 };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Resolves a (possibly relative) reference against this URL.
    pub fn join(&self, reference: &str) -> String {
        if reference.contains("://") {
            return reference.to_string();
        }
        let base = format!("{}://{}", self.scheme, self.host_header());
        if reference.starts_with('/') {
            format!("{base}{reference}")
        } else {
            let dir = self.path.split('?').next().unwrap_or("/");
            let dir = &dir[..dir.rfind('/').map(|i| i + 1).unwrap_or(1)];
            format!("{base}{dir}{reference}")
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Vec<u8>,
}

pub struct Options {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub max_body: usize,
    pub max_redirects: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { connect_timeout: Duration::from_secs(8), io_timeout: Duration::from_secs(30), max_body: 1 << 30, max_redirects: 3 }
    }
}

fn connect(url: &Url, opts: &Options) -> io::Result<TcpStream> {
    if url.scheme != "http" {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "HTTPS is not supported by the built-in client"));
    }
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host not found");
    for addr in (url.host.as_str(), url.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, opts.connect_timeout) {
            Ok(s) => {
                s.set_read_timeout(Some(opts.io_timeout))?;
                s.set_write_timeout(Some(opts.io_timeout))?;
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn write_request(stream: &mut TcpStream, method: &str, url: &Url, headers: &[(String, String)], body: &[u8]) -> io::Result<()> {
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Beatlumen/0.1\r\nConnection: close\r\n", url.path, url.host_header());
    for (k, v) in headers {
        if k.contains(['\r', '\n']) || v.contains(['\r', '\n']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid header"));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Reads the status line and headers.
fn read_head<R: BufRead>(r: &mut R) -> io::Result<(u16, Headers)> {
    let mut limit = MAX_HEAD;
    let line = read_line(r, &mut limit)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "empty response"))?;
    let mut parts = line.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/1.") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad status line: {line}")));
    }
    let status = parts.next().and_then(|s| s.parse::<u16>().ok()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status code"))?;
    let headers = read_headers(r, &mut limit)?;
    Ok((status, headers))
}

/// A body reader for a response (content-length, chunked or until close).
pub enum Body<R: BufRead> {
    Length(io::Take<R>),
    Chunked(ChunkedReader<R>),
    Close(R),
}

impl<R: BufRead> Read for Body<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Body::Length(r) => r.read(buf),
            Body::Chunked(r) => r.read(buf),
            Body::Close(r) => r.read(buf),
        }
    }
}

fn body_reader<R: BufRead>(r: R, method: &str, status: u16, headers: &Headers) -> Body<R> {
    if method == "HEAD" || status == 204 || status == 304 {
        return Body::Length(r.take(0));
    }
    if headers.is_chunked() {
        Body::Chunked(ChunkedReader::new(r))
    } else if let Some(n) = headers.content_length() {
        Body::Length(r.take(n))
    } else {
        Body::Close(r)
    }
}

/// Performs a blocking request and reads the whole body.
pub fn request(method: &str, url: &str, headers: &[(String, String)], body: &[u8], opts: &Options) -> Result<Response, String> {
    let mut url_s = url.to_string();
    for _ in 0..=opts.max_redirects {
        let u = Url::parse(&url_s)?;
        let mut stream = connect(&u, opts).map_err(|e| format!("connect {}:{}: {e}", u.host, u.port))?;
        write_request(&mut stream, method, &u, headers, body).map_err(|e| format!("send: {e}"))?;
        let mut r = BufReader::with_capacity(64 * 1024, stream);
        let (status, h) = read_head(&mut r).map_err(|e| format!("receive: {e}"))?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = h.get("location") {
                url_s = u.join(loc);
                continue;
            }
        }
        let mut reader = body_reader(r, method, status, &h);
        let mut out = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut chunk).map_err(|e| format!("receive body: {e}"))?;
            if n == 0 {
                break;
            }
            if out.len() + n > opts.max_body {
                return Err("response body too large".into());
            }
            out.extend_from_slice(&chunk[..n]);
        }
        return Ok(Response { status, headers: h, body: out });
    }
    Err("too many redirects".into())
}

/// A seekable byte stream over HTTP using range requests. Sequential reads
/// reuse one long-running response; seeks reopen the connection.
pub struct HttpStream {
    url: Url,
    headers: Vec<(String, String)>,
    opts: Options,
    len: u64,
    pos: u64,
    conn: Option<(Body<BufReader<TcpStream>>, u64)>,
}

impl HttpStream {
    /// Opens the resource (issues the first request to learn its length).
    pub fn open(url: &str, headers: &[(String, String)]) -> Result<HttpStream, String> {
        let url = Url::parse(url)?;
        let mut s = HttpStream { url, headers: headers.to_vec(), opts: Options::default(), len: 0, pos: 0, conn: None };
        s.reconnect(0).map_err(|e| e.to_string())?;
        Ok(s)
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn reconnect(&mut self, at: u64) -> io::Result<()> {
        self.conn = None;
        let mut stream = connect(&self.url, &self.opts)?;
        let mut headers = self.headers.clone();
        headers.push(("Range".into(), format!("bytes={at}-")));
        write_request(&mut stream, "GET", &self.url, &headers, &[])?;
        let mut r = BufReader::with_capacity(128 * 1024, stream);
        let (status, h) = read_head(&mut r)?;
        match status {
            206 => {
                // Content-Range: bytes a-b/total
                let total = h
                    .get("content-range")
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|t| t.trim().parse::<u64>().ok())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing content-range"))?;
                self.len = total;
                self.conn = Some((body_reader(r, "GET", status, &h), at));
            }
            200 => {
                // Server ignored the range: skip to the wanted position.
                self.len = h.content_length().unwrap_or(0);
                let mut body = body_reader(r, "GET", status, &h);
                io::copy(&mut (&mut body).take(at), &mut io::sink())?;
                self.conn = Some((body, at));
            }
            416 => {
                self.conn = None;
            }
            _ => return Err(io::Error::other(format!("HTTP status {status}"))),
        }
        Ok(())
    }
}

impl Read for HttpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        for attempt in 0..3 {
            let reuse = matches!(&self.conn, Some((_, p)) if *p == self.pos);
            if !reuse {
                self.reconnect(self.pos)?;
            }
            let Some((body, p)) = self.conn.as_mut() else { return Ok(0) };
            match body.read(buf) {
                Ok(0) if attempt < 2 => {
                    self.conn = None;
                    continue;
                }
                Ok(n) => {
                    *p += n as u64;
                    self.pos += n as u64;
                    return Ok(n);
                }
                Err(e) if attempt < 2 && e.kind() != io::ErrorKind::Interrupted => {
                    self.conn = None;
                    let _ = e;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }
}

impl Seek for HttpStream {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.len as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        let new = new as u64;
        // Small forward seeks on the open connection are cheaper to skip.
        if let Some((body, p)) = self.conn.as_mut() {
            if new > *p && new - *p <= 64 * 1024 {
                let skip = new - *p;
                let copied = io::copy(&mut body.take(skip), &mut io::sink())?;
                *p += copied;
            }
        }
        self.pos = new;
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let u = Url::parse("http://example.com:8080/a/b?x=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("example.com", 8080, "/a/b?x=1"));
        let u = Url::parse("http://[::1]/").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 80));
        assert_eq!(u.host_header(), "[::1]");
        assert!(Url::parse("ftp://x").is_err());
        let u = Url::parse("http://h/dir/file").unwrap();
        assert_eq!(u.join("other"), "http://h/dir/other");
        assert_eq!(u.join("/root"), "http://h/root");
    }
}
