//! Small threaded HTTP/1.1 server with keep-alive, request bodies
//! (content-length and chunked) and file responses with byte ranges.

use crate::http::{
    percent_decode, read_headers, read_line, status_text, ChunkedReader, Headers, MAX_HEAD,
};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
}

pub type Handler = dyn Fn(Request) -> Response + Send + Sync;

pub struct ServerConfig {
    pub max_connections: usize,
    pub max_body: usize,
    pub keep_alive: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            max_connections: 256,
            max_body: 256 * 1024 * 1024,
            keep_alive: Duration::from_secs(30),
        }
    }
}

/// Accepts connections forever, handling each on its own thread.
pub fn serve(listener: TcpListener, handler: Arc<Handler>, config: ServerConfig) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    let config = Arc::new(config);
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        if active.load(Ordering::Relaxed) >= config.max_connections {
            let mut s = stream;
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let handler = handler.clone();
        let active2 = active.clone();
        let config = config.clone();
        let spawned = std::thread::Builder::new()
            .name("http-conn".into())
            .spawn(move || {
                let _ = handle_connection(stream, &*handler, &config);
                active2.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

fn handle_connection(
    stream: TcpStream,
    handler: &Handler,
    config: &ServerConfig,
) -> io::Result<()> {
    let remote = stream.peer_addr().ok();
    stream.set_read_timeout(Some(config.keep_alive))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let _ = stream.set_nodelay(true);
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::with_capacity(16 * 1024, stream);
    loop {
        let mut limit = MAX_HEAD;
        let line = match read_line(&mut reader, &mut limit) {
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(_) => return Ok(()),
        };
        if line.is_empty() {
            continue;
        }
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
        // Body.
        let mut body = Vec::new();
        if headers.is_chunked() {
            let mut r = ChunkedReader::new(&mut reader);
            if (&mut r)
                .take(config.max_body as u64 + 1)
                .read_to_end(&mut body)
                .is_err()
                || body.len() > config.max_body
            {
                write_response(
                    &mut writer,
                    Response::text(413, "body too large"),
                    false,
                    true,
                )?;
                return Ok(());
            }
        } else if let Some(n) = headers.content_length() {
            if n as usize > config.max_body {
                write_response(
                    &mut writer,
                    Response::text(413, "body too large"),
                    false,
                    true,
                )?;
                return Ok(());
            }
            body.resize(n as usize, 0);
            if reader.read_exact(&mut body).is_err() {
                return Ok(());
            }
        }
        if headers
            .get("expect")
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
        {
            // Bodies were already read; clients that wait for 100 continue
            // after a timeout anyway.
        }
        let (path, raw_query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let req = Request {
            method: method.clone(),
            path: percent_decode(&path, false),
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
        Body::Empty => 0,
        Body::Bytes(b) => b.len() as u64,
        Body::File { len, .. } => *len,
    };
    resp.headers.set("Content-Length", len.to_string());
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
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client;

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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
