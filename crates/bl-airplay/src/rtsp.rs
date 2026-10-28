//! RTSP messages as AirPlay senders use them (HTTP-like text headers with an
//! optional body), with size limits for everything read from the network.

use std::io::{BufRead, Read};

/// Longest accepted header block.
const MAX_HEADERS: usize = 16 * 1024;
/// Largest accepted body (cover art is the largest one).
pub const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub uri: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> &str {
        self.header("Content-Type").unwrap_or("")
    }
}

#[derive(Debug)]
pub enum ReadError {
    /// The connection was closed between requests.
    Closed,
    Io(std::io::Error),
    Invalid(&'static str),
}

/// Reads one request. `Closed` when the peer closed the connection cleanly.
pub fn read_request(r: &mut impl BufRead) -> Result<Request, ReadError> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = r
            .take((MAX_HEADERS - head.len()) as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(ReadError::Io)?;
        if n == 0 {
            return if head.is_empty() {
                Err(ReadError::Closed)
            } else {
                Err(ReadError::Invalid("truncated header"))
            };
        }
        if head.len() + line.len() > MAX_HEADERS {
            return Err(ReadError::Invalid("header too large"));
        }
        let blank = line == b"\r\n" || line == b"\n";
        if blank && head.is_empty() {
            // Stray empty line between requests.
            continue;
        }
        head.extend_from_slice(&line);
        if blank {
            break;
        }
    }
    let text = std::str::from_utf8(&head).map_err(|_| ReadError::Invalid("header not UTF-8"))?;
    let mut lines = text.lines();
    let first = lines.next().ok_or(ReadError::Invalid("empty request"))?;
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let uri = parts.next().unwrap_or("").to_string();
    let proto = parts.next().unwrap_or("");
    if method.is_empty() || !(proto.starts_with("RTSP/") || proto.starts_with("HTTP/")) {
        return Err(ReadError::Invalid("bad request line"));
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once(':')
            .ok_or(ReadError::Invalid("bad header"))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let mut req = Request {
        method,
        uri,
        headers,
        body: Vec::new(),
    };
    if let Some(len) = req.header("Content-Length") {
        let len: usize = len.parse().map_err(|_| ReadError::Invalid("bad length"))?;
        if len > MAX_BODY {
            return Err(ReadError::Invalid("body too large"));
        }
        req.body = vec![0; len];
        r.read_exact(&mut req.body).map_err(ReadError::Io)?;
    }
    Ok(req)
}

pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn body(mut self, content_type: &str, body: Vec<u8>) -> Response {
        self.headers
            .push(("Content-Type".to_string(), content_type.to_string()));
        self.body = body;
        self
    }

    /// Serializes the response; `cseq` echoes the request's sequence number.
    pub fn to_bytes(&self, cseq: Option<&str>) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            453 => "Not Enough Bandwidth",
            455 => "Method Not Valid in This State",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            _ => "Error",
        };
        let mut out = format!("RTSP/1.0 {} {}\r\n", self.status, reason);
        if let Some(c) = cseq {
            out.push_str(&format!("CSeq: {c}\r\n"));
        }
        out.push_str("Server: AirTunes/105.1\r\n");
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        if !self.body.is_empty() {
            out.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

/// Values of a `key=value;key=value` header (Transport, RTP-Info).
pub fn param(header: &str, key: &str) -> Option<String> {
    header.split(';').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case(key)
            .then(|| v.trim().to_string())
    })
}

/// Fields of an HTTP Digest `Authorization` header.
pub fn digest_field(header: &str, key: &str) -> Option<String> {
    let rest = header.trim().strip_prefix("Digest")?;
    let mut s = rest;
    while !s.is_empty() {
        s = s.trim_start_matches([' ', ',']);
        let (k, after) = s.split_once('=')?;
        let (v, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"')?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if k.trim().eq_ignore_ascii_case(key) {
            return Some(v.to_string());
        }
        s = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests_in_sequence() {
        let data = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nApple-Challenge: abc\r\n\r\n\
            ANNOUNCE rtsp://1.2.3.4/123 RTSP/1.0\r\nCSeq: 2\r\nContent-Type: application/sdp\r\nContent-Length: 5\r\n\r\nv=0\r\n";
        let mut r = std::io::BufReader::new(&data[..]);
        let a = read_request(&mut r).unwrap();
        assert_eq!(a.method, "OPTIONS");
        assert_eq!(a.header("apple-challenge"), Some("abc"));
        let b = read_request(&mut r).unwrap();
        assert_eq!(b.method, "ANNOUNCE");
        assert_eq!(b.body, b"v=0\r\n");
        assert!(matches!(read_request(&mut r), Err(ReadError::Closed)));
    }

    #[test]
    fn rejects_oversized_input() {
        let mut big = b"SET_PARAMETER * RTSP/1.0\r\nX: ".to_vec();
        big.extend(std::iter::repeat(b'a').take(MAX_HEADERS));
        big.extend_from_slice(b"\r\n\r\n");
        assert!(read_request(&mut std::io::BufReader::new(&big[..])).is_err());
        let huge = format!(
            "SET_PARAMETER * RTSP/1.0\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(read_request(&mut std::io::BufReader::new(huge.as_bytes())).is_err());
        assert!(read_request(&mut std::io::BufReader::new(&b"garbage\r\n\r\n"[..])).is_err());
    }

    #[test]
    fn header_params() {
        let t =
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port=6001;timing_port=6002";
        assert_eq!(param(t, "control_port").as_deref(), Some("6001"));
        assert_eq!(param(t, "server_port"), None);
        let d = r#"Digest username="iTunes", realm="raop", nonce="abc", uri="*", response="0123""#;
        assert_eq!(digest_field(d, "nonce").as_deref(), Some("abc"));
        assert_eq!(digest_field(d, "response").as_deref(), Some("0123"));
        assert_eq!(digest_field(d, "uri").as_deref(), Some("*"));
    }
}
