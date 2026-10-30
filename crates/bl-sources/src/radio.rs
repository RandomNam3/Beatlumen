//! Internet radio: station files, Icecast/Shoutcast streams and HLS.
//!
//! * Station files (`.pls`, `.m3u`, `.xspf`, and `.m3u8` lists of stream
//!   URLs) are recognised by their Content-Type (`audio/x-scpls`,
//!   `audio/x-mpegurl`, `application/xspf+xml`, …) or their content and
//!   read with [`crate::playlist`] ([`resolve_station`]). A playlist with
//!   `#EXT-X-` tags is HLS, not a station file.
//! * [`IcyStream`]: an Icecast/Shoutcast stream requested with
//!   `Icy-MetaData: 1`. The metadata block after every `icy-metaint`
//!   audio bytes is removed and its `StreamTitle` published through a
//!   shared handle. Shoutcast answers `ICY 200 OK`, which bl-net's client
//!   rejects, so this module has a small GET of its own on top of bl-net's
//!   TLS connector and header parsing.
//! * [`HlsStream`]: HTTP Live Streaming with MPEG-TS (demuxed by
//!   [`TsDemuxer`]) or packed ADTS/MP3 segments.
//! * [`open_stream`] does all of this for a URL; [`open_decoder`] decodes
//!   the result (bl-media's decoders need the length of a file, see
//!   [`LiveSource`]).
//!
//! Live streams cannot seek and are not reconnected: reading ends when the
//! server ends the stream. Titles change when the reader passes them,
//! which is ahead of what is audible by the player's buffer.
//!
//! Network input is bounded: response heads 64 KiB, station files 1 MiB,
//! ICY metadata blocks 4080 bytes (by their format), HLS playlists 1 MiB
//! and segments 16 MiB, 5 redirects. Redirects, station files and
//! playlists may lead to other hosts (radio streams often do), but a
//! station on the internet cannot send the server to an address in the
//! local network (loopback, private, link-local); one configured with a
//! local address may. For HTTPS the address is checked before the TLS
//! connector resolves the name again.

mod decode;
mod hls;
mod ts;

pub use decode::{open_decoder, LiveSource};
pub use hls::HlsStream;
pub use ts::TsDemuxer;

use crate::playlist::{self, Entry, Format};
use bl_net::client::{Conn, Url};
use bl_net::http::{read_headers, read_line, ChunkedReader, Headers, MAX_HEAD};
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
/// Largest `icy-metaint` accepted.
const MAX_METAINT: usize = 1 << 20;
/// Entries of a station file tried before giving up.
const MAX_TRIES: usize = 5;
/// Longest station name or genre kept (characters).
const MAX_NAME: usize = 256;

/// The current title of a stream ("Artist - Title" as the station sends
/// it), updated by the reader.
pub type Titles = Arc<Mutex<Option<String>>>;

fn set_title(titles: &Titles, title: Option<String>) {
    *titles.lock().unwrap_or_else(|e| e.into_inner()) = title;
}

/// Which addresses the requests for a station may reach.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reach {
    /// Addresses in the local network are allowed.
    local: bool,
}

impl Reach {
    /// The station's own address decides: a local one allows local ones.
    pub(crate) fn of(url: &str) -> Reach {
        let local = Url::parse(url)
            .ok()
            .and_then(|u| (u.host.as_str(), u.port).to_socket_addrs().ok())
            .is_some_and(|mut a| a.any(|a| !is_public(a.ip())));
        Reach { local }
    }
}

/// An address on the internet (not loopback, private, link-local, …).
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || o[0] == 0
                || (o[0] == 100 && o[1] & 0xc0 == 64) // shared address space
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || s[0] & 0xfe00 == 0xfc00 // unique local
                || s[0] & 0xffc0 == 0xfe80 // link-local
                || s[0] & 0xffc0 == 0xfec0) // site-local
        }
    }
}

/// An HTTP response with its body as a reader.
pub(crate) struct Response {
    /// The URL after redirects.
    pub url: String,
    pub status: u16,
    pub headers: Headers,
    pub body: Box<dyn Read + Send>,
}

/// A URL fit for a request line: no control characters, spaces and
/// non-ASCII characters percent-encoded, no fragment.
fn clean_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.chars().any(char::is_control) {
        return Err("invalid URL".into());
    }
    let url = url.split('#').next().unwrap_or(url);
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        if c.is_ascii() && c != ' ' {
            out.push(c);
        } else {
            let mut b = [0u8; 4];
            for byte in c.encode_utf8(&mut b).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(out)
}

/// Resolves a reference against a base URL (absolute, `//host/…`,
/// `/path`, `?query` or relative; `.` and `..` are removed).
pub(crate) fn join(base: &str, reference: &str) -> String {
    let r = reference.trim();
    let scheme = r.find("://").map_or("", |i| &r[..i]);
    if scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
    {
        return r.to_string();
    }
    let Ok(u) = Url::parse(base) else {
        return r.to_string();
    };
    if let Some(rest) = r.strip_prefix("//") {
        return format!("{}://{rest}", u.scheme);
    }
    let origin = format!("{}://{}", u.scheme, u.host_header());
    let base_path = u.path.split(['?', '#']).next().unwrap_or("/");
    let joined = if r.is_empty() {
        return format!("{origin}{}", u.path.split('#').next().unwrap_or("/"));
    } else if r.starts_with('?') {
        format!("{base_path}{r}")
    } else if r.starts_with('/') {
        r.to_string()
    } else {
        let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
        format!("{dir}{r}")
    };
    let (path, query) = joined.split_at(joined.find('?').unwrap_or(joined.len()));
    format!("{origin}{}{query}", remove_dots(path))
}

