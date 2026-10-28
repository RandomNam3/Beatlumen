//! HTTP/1.1 message primitives shared by the client and the server.

use std::io::{self, BufRead, Read, Write};

/// Maximum accepted size of a request/response head.
pub const MAX_HEAD: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self
            .0
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            slot.1 = value;
        } else {
            self.0.push((name.to_string(), value));
        }
    }
    pub fn add(&mut self, name: &str, value: impl Into<String>) {
        self.0.push((name.to_string(), value.into()));
    }
    pub fn content_length(&self) -> Option<u64> {
        self.get("content-length")
            .and_then(|v| v.trim().parse().ok())
    }
    pub fn is_chunked(&self) -> bool {
        self.get("transfer-encoding")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("chunked"))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Reads a CRLF (or LF) terminated line without the terminator.
pub fn read_line<R: BufRead>(r: &mut R, limit: &mut usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated line",
                ))
            };
        }
        if let Some(i) = avail.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&avail[..i]);
            r.consume(i + 1);
            break;
        }
        let n = avail.len();
        buf.extend_from_slice(avail);
        r.consume(n);
        if buf.len() > *limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
    }
    *limit = limit.saturating_sub(buf.len() + 2);
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Reads header lines up to the empty line.
pub fn read_headers<R: BufRead>(r: &mut R, limit: &mut usize) -> io::Result<Headers> {
    let mut h = Headers::default();
    loop {
        let line = read_line(r, limit)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "eof in headers"))?;
        if line.is_empty() {
            return Ok(h);
        }
        if h.0.len() > 200 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
        if let Some((k, v)) = line.split_once(':') {
            h.0.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
}

/// A reader for a chunked transfer-encoded body.
pub struct ChunkedReader<R> {
    inner: R,
    remaining: u64,
    done: bool,
}

impl<R: BufRead> ChunkedReader<R> {
    pub fn new(inner: R) -> Self {
        ChunkedReader {
            inner,
            remaining: 0,
            done: false,
        }
    }
}

impl<R: BufRead> Read for ChunkedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut limit = 4096;
            let line = read_line(&mut self.inner, &mut limit)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "eof in chunk size"))?;
            let size_str = line.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size_str, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                // Trailers.
                let mut limit = MAX_HEAD;
                read_headers(&mut self.inner, &mut limit)?;
                self.done = true;
                return Ok(0);
            }
            self.remaining = size;
        }
        let want = (buf.len() as u64).min(self.remaining) as usize;
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof in chunk"));
        }
        self.remaining -= n as u64;
        if self.remaining == 0 {
            let mut limit = 16;
            read_line(&mut self.inner, &mut limit)?;
        }
        Ok(n)
    }
}

/// Writes a body using chunked encoding.
pub struct ChunkedWriter<W: Write> {
    inner: W,
}

impl<W: Write> ChunkedWriter<W> {
    pub fn new(inner: W) -> Self {
        ChunkedWriter { inner }
    }
    pub fn finish(mut self) -> io::Result<W> {
        self.inner.write_all(b"0\r\n\r\n")?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for ChunkedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        write!(self.inner, "{:x}\r\n", buf.len())?;
        self.inner.write_all(buf)?;
        self.inner.write_all(b"\r\n")?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Percent-decodes a URL component (`+` is kept unless `form` is set).
pub fn percent_decode(s: &str, form: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|x| u8::from_str_radix(x, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if form => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes everything except unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parses a query string into decoded key/value pairs.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k, true), percent_decode(v, true)),
            None => (percent_decode(p, true), String::new()),
        })
        .collect()
}

/// Parses a single `Range: bytes=a-b` header for a resource of `len` bytes.
/// Returns the inclusive byte range, `Err(())` if unsatisfiable, `Ok(None)`
/// if absent/unsupported (serve the full body).
#[allow(clippy::result_unit_err)]
pub fn parse_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(h) = header else { return Ok(None) };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (a, b) = (a.trim(), b.trim());
    if len == 0 {
        return Err(());
    }
    let range = if a.is_empty() {
        let n: u64 = b.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end: u64 = if b.is_empty() {
            len - 1
        } else {
            b.parse::<u64>().map_err(|_| ())?.min(len - 1)
        };
        if start > end || start >= len {
            return Err(());
        }
        (start, end)
    };
    Ok(Some(range))
}

pub fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn chunked_roundtrip() {
        let mut out = Vec::new();
        {
            let mut w = ChunkedWriter::new(&mut out);
            w.write_all(b"hello ").unwrap();
            w.write_all(b"world").unwrap();
            w.finish().unwrap();
        }
        let mut r = ChunkedReader::new(BufReader::new(&out[..]));
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(Some("bytes=0-99"), 1000), Ok(Some((0, 99))));
        assert_eq!(parse_range(Some("bytes=500-"), 1000), Ok(Some((500, 999))));
        assert_eq!(parse_range(Some("bytes=-100"), 1000), Ok(Some((900, 999))));
        assert_eq!(
            parse_range(Some("bytes=990-2000"), 1000),
            Ok(Some((990, 999)))
        );
        assert_eq!(parse_range(Some("bytes=1000-"), 1000), Err(()));
        assert_eq!(parse_range(None, 1000), Ok(None));
    }

    #[test]
    fn query() {
        let q = parse_query("a=1&b=hello%20world&c=x+y&d");
        assert_eq!(q[1], ("b".into(), "hello world".into()));
        assert_eq!(q[2].1, "x y");
        assert_eq!(q[3], ("d".into(), String::new()));
        assert_eq!(percent_decode(&percent_encode("äö/ü?x"), false), "äö/ü?x");
        assert_eq!(percent_decode("100%", false), "100%");
    }
}