/// Removes `.` and `..` segments from an absolute URL path.
fn remove_dots(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').skip(1).collect();
    let mut out: Vec<&str> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        match *s {
            "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
        if last && matches!(*s, "." | "..") {
            out.push("");
        }
    }
    format!("/{}", out.join("/"))
}

fn connect(u: &Url, reach: Reach) -> Result<Box<dyn Conn>, String> {
    let addrs: Vec<SocketAddr> = (u.host.as_str(), u.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", u.host))?
        .filter(|a| reach.local || is_public(a.ip()))
        .collect();
    if addrs.is_empty() {
        return Err(format!(
            "{}: refused (an address in the local network)",
            u.host
        ));
    }
    if u.scheme == "https" {
        return bl_net::client::tls_connect(&u.host, u.port, CONNECT_TIMEOUT, IO_TIMEOUT, None)
            .map_err(|e| format!("connect {}: {e}", u.host));
    }
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_read_timeout(Some(IO_TIMEOUT))
                    .and_then(|_| s.set_write_timeout(Some(IO_TIMEOUT)))
                    .map_err(|e| e.to_string())?;
                let _ = s.set_nodelay(true);
                return Ok(Box::new(s));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(format!(
        "connect {}:{}: {}",
        u.host,
        u.port,
        last.map_or_else(|| "no address".to_string(), |e| e.to_string())
    ))
}

/// The status line (`HTTP/1.x` or Shoutcast's `ICY`) and headers.
fn read_head<R: BufRead>(r: &mut R) -> Result<(u16, Headers), String> {
    let mut limit = MAX_HEAD;
    let line = read_line(r, &mut limit)
        .map_err(|e| format!("receive: {e}"))?
        .ok_or("empty response")?;
    let mut parts = line.split_whitespace();
    let proto = parts.next().unwrap_or("");
    if !(proto.starts_with("HTTP/1.") || proto == "ICY") {
        return Err("not an HTTP response".into());
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("bad status code")?;
    let headers = read_headers(r, &mut limit).map_err(|e| format!("receive: {e}"))?;
    Ok((status, headers))
}

/// A GET request following redirects; `icy` asks for ICY metadata.
pub(crate) fn get(url: &str, reach: Reach, icy: bool) -> Result<Response, String> {
    let mut url = clean_url(url)?;
    for _ in 0..=MAX_REDIRECTS {
        let u = Url::parse(&url)?;
        let mut conn = connect(&u, reach)?;
        let mut head = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Beatlumen/0.1\r\nAccept: */*\r\n",
            u.path,
            u.host_header()
        );
        if icy {
            head.push_str("Icy-MetaData: 1\r\n");
        }
        head.push_str("Connection: close\r\n\r\n");
        conn.write_all(head.as_bytes())
            .and_then(|_| conn.flush())
            .map_err(|e| format!("send: {e}"))?;
        let mut r = BufReader::with_capacity(32 * 1024, conn);
        let (status, headers) = read_head(&mut r)?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = headers
                .get("location")
                .ok_or("redirect without a location")?;
            url = clean_url(&join(&url, location))?;
            continue;
        }
        let body: Box<dyn Read + Send> = if headers.is_chunked() {
            Box::new(ChunkedReader::new(r))
        } else if let Some(n) = headers.content_length() {
            Box::new(r.take(n))
        } else {
            Box::new(r)
        };
        return Ok(Response {
            url,
            status,
            headers,
            body,
        });
    }
    Err("too many redirects".into())
}

/// Reads all of a body of at most `max` bytes.
fn read_all(r: &mut dyn Read, max: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    r.take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("receive: {e}"))?;
    if out.len() > max {
        return Err("response too large".into());
    }
    Ok(out)
}

/// Fetches a whole resource of at most `max` bytes; returns the URL after
/// redirects and the body.
pub(crate) fn fetch(url: &str, reach: Reach, max: usize) -> Result<(String, Vec<u8>), String> {
    let mut r = get(url, reach, false)?;
    if r.status != 200 {
        return Err(format!("HTTP status {}", r.status));
    }
    let body = read_all(&mut r.body, max)?;
    Ok((r.url, body))
}

/// The media type of a response (lower case, without parameters).
fn mime_of(h: &Headers) -> String {
    h.get("content-type")
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The codec hint of a Content-Type ("" if it does not tell).
pub fn codec_for_mime(content_type: &str) -> &'static str {
    let m = content_type.split(';').next().unwrap_or("").trim();
    match m.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" | "audio/x-mp3"
        | "audio/mpg" => "mp3",
        "audio/aac" | "audio/aacp" | "audio/x-aac" | "audio/x-aacp" | "audio/aac-adts" => "aac",
        "audio/ogg" | "application/ogg" | "audio/x-ogg" | "audio/vorbis" | "audio/x-vorbis+ogg" => {
            "ogg"
        }
        "audio/opus" | "audio/x-opus+ogg" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        _ => "",
    }
}

/// The codec of the start of a stream ("" if unknown): Ogg, FLAC, ADTS
/// AAC (two frames in a row) or MPEG audio.
pub fn sniff_codec(d: &[u8]) -> &'static str {
    if d.starts_with(b"OggS") {
        return "ogg";
    }
    if d.starts_with(b"fLaC") {
        return "flac";
    }
    if d.starts_with(b"ID3") {
        return "mp3";
    }
    for i in 0..d.len().saturating_sub(4) {
        if d[i] != 0xff || d[i + 1] & 0xe0 != 0xe0 {
            continue;
        }
        if d[i + 1] & 0x06 == 0 {
            // Layer 0: ADTS, if another frame follows.
            let next = bl_aac::AdtsHeader::parse(&d[i..])
                .and_then(|h| d.get(i + h.frame_length..))
                .and_then(bl_aac::AdtsHeader::parse);
            if next.is_some() {
                return "aac";
            }
        } else if d[i + 2] & 0xf0 != 0xf0 && d[i + 2] & 0x0c != 0x0c {
            // A valid bitrate and sample rate index.
            return "mp3";
        }
    }
    ""
}

/// A stream of a station.
#[derive(Clone, Debug, PartialEq)]
pub struct StationStream {
    pub url: String,
    pub title: Option<String>,
    /// An HLS playlist (play it with [`HlsStream`]). For the entries of a
    /// station file this is a guess from the extension; [`open_stream`]
    /// looks at the content.
    pub hls: bool,
}

/// What a station URL serves.
enum Fetched {
    /// A station file's streams.
    List(Vec<StationStream>),
    /// An HLS playlist (URL after redirects and its text).
    Hls { url: String, text: String },
    /// Audio: the open response.
    Audio(Response),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Playlist(Format),
    Audio,
    Html,
    Unknown,
}

fn kind_of(mime: &str) -> Kind {
    match mime {
        "audio/x-scpls" | "audio/scpls" | "application/pls+xml" | "application/pls" => {
            Kind::Playlist(Format::Pls)
        }
        "audio/x-mpegurl"
        | "audio/mpegurl"
        | "application/x-mpegurl"
        | "application/mpegurl"
        | "application/vnd.apple.mpegurl"
        | "audio/m3u"
        | "audio/x-m3u" => Kind::Playlist(Format::M3u),
        "application/xspf+xml" => Kind::Playlist(Format::Xspf),
        "text/html" | "application/xhtml+xml" => Kind::Html,
        m if m.starts_with("audio/") || m == "application/ogg" => Kind::Audio,
        _ => Kind::Unknown,
    }
}

/// The streams of a station file's entries: http(s) URLs, relative ones
/// resolved against the file's URL (local paths are left out).
fn station_streams(entries: Vec<Entry>, base: &str) -> Vec<StationStream> {
    entries
        .into_iter()
        .filter_map(|e| {
            let loc = e.location.trim();
            let lower = loc.to_ascii_lowercase();
            let url = if lower.starts_with("http://") || lower.starts_with("https://") {
                loc.to_string()
            } else if loc.contains(':') || loc.contains('\\') {
                return None; // another scheme, file: or a Windows path
            } else {
                join(base, loc)
            };
            Url::parse(&url).ok()?;
            let title = match (e.artist, e.title) {
                (Some(a), Some(t)) => Some(format!("{a} - {t}")),
                (a, t) => t.or(a),
            };
            Some(StationStream {
                hls: crate::ext_of(&url) == "m3u8",
                url,
                title,
            })
        })
        .collect()
}

fn fetch_station(url: &str, reach: Reach) -> Result<Fetched, String> {
    let mut r = get(url, reach, true)?;
    if !(200..300).contains(&r.status) {
        return Err(format!("HTTP status {}", r.status));
    }
    let kind = kind_of(&mime_of(&r.headers));
    match kind {
        Kind::Html => return Err("not a radio stream (a web page)".into()),
        Kind::Audio => return Ok(Fetched::Audio(r)),
        _ => {}
    }
    let mut head = Vec::new();
    (&mut r.body)
        .take(1024)
        .read_to_end(&mut head)
        .map_err(|e| format!("receive: {e}"))?;
    let hint = match kind {
        Kind::Playlist(f) => Some(f.ext().to_string()),
        _ => Some(crate::ext_of(&r.url)),
    };
    let Some(format) = playlist::detect(&head, hint.as_deref()) else {
        // Audio after all: put back what was read.
        let body = std::mem::replace(&mut r.body, Box::new(io::empty()));
        r.body = Box::new(Cursor::new(head).chain(body));
        return Ok(Fetched::Audio(r));
    };
    let data = read_all(
        &mut Cursor::new(head).chain(&mut r.body),
        playlist::MAX_SIZE,
    )?;
    if format == Format::M3u {
        let text = playlist::decode_text(&data);
        if hls::is_hls(&text) {
            return Ok(Fetched::Hls { url: r.url, text });
        }
    }
    let list = station_streams(playlist::parse(&data, Some(format.ext()))?, &r.url);
    if list.is_empty() {
        return Err("the station file lists no streams".into());
    }
    Ok(Fetched::List(list))
}

/// What a station URL leads to: the streams of a station file, or the URL
/// itself (an HLS playlist or a stream; then the title is its `icy-name`).
pub fn resolve_station(url: &str) -> Result<Vec<StationStream>, String> {
    let own = |title: Option<String>, hls: bool| {
        vec![StationStream {
            url: url.trim().to_string(),
            title,
            hls,
        }]
    };
    Ok(match fetch_station(url, Reach::of(url))? {
        Fetched::List(list) => list,
        Fetched::Hls { .. } => own(None, true),
        Fetched::Audio(r) => own(header_text(&r.headers, "icy-name"), false),
    })
}

/// A header's value as display text (trimmed, at most 256 characters).
fn header_text(h: &Headers, name: &str) -> Option<String> {
    let v: String = h.get(name)?.trim().chars().take(MAX_NAME).collect();
    (!v.is_empty()).then_some(v)
}

/// The `StreamTitle` of an ICY metadata block (UTF-8, else Latin-1): None
/// without one, empty when the station clears it.
pub fn parse_stream_title(block: &[u8]) -> Option<String> {
    let find = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n);
    let end = block.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let block = &block[..end];
    const KEY: &[u8] = b"streamtitle=";
    let at = find(&block.to_ascii_lowercase(), KEY)? + KEY.len();
    let rest = &block[at..];
    let value = match rest.first() {
        // Quoted: up to the quote before ';' (titles contain quotes:
        // "Guns N' Roses - Don't Cry").
        Some(&q @ (b'\'' | b'"')) => {
            let rest = &rest[1..];
            match find(rest, &[q, b';']) {
                Some(i) => &rest[..i],
                None => rest.strip_suffix(&[q]).unwrap_or(rest),
            }
        }
        _ => &rest[..find(rest, b";").unwrap_or(rest.len())],
    };
    let text = match std::str::from_utf8(value) {
        Ok(s) => s.to_string(),
        Err(_) => value.iter().map(|&b| b as char).collect(),
    };
    Some(
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string(),
    )
}

/// An Icecast/Shoutcast stream: reads the audio without the metadata
/// blocks, whose titles go to [`IcyStream::titles`].
pub struct IcyStream {
    body: Box<dyn Read + Send>,
    /// Audio bytes between metadata blocks (0: no metadata).
    metaint: usize,
    /// Audio bytes until the next metadata block.
    left: usize,
    titles: Titles,
    /// `icy-name`.
    pub name: Option<String>,
    /// `icy-genre`.
    pub genre: Option<String>,
    /// The Content-Type (lower case, without parameters).
    pub content_type: String,
    /// "mp3", "aac", "ogg", "opus", "flac" or "" (from the Content-Type).
    pub codec: String,
}

impl IcyStream {
    /// Connects (with `Icy-MetaData: 1`, following up to 5 redirects).
    pub fn open(url: &str) -> Result<IcyStream, String> {
        IcyStream::from_response(get(url, Reach::of(url), true)?)
    }

    /// A stream read from `body`, with a metadata block after every
    /// `metaint` audio bytes (0: none).
    pub fn new(body: Box<dyn Read + Send>, metaint: usize) -> IcyStream {
        IcyStream {
            body,
            metaint,
            left: metaint,
            titles: Titles::default(),
            name: None,
            genre: None,
            content_type: String::new(),
            codec: String::new(),
        }
    }

    fn from_response(r: Response) -> Result<IcyStream, String> {
        if !(200..300).contains(&r.status) {
            return Err(format!("HTTP status {}", r.status));
        }
        let metaint = match r.headers.get("icy-metaint") {
            None => 0,
            Some(v) => v
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=MAX_METAINT).contains(n))
                .ok_or("invalid icy-metaint")?,
        };
        let content_type = mime_of(&r.headers);
        let name = header_text(&r.headers, "icy-name");
        let genre = header_text(&r.headers, "icy-genre");
        let mut s = IcyStream::new(r.body, metaint);
        s.codec = codec_for_mime(&content_type).to_string();
        s.content_type = content_type;
        s.name = name;
        s.genre = genre;
        Ok(s)
    }

    /// The current `StreamTitle` (shared; updated while reading).
    pub fn titles(&self) -> Titles {
        self.titles.clone()
    }

    /// Reads a metadata block; false at the end of the stream.
    fn read_meta(&mut self) -> io::Result<bool> {
        let mut len = [0u8; 1];
        loop {
            match self.body.read(&mut len) {
                Ok(0) => return Ok(false),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let n = len[0] as usize * 16;
        if n > 0 {
            let mut block = vec![0u8; n];
            self.body.read_exact(&mut block)?;
            if let Some(t) = parse_stream_title(&block) {
                set_title(&self.titles, (!t.is_empty()).then_some(t));
            }
        }
        Ok(true)
    }
}

impl Read for IcyStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.metaint == 0 {
            return self.body.read(buf);
        }
        if self.left == 0 {
            if !self.read_meta()? {
                return Ok(0);
            }
            self.left = self.metaint;
        }
        let n = buf.len().min(self.left);
        let got = self.body.read(&mut buf[..n])?;
        self.left -= got;
        Ok(got)
    }
}

/// An open radio stream.
pub struct RadioStream {
    /// The audio: ICY metadata removed, HLS segments joined and demuxed.
    pub reader: Box<dyn Read + Send>,
    /// "mp3", "aac", "ogg", "opus", "flac" or "" when unknown.
    pub codec: String,
    /// The station's name (`icy-name` or its station file's title).
    pub name: Option<String>,
    /// The current title, updated while reading.
    pub titles: Titles,
}

/// Opens a station: resolves station files (trying their entries in
/// order), plays HLS playlists (by content) and Icecast/Shoutcast or
/// plain HTTP streams.
pub fn open_stream(url: &str) -> Result<RadioStream, String> {
    open_with(url, Reach::of(url), 0)
}

fn open_with(url: &str, reach: Reach, depth: usize) -> Result<RadioStream, String> {
    match fetch_station(url, reach)? {
        Fetched::Hls { url, text } => {
            let s = HlsStream::from_playlist(&url, &text, reach)?;
            Ok(RadioStream {
                codec: s.codec().to_string(),
                name: None,
                titles: s.titles(),
                reader: Box::new(s),
            })
        }
        Fetched::Audio(r) => {
            let s = IcyStream::from_response(r)?;
            let (name, titles, mut codec) = (s.name.clone(), s.titles(), s.codec.clone());
            let mut reader: Box<dyn Read + Send> = Box::new(s);
            if codec.is_empty() {
                let mut head = Vec::new();
                (&mut reader)
                    .take(4096)
                    .read_to_end(&mut head)
                    .map_err(|e| format!("receive: {e}"))?;
                codec = sniff_codec(&head).to_string();
                reader = Box::new(Cursor::new(head).chain(reader));
            }
            Ok(RadioStream {
                reader,
                codec,
                name,
                titles,
            })
        }
        Fetched::List(list) => {
            if depth >= 2 {
                return Err("station files nested too deeply".into());
            }
            let mut last = String::new();
            for s in list.iter().take(MAX_TRIES) {
                match open_with(&s.url, reach, depth + 1) {
                    Ok(mut r) => {
                        if r.name.is_none() {
                            r.name = s.title.clone();
                        }
                        return Ok(r);
                    }
                    Err(e) => last = e,
                }
            }
            Err(last)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ts::testgen;
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Route = dyn Fn(&str, &str, &str) -> Vec<u8> + Send + Sync;

    /// A local HTTP server: `route(base, path, request head)` gives the raw
    /// response. Returns the base URL.
    fn serve(route: impl Fn(&str, &str, &str) -> Vec<u8> + Send + Sync + 'static) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let route: Arc<Route> = Arc::new(route);
        let b = base.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { break };
                let (route, base) = (route.clone(), b.clone());
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if c.read(&mut byte).unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let _ = c.write_all(&route(&base, &path, &head));
                });
            }
        });
        base
    }

    fn ok(content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut r = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        r.extend_from_slice(body);
        r
    }

    fn status(code: u16, extra: &str) -> Vec<u8> {
        format!("HTTP/1.1 {code} X\r\n{extra}Content-Length: 0\r\n\r\n").into_bytes()
    }

    /// `audio` with a metadata block after every `metaint` bytes; the
    /// blocks carry `titles` in turn (None: an empty block).
    fn icy_body(audio: &[u8], metaint: usize, titles: &[Option<&str>]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, chunk) in audio.chunks(metaint).enumerate() {
            out.extend_from_slice(chunk);
            if chunk.len() < metaint {
                break;
            }
            match titles.get(i % titles.len().max(1)).copied().flatten() {
                Some(t) => {
                    let mut m = format!("StreamTitle='{t}';StreamUrl='';").into_bytes();
                    m.resize(m.len().div_ceil(16) * 16, 0);
                    out.push((m.len() / 16) as u8);
                    out.extend(m);
                }
                None => out.push(0),
            }
        }
        out
    }

    fn adts_frame(len: usize, fill: u8) -> Vec<u8> {
        // AAC-LC, 44.1 kHz, stereo, no CRC.
        let mut f = vec![0xff, 0xf1, 0x50, 0x80, 0, 0x1f, 0xfc];
        f[3] |= ((len >> 11) & 3) as u8;
        f[4] = ((len >> 3) & 0xff) as u8;
        f[5] = (((len & 7) << 5) as u8) | 0x1f;
        f.resize(len, fill);
        f
    }

    #[test]
    fn icy_metadata_is_stripped() {
        let audio: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let body = icy_body(&audio, 16, &[Some("A - One"), None, Some("B - Two")]);
        for chunk in [1, 5, 16, 17, 4096] {
            let mut s = IcyStream::new(Box::new(Cursor::new(body.clone())), 16);
            let titles = s.titles();
            let mut out = Vec::new();
            let mut buf = vec![0u8; chunk];
            let mut seen = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
                let t = titles.lock().unwrap().clone();
                if seen.last() != Some(&t) {
                    seen.push(t);
                }
            }
            assert_eq!(out, audio, "chunk {chunk}");
            assert_eq!(seen[1].as_deref(), Some("A - One"));
            assert!(seen.contains(&Some("B - Two".to_string())));
        }
        // Without metadata, reading passes through.
        let mut s = IcyStream::new(Box::new(Cursor::new(audio.clone())), 0);
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, audio);
        // A truncated metadata block is an error.
        let mut cut = audio[..16].to_vec();
        cut.extend_from_slice(&[2, b'S', b't']);
        let mut s = IcyStream::new(Box::new(Cursor::new(cut)), 16);
        let mut out = Vec::new();
        assert!(s.read_to_end(&mut out).is_err());
    }

    #[test]
    fn stream_titles() {
        let t = |b: &[u8]| parse_stream_title(b);
        assert_eq!(
            t(b"StreamTitle='Artist - Song';StreamUrl='http://x/';\0\0\0").as_deref(),
            Some("Artist - Song")
        );
        assert_eq!(
            t(b"StreamTitle='Guns N' Roses - Don't Cry';").as_deref(),
            Some("Guns N' Roses - Don't Cry")
        );
        assert_eq!(
            t(b"StreamTitle='Bj\xf6rk - J\xf3ga';").as_deref(),
            Some("Björk - Jóga")
        );
        assert_eq!(
            t("StreamTitle='Björk - Jóga';".as_bytes()).as_deref(),
            Some("Björk - Jóga")
        );
        assert_eq!(t(b"StreamTitle='';").as_deref(), Some(""));
        assert_eq!(t(b"streamtitle=\"x\";").as_deref(), Some("x"));
        assert_eq!(
            t(b"StreamTitle='unterminated").as_deref(),
            Some("unterminated")
        );
        assert_eq!(t(b"StreamTitle='a\nb';").as_deref(), Some("a b"));
        assert_eq!(t(b"StreamUrl='x';"), None);
        assert_eq!(t(b""), None);
    }

    #[test]
    fn helpers() {
        assert_eq!(codec_for_mime("audio/mpeg"), "mp3");
        assert_eq!(codec_for_mime("audio/aacp; charset=x"), "aac");
        assert_eq!(codec_for_mime("AUDIO/AAC"), "aac");
        assert_eq!(codec_for_mime("application/ogg"), "ogg");
        assert_eq!(codec_for_mime("audio/opus"), "opus");
        assert_eq!(codec_for_mime("audio/flac"), "flac");
        assert_eq!(codec_for_mime("text/plain"), "");

        let mut adts = adts_frame(200, 1);
        adts.extend(adts_frame(210, 2));
        assert_eq!(sniff_codec(&adts), "aac");
        assert_eq!(sniff_codec(b"\x00\x00\xff\xfb\x90\x64\x00\x00"), "mp3");
        assert_eq!(sniff_codec(b"OggS\0\x02"), "ogg");
        assert_eq!(sniff_codec(b"ID3\x04"), "mp3");
        assert_eq!(sniff_codec(b"<html>"), "");

        let b = "http://h:8000/a/b/list.pls?x=1";
        assert_eq!(join(b, "stream"), "http://h:8000/a/b/stream");
        assert_eq!(join(b, "../c/./d"), "http://h:8000/a/c/d");
        assert_eq!(join(b, "../../../.."), "http://h:8000/");
        assert_eq!(join(b, "/live?y=2"), "http://h:8000/live?y=2");
        assert_eq!(join(b, "?y=2"), "http://h:8000/a/b/list.pls?y=2");
        assert_eq!(join(b, "//cdn.example/s"), "http://cdn.example/s");
        assert_eq!(join(b, "https://o/s"), "https://o/s");
        assert_eq!(
            join(b, "s.ts?u=http://x/"),
            "http://h:8000/a/b/s.ts?u=http://x/"
        );
        assert_eq!(join("https://[::1]/x/", "y/.."), "https://[::1]/x/");

        assert_eq!(clean_url(" http://h/a b#frag ").unwrap(), "http://h/a%20b");
        assert_eq!(clean_url("http://h/ä").unwrap(), "http://h/%C3%A4");
        assert!(clean_url("http://h/a\r\nX: y").is_err());

        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2001:4860:4860::8888", "::ffff:1.1.1.1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn internet_stations_cannot_reach_the_local_network() {
        let base = serve(|_, _, _| ok("audio/mpeg", b"x"));
        let e = get(&base, Reach { local: false }, true).err().unwrap();
        assert!(e.contains("local network"), "{e}");
        assert!(get(&base, Reach::of(&base), true).is_ok());
    }

    /// A station: a .pls (first entry dead), a redirect to it, an M3U
    /// served as text/plain, a web page and an Icecast stream.
    fn station() -> (String, Vec<u8>) {
        let audio: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let body = icy_body(&audio, 100, &[Some("Artist - Title")]);
        let base = serve(move |base, path, head| {
            match path {
                "/station.pls" => ok(
                    "audio/x-scpls",
                    format!(
                        "[playlist]\nNumberOfEntries=2\nFile1={base}/dead\nTitle1=Dead\n\
                         File2={base}/stream\nTitle2=Test FM (backup)\nLength2=-1\nVersion=2\n"
                    )
                    .as_bytes(),
                ),
                "/go" => status(302, "Location: /station.pls\r\n"),
                "/loop" => status(302, "Location: /loop\r\n"),
                "/list" => ok(
                    "text/plain",
                    b"#EXTM3U\n#EXTINF:-1,Plain List\nstream\n/etc/passwd\n\
                      file:///etc/passwd\nC:\\x.mp3\n",
                ),
                "/page" => ok("text/html", b"<html></html>"),
                "/stream" => {
                    // Metadata only when asked for.
                    assert!(head.contains("Icy-MetaData: 1"));
                    let mut r = b"ICY 200 OK\r\nicy-name: Test FM\r\nicy-genre: Jazz\r\n\
                        content-type: audio/mpeg\r\nicy-metaint: 100\r\n\r\n"
                        .to_vec();
                    r.extend_from_slice(&body);
                    r
                }
                _ => status(404, ""),
            }
        });
        (base, audio)
    }

    #[test]
    fn station_files_and_icecast() {
        let (base, audio) = station();
        let list = resolve_station(&format!("{base}/go")).unwrap();
        assert_eq!(
            list,
            vec![
                StationStream {
                    url: format!("{base}/dead"),
                    title: Some("Dead".into()),
                    hls: false
                },
                StationStream {
                    url: format!("{base}/stream"),
                    title: Some("Test FM (backup)".into()),
                    hls: false
                },
            ]
        );
        // Sniffed from the content; local paths are left out.
        let list = resolve_station(&format!("{base}/list")).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].url, format!("{base}/stream"));
        assert_eq!(list[0].title.as_deref(), Some("Plain List"));
        assert_eq!(list[1].url, format!("{base}/etc/passwd"));
        // A stream is itself.
        let list = resolve_station(&format!("{base}/stream")).unwrap();
        assert_eq!(list[0].url, format!("{base}/stream"));
        assert_eq!(list[0].title.as_deref(), Some("Test FM"));
        assert!(resolve_station(&format!("{base}/page")).is_err());
        assert!(resolve_station(&format!("{base}/loop"))
            .unwrap_err()
            .contains("redirects"));
        assert!(resolve_station(&format!("{base}/missing")).is_err());

        // Through the station file (the dead entry is skipped).
        let mut s = open_stream(&format!("{base}/go")).unwrap();
        assert_eq!(s.codec, "mp3");
        assert_eq!(s.name.as_deref(), Some("Test FM"));
        let mut out = Vec::new();
        s.reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, audio);
        assert_eq!(s.titles.lock().unwrap().as_deref(), Some("Artist - Title"));

        let s = IcyStream::open(&format!("{base}/stream")).unwrap();
        assert_eq!(
            (
                s.name.as_deref(),
                s.genre.as_deref(),
                s.content_type.as_str(),
                s.codec.as_str()
            ),
            (Some("Test FM"), Some("Jazz"), "audio/mpeg", "mp3")
        );
    }

    /// An HLS server: master → media playlist with TS and packed AAC
    /// segments.
    #[test]
    fn hls_vod() {
        let frames: Vec<Vec<u8>> = (0..6).map(|i| adts_frame(300 + i * 10, i as u8)).collect();
        let f = frames.clone();
        let base = serve(move |_, path, _| match path {
            "/radio.m3u8" => ok(
                "application/vnd.apple.mpegurl",
                b"#EXTM3U\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\nlow/index.m3u8\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nhi/index.m3u8\n",
            ),
            "/hi/index.m3u8" => ok(
                "application/vnd.apple.mpegurl",
                b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:7\n\
                  #EXTINF:6,One - First\nseg7.ts\n#EXTINF:6,Two - Second\nseg8.ts\n\
                  #EXTINF:6,title=\"Third\",artist=\"Three\"\n/hi/seg9.aac\n#EXT-X-ENDLIST\n",
            ),
            "/hi/seg7.ts" => ok("video/mp2t", &testgen::stream(&f[0..2], 0x0f, false)),
            "/hi/seg8.ts" => ok("video/mp2t", &testgen::stream(&f[2..4], 0x0f, true)),
            "/hi/seg9.aac" => {
                let mut d = b"ID3\x04\x00\x00\x00\x00\x00\x03xyz".to_vec();
                d.extend(f[4..].concat());
                ok("audio/aac", &d)
            }
            _ => status(404, ""),
        });
        for url in [
            format!("{base}/radio.m3u8"),
            format!("{base}/hi/index.m3u8"),
        ] {
            let mut s = open_stream(&url).unwrap();
            assert_eq!(s.codec, "aac");
            let mut out = Vec::new();
            s.reader.read_to_end(&mut out).unwrap();
            assert_eq!(out, frames.concat());
            assert_eq!(s.titles.lock().unwrap().as_deref(), Some("Three - Third"));
        }
        assert_eq!(
            resolve_station(&format!("{base}/radio.m3u8")).unwrap()[0],
            StationStream {
                url: format!("{base}/radio.m3u8"),
                title: None,
                hls: true
            }
        );
        let s = HlsStream::open(&format!("{base}/hi/index.m3u8")).unwrap();
        assert_eq!(s.codec(), "aac");
    }

    /// A live playlist whose window moves on with every request; it ends
    /// after the third.
    #[test]
    fn hls_live() {
        let frames: Vec<Vec<u8>> = (0..5).map(|i| adts_frame(200, i as u8)).collect();
        let f = frames.clone();
        let loads = Arc::new(AtomicUsize::new(0));
        let l = loads.clone();
        let base = serve(move |_, path, _| {
            if path == "/live.m3u8" {
                let k = l.fetch_add(1, Ordering::SeqCst);
                let mut p =
                    format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{k}\n");
                for i in k..k + 3 {
                    p.push_str(&format!("#EXTINF:1,\ns{i}.ts\n"));
                }
                if k == 2 {
                    p.push_str("#EXT-X-ENDLIST\n");
                }
                return ok("application/vnd.apple.mpegurl", p.as_bytes());
            }
            let i: usize = path
                .trim_start_matches("/s")
                .trim_end_matches(".ts")
                .parse()
                .unwrap_or(99);
            match f.get(i) {
                Some(frame) => ok(
                    "video/mp2t",
                    &testgen::stream(std::slice::from_ref(frame), 0x0f, true),
                ),
                None => status(404, ""),
            }
        });
        let mut s = HlsStream::open(&format!("{base}/live.m3u8")).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, frames.concat());
        assert_eq!(loads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn hls_errors() {
        const M3U8: &str = "application/vnd.apple.mpegurl";
        let fetched = Arc::new(AtomicUsize::new(0));
        let counter = fetched.clone();
        let base = serve(move |_, path, _| {
            let playlist = |tags: &str, segments: &str| {
                let text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n{tags}{segments}");
                ok(M3U8, text.as_bytes())
            };
            match path {
                "/fmp4.m3u8" => playlist("#EXT-X-MAP:URI=\"init.mp4\"\n", "#EXTINF:6,\na.m4s\n"),
                "/aes.m3u8" => playlist("#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n", "a.ts\n"),
                "/video.m3u8" => playlist("#EXT-X-ENDLIST\n", "#EXTINF:6,\nv.ts\n"),
                "/v.ts" => ok("video/mp2t", &testgen::stream(&[vec![0; 100]], 0x1b, true)),
                "/nopmt.m3u8" => {
                    let segments: String = (0..50).map(|i| format!("j{i}.ts\n")).collect();
                    playlist("#EXT-X-ENDLIST\n", &segments)
                }
                p if p.starts_with("/j") => {
                    // Packets of some stream, but no PAT or PMT.
                    counter.fetch_add(1, Ordering::SeqCst);
                    ok("video/mp2t", &testgen::packet(0x300, true, 0, &[1; 184]))
                }
                _ => status(404, ""),
            }
        });
        let e = |p: &str| open_stream(&format!("{base}{p}")).err().unwrap();
        assert!(e("/fmp4.m3u8").contains("fMP4"));
        assert!(e("/aes.m3u8").contains("encrypted"));
        assert!(e("/video.m3u8").contains("no AAC or MPEG audio"));
        assert!(e("/nopmt.m3u8").contains("no AAC or MPEG audio"));
        let n = fetched.load(Ordering::SeqCst);
        assert!(n <= 10, "{n} segments fetched");
    }

    // Decoding real streams (skipped without ffmpeg).

    fn ffmpeg(args: &str) -> Option<Vec<u8>> {
        let out = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
            .args(args.split_whitespace())
            .arg("pipe:1")
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            eprintln!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
            return None;
        }
        Some(out.stdout)
    }

    /// Decodes everything; returns (sample rate, channels, frames, RMS).
    fn decode_all(stream: RadioStream) -> (u32, u16, usize, f32) {
        let mut dec = open_decoder(stream).unwrap();
        let spec = dec.spec();
        let mut pcm = Vec::new();
        while dec.next_chunk(&mut pcm).unwrap() {}
        let rms = (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        (
            spec.sample_rate,
            spec.channels,
            pcm.len() / spec.channels as usize,
            rms,
        )
    }

    fn radio(data: Vec<u8>, codec: &str) -> RadioStream {
        RadioStream {
            reader: Box::new(Cursor::new(data)),
            codec: codec.into(),
            name: None,
            titles: Titles::default(),
        }
    }

    const SINE: &str = "-i sine=frequency=440:duration=3:sample_rate=44100 -ac 2";
    /// The sine's amplitude is 1/8, -3 dB per channel after the upmix:
    /// an RMS of about 0.0625.
    const SINE_RMS: std::ops::Range<f32> = 0.055..0.07;

    #[test]
    fn decodes_mp3_icecast_stream() {
        let Some(mp3) = ffmpeg(&format!("{SINE} -c:a libmp3lame -b:a 128k -f mp3")) else {
            return;
        };
        let body = icy_body(&mp3, 8192, &[Some("Sine - 440")]);
        let base = serve(move |_, _, _| {
            let mut r = b"HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-metaint: 8192\r\n\r\n"
                .to_vec();
            r.extend_from_slice(&body);
            r
        });
        let s = open_stream(&format!("{base}/live")).unwrap();
        let titles = s.titles.clone();
        let (rate, ch, frames, rms) = decode_all(s);
        assert_eq!((rate, ch), (44100, 2));
        assert!((120_000..140_000).contains(&frames), "{frames}");
        assert!(SINE_RMS.contains(&rms), "{rms}");
        assert_eq!(titles.lock().unwrap().as_deref(), Some("Sine - 440"));
        // Without a codec hint the content tells.
        let (_, _, frames, _) = decode_all(radio(mp3, ""));
        assert!(frames > 120_000);
    }

    #[test]
    fn decodes_aac_and_mp3_in_ts() {
        for (args, codec) in [
            ("-c:a aac -b:a 96k", "aac"),
            ("-c:a libmp3lame -b:a 128k", "mp3"),
        ] {
            let Some(ts) = ffmpeg(&format!("{SINE} {args} -f mpegts")) else {
                return;
            };
            let mut demux = TsDemuxer::new();
            let mut es = Vec::new();
            for chunk in ts.chunks(1000) {
                demux.push(chunk, &mut es);
            }
            demux.flush(&mut es);
            assert_eq!(demux.codec(), Some(codec));
            assert_eq!(sniff_codec(&es), codec);
            let (rate, ch, frames, rms) = decode_all(radio(es, codec));
            assert_eq!((rate, ch), (44100, 2), "{codec}");
            assert!((125_000..140_000).contains(&frames), "{codec}: {frames}");
            assert!(SINE_RMS.contains(&rms), "{codec}: {rms}");
        }
    }

    #[test]
    fn decodes_chained_ogg() {
        let ogg = |codec: &str, title: &str| {
            ffmpeg(&format!(
                "-i sine=frequency=440:duration=1:sample_rate=48000 -ac 2 -c:a {codec} \
                 -metadata title={title} -metadata artist=Band -f ogg"
            ))
        };
        let (Some(a), Some(b)) = (ogg("libvorbis", "One"), ogg("libvorbis", "Two")) else {
            return;
        };
        let titles = Titles::default();
        let s = RadioStream {
            reader: Box::new(Cursor::new([a, b].concat())),
            codec: "ogg".into(),
            name: None,
            titles: titles.clone(),
        };
        let (rate, ch, frames, rms) = decode_all(s);
        assert_eq!((rate, ch), (48000, 2));
        // Both chains (1 s each).
        assert!((92_000..100_000).contains(&frames), "{frames}");
        assert!(SINE_RMS.contains(&rms), "{rms}");
        assert_eq!(titles.lock().unwrap().as_deref(), Some("Band - Two"));

        let Some(opus) = ogg("libopus", "Opus") else {
            return;
        };
        let (rate, _, frames, _) = decode_all(radio(opus, ""));
        assert_eq!(rate, 48000);
        assert!((44_000..50_000).contains(&frames), "{frames}");
    }

    #[test]
    fn live_source_seeks() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
        let mut s = LiveSource::new(Box::new(Cursor::new(data.clone())));
        use std::io::{Seek, SeekFrom};
        assert!(s.seek(SeekFrom::End(0)).is_err());
        let mut b = [0u8; 100];
        s.read_exact(&mut b).unwrap();
        s.seek(SeekFrom::Start(10)).unwrap();
        s.read_exact(&mut b).unwrap();
        assert_eq!(b[0], 10);
        // Forward skips.
        assert_eq!(s.seek(SeekFrom::Current(1000)).unwrap(), 1110);
        s.read_exact(&mut b).unwrap();
        assert_eq!(b[0], data[1110]);
        // Past the kept start, going back fails.
        s.seek(SeekFrom::Start(2_000_000)).unwrap();
        assert!(s.seek(SeekFrom::Start(5)).is_err());
        s.read_exact(&mut b).unwrap();
        assert_eq!(b[0], data[2_000_000]);
        assert!(s.seek(SeekFrom::Current(-200)).is_err());
    }
}
